// SPDX-License-Identifier: GPL-3.0-or-later
//! Phase-0 playback spike: plays one URL or local file inside an eframe
//! (glow) window with a small egui control overlay.
//!
//! ```text
//! cargo run -p opensmarters-player --example spike -- <url-or-file> [options]
//!
//!   --no-audio                    ao=null (silent; for automated runs)
//!   --screenshot-after <secs>     take a screenshot after <secs> …
//!   --screenshot-out <png>        … save it here, then exit
//!   --at <secs>:<action>          scripted action (repeatable):
//!         seek=<abs> | seekrel=<s> | pause | play | aid=<id> | sid=<id|no>
//!         fit=next | volume=<0-130> | stop | shot=<png> | quit
//!         size=<w>x<h> (resize the window, in points)
//!   --status-every <secs>         print a state line to stdout (default 1)
//!   --allow-streaming             also accept rtmp/rtsp/udp URLs
//!   --mpv-log <level>             print mpv log lines up to this level
//!                                 (fatal|error|warn|info|v|debug; default warn)
//! ```
//!
//! mpv log lines are printed with URLs redacted (warn and above).

#![allow(clippy::print_stdout)]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context as _, bail};
use eframe::egui;
use opensmarters_player::{
    EndReason, LoadOptions, LogLevel, PlayableUrl, Player, PlayerConfig, PlayerEvent, PlayerState,
    TrackKind, TrackSelection, UrlPolicy, UserAgent, VideoFit, VideoView,
};

#[derive(Debug, Clone)]
enum Action {
    Seek(f64),
    SeekRel(f64),
    Pause,
    Play,
    Aid(i64),
    Sid(Option<i64>),
    FitNext,
    Volume(f64),
    Stop,
    Size(f32, f32),
    Shot(PathBuf),
    ShotAndQuit(PathBuf),
    Quit,
}

#[derive(Debug)]
struct Args {
    input: String,
    no_audio: bool,
    allow_streaming: bool,
    status_every: f64,
    mpv_log: LogLevel,
    actions: Vec<(f64, Action)>,
}

fn parse_action(s: &str) -> anyhow::Result<Action> {
    let (k, v) = s.split_once('=').unwrap_or((s, ""));
    let num = || {
        v.parse::<f64>()
            .with_context(|| format!("bad number in {s:?}"))
    };
    Ok(match k {
        "seek" => Action::Seek(num()?),
        "seekrel" => Action::SeekRel(num()?),
        "pause" => Action::Pause,
        "play" => Action::Play,
        "aid" => Action::Aid(v.parse().context("aid")?),
        "sid" if v == "no" => Action::Sid(None),
        "sid" => Action::Sid(Some(v.parse().context("sid")?)),
        "fit" => Action::FitNext,
        "volume" => Action::Volume(num()?),
        "stop" => Action::Stop,
        "size" => {
            let (w, h) = v.split_once('x').context("size=<w>x<h>")?;
            Action::Size(w.parse()?, h.parse()?)
        }
        "shot" => Action::Shot(PathBuf::from(v)),
        "quit" => Action::Quit,
        _ => bail!("unknown action {s:?}"),
    })
}

fn parse_args() -> anyhow::Result<Args> {
    let mut it = std::env::args().skip(1);
    let mut input = None;
    let mut no_audio = false;
    let mut allow_streaming = false;
    let mut status_every = 1.0;
    let mut mpv_log = LogLevel::Warn;
    let mut actions = Vec::new();
    let mut shot_after = None;
    let mut shot_out = None;
    while let Some(a) = it.next() {
        let mut val = |name: &str| it.next().with_context(|| format!("{name} needs a value"));
        match a.as_str() {
            "--no-audio" => no_audio = true,
            "--allow-streaming" => allow_streaming = true,
            "--status-every" => status_every = val("--status-every")?.parse()?,
            "--mpv-log" => {
                mpv_log =
                    LogLevel::from_mpv_str(&val("--mpv-log")?).context("bad --mpv-log level")?;
            }
            "--screenshot-after" => shot_after = Some(val("--screenshot-after")?.parse::<f64>()?),
            "--screenshot-out" => shot_out = Some(PathBuf::from(val("--screenshot-out")?)),
            "--at" => {
                let spec = val("--at")?;
                let (t, act) = spec.split_once(':').context("--at needs <secs>:<action>")?;
                actions.push((t.parse::<f64>()?, parse_action(act)?));
            }
            s if s.starts_with("--") => bail!("unknown flag {s}"),
            s => input = Some(s.to_owned()),
        }
    }
    match (shot_after, shot_out) {
        (Some(t), Some(p)) => actions.push((t, Action::ShotAndQuit(p))),
        (None, None) => {}
        _ => bail!("--screenshot-after and --screenshot-out go together"),
    }
    actions.sort_by(|a, b| a.0.total_cmp(&b.0));
    Ok(Args {
        input: input.context("usage: spike <url-or-file> [options]")?,
        no_audio,
        allow_streaming,
        status_every,
        mpv_log,
        actions,
    })
}

/// Very small stand-in for the app's redaction layer: hide anything that
/// looks like a URL or an absolute path.
fn redact(text: &str) -> String {
    text.split(' ')
        .map(|w| {
            if w.contains("://") || w.contains(":\\") || w.contains(":/") {
                "<redacted>"
            } else {
                w
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn fmt_time(t: Option<f64>) -> String {
    match t {
        Some(t) if t.is_finite() && t >= 0.0 => {
            // Truncation to whole seconds is intended.
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let s = t as u64;
            format!("{:02}:{:02}", s / 60, s % 60)
        }
        _ => "--:--".to_owned(),
    }
}

fn status_line(st: &PlayerState, fit: VideoFit, view: &VideoView) -> String {
    let sel = |k| st.selected(k).map_or("-".to_owned(), |t| t.id.to_string());
    let fit_name = fit
        .preset_index()
        .map_or("custom", |i| VideoFit::PRESETS[i].0);
    let stats = view.stats();
    format!(
        "[state] t={:.2} dur={} paused={} idle={} loaded={} buffering={} seekable={} vid={} aid={} sid={} \
         tracks(v={},a={},s={}) vsize={:?} fit={} (aspect_override={:.3} keepaspect={} panscan={}) \
         hwdec={} frames(rendered={},presented={}) fbo={:?} end={:?}",
        st.time_pos.unwrap_or(-1.0),
        st.duration.map_or("-".into(), |d| format!("{d:.2}")),
        st.paused,
        st.idle,
        st.file_loaded,
        st.is_buffering(),
        st.seekable,
        sel(TrackKind::Video),
        sel(TrackKind::Audio),
        sel(TrackKind::Subtitle),
        st.tracks
            .iter()
            .filter(|t| t.kind == TrackKind::Video)
            .count(),
        st.audio_tracks().count(),
        st.subtitle_tracks().count(),
        st.video_size(),
        fit_name,
        st.video_aspect_override,
        st.keepaspect,
        st.panscan,
        st.hwdec_current.as_deref().unwrap_or("no"),
        stats.frames_rendered,
        stats.frames_presented,
        stats.target_size,
        st.last_end,
    )
}

struct Spike {
    player: Player,
    video: VideoView,
    started: Instant,
    actions: Vec<(f64, Action)>,
    status_every: f64,
    mpv_log: LogLevel,
    last_status: f64,
    seek_drag: Option<f64>,
    show_overlay: bool,
    quitting: bool,
}

impl Spike {
    fn run_action(&mut self, ctx: &egui::Context, a: Action) {
        println!("[action] {a:?}");
        let r = match a {
            Action::Seek(t) => self.player.seek_absolute(t),
            Action::SeekRel(t) => self.player.seek_relative(t),
            Action::Pause => self.player.pause(),
            Action::Play => self.player.play(),
            Action::Aid(id) => self.player.select_audio(TrackSelection::Id(id)),
            Action::Sid(Some(id)) => self.player.select_subtitle(TrackSelection::Id(id)),
            Action::Sid(None) => self.player.select_subtitle(TrackSelection::Off),
            Action::FitNext => self
                .player
                .set_video_fit(self.player.video_fit().next_preset()),
            Action::Volume(v) => self.player.set_volume(v),
            Action::Stop => self.player.stop(),
            Action::Size(w, h) => {
                ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(egui::vec2(w, h)));
                Ok(())
            }
            Action::Shot(p) => {
                request_shot(ctx, p, false);
                Ok(())
            }
            Action::ShotAndQuit(p) => {
                self.quitting = true;
                request_shot(ctx, p, true);
                Ok(())
            }
            Action::Quit => {
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                Ok(())
            }
        };
        if let Err(e) = r {
            println!("[action] failed: {e}");
        }
    }

    fn overlay(&mut self, ctx: &egui::Context, rect: egui::Rect, st: &PlayerState) {
        // Top-left status.
        egui::Area::new(egui::Id::unique("spike-status"))
            .order(egui::Order::Foreground)
            .fixed_pos(rect.left_top() + egui::vec2(10.0, 10.0))
            .show(ctx, |ui| {
                egui::Frame::new()
                    .fill(egui::Color32::from_black_alpha(160))
                    .inner_margin(6.0)
                    .corner_radius(4.0)
                    .show(ui, |ui| {
                        let title = st.media_title.as_deref().unwrap_or("(no title)");
                        ui.colored_label(
                            egui::Color32::WHITE,
                            format!("opensmarters spike · {title}"),
                        );
                        let mut line = String::new();
                        if st.is_buffering() {
                            line.push_str("Buffering… ");
                            if let Some(p) = st.cache_buffering_percent {
                                line.push_str(&format!("{p}% "));
                            }
                        }
                        if let Some((w, h)) = st.video_size() {
                            line.push_str(&format!("{w}×{h} "));
                        }
                        let fit = self.player.video_fit();
                        line.push_str(&format!(
                            "fit: {}",
                            fit.preset_index()
                                .map_or("custom", |i| VideoFit::PRESETS[i].0)
                        ));
                        if let Some(EndReason::Error(e)) = &st.last_end {
                            line = format!("Error: {e}");
                        } else if st.last_end == Some(EndReason::Eof) {
                            line = "Finished".into();
                        }
                        ui.colored_label(egui::Color32::LIGHT_GRAY, line);
                    });
            });

        // Bottom control bar.
        let bar_h = 64.0;
        let bar_rect = egui::Rect::from_min_max(
            egui::pos2(rect.left() + 8.0, rect.bottom() - bar_h - 8.0),
            egui::pos2(rect.right() - 8.0, rect.bottom() - 8.0),
        );
        egui::Area::new(egui::Id::unique("spike-controls"))
            .order(egui::Order::Foreground)
            .fixed_pos(bar_rect.left_top())
            .show(ctx, |ui| {
                ui.set_width(bar_rect.width());
                egui::Frame::new()
                    .fill(egui::Color32::from_black_alpha(170))
                    .inner_margin(8.0)
                    .corner_radius(6.0)
                    .show(ui, |ui| {
                        ui.style_mut().visuals.override_text_color = Some(egui::Color32::WHITE);
                        // Seek bar row.
                        ui.horizontal(|ui| {
                            ui.label(fmt_time(st.time_pos));
                            let dur = st.duration.unwrap_or(0.0).max(0.0);
                            let mut pos = self.seek_drag.unwrap_or(st.time_pos.unwrap_or(0.0));
                            let w = (ui.available_width() - 60.0).max(50.0);
                            ui.spacing_mut().slider_width = w;
                            let resp = ui.add_enabled(
                                st.seekable && dur > 0.0,
                                egui::Slider::new(&mut pos, 0.0..=dur.max(0.001)).show_value(false),
                            );
                            if resp.dragged() {
                                self.seek_drag = Some(pos);
                            }
                            if resp.drag_stopped() || (resp.changed() && !resp.dragged()) {
                                let _ = self.player.seek_absolute(pos);
                                self.seek_drag = None;
                            }
                            ui.label(fmt_time(st.duration));
                        });
                        // Buttons row.
                        ui.horizontal(|ui| {
                            let label = if st.paused { "Play" } else { "Pause" };
                            if ui.button(label).clicked() {
                                let _ = self.player.toggle_pause();
                            }
                            if ui.button("-10s").clicked() {
                                let _ = self.player.seek_relative(-10.0);
                            }
                            if ui.button("+10s").clicked() {
                                let _ = self.player.seek_relative(10.0);
                            }
                            ui.separator();
                            let mut vol = st.volume;
                            ui.spacing_mut().slider_width = 90.0;
                            if ui
                                .add(
                                    egui::Slider::new(&mut vol, 0.0..=130.0)
                                        .text("vol")
                                        .show_value(false),
                                )
                                .changed()
                            {
                                let _ = self.player.set_volume(vol);
                            }
                            if ui
                                .button(if st.muted { "Unmute" } else { "Mute" })
                                .clicked()
                            {
                                let _ = self.player.toggle_mute();
                            }
                            ui.separator();
                            let cur_a = st.selected(TrackKind::Audio).map(|t| t.id);
                            egui::ComboBox::from_id_salt("audio")
                                .selected_text(format!(
                                    "Audio {}",
                                    cur_a.map_or("off".into(), |i| i.to_string())
                                ))
                                .show_ui(ui, |ui| {
                                    for t in st.audio_tracks() {
                                        if ui.selectable_label(t.selected, t.label()).clicked() {
                                            let _ =
                                                self.player.select_audio(TrackSelection::Id(t.id));
                                        }
                                    }
                                });
                            let cur_s = st.selected(TrackKind::Subtitle).map(|t| t.id);
                            egui::ComboBox::from_id_salt("subs")
                                .selected_text(format!(
                                    "Subs {}",
                                    cur_s.map_or("off".into(), |i| i.to_string())
                                ))
                                .show_ui(ui, |ui| {
                                    if ui.selectable_label(cur_s.is_none(), "Off").clicked() {
                                        let _ = self.player.select_subtitle(TrackSelection::Off);
                                    }
                                    for t in st.subtitle_tracks() {
                                        if ui.selectable_label(t.selected, t.label()).clicked() {
                                            let _ = self
                                                .player
                                                .select_subtitle(TrackSelection::Id(t.id));
                                        }
                                    }
                                });
                            let fit = self.player.video_fit();
                            let name = fit
                                .preset_index()
                                .map_or("custom", |i| VideoFit::PRESETS[i].0);
                            if ui.button(format!("Aspect: {name}")).clicked() {
                                let _ = self.player.set_video_fit(fit.next_preset());
                            }
                        });
                    });
            });
    }
}

fn request_shot(ctx: &egui::Context, path: PathBuf, quit: bool) {
    let ctx2 = ctx.clone();
    ctx.request_screenshot(move |img| {
        match save_png(&path, &img) {
            Ok(()) => println!(
                "[screenshot] saved {} ({}x{})",
                path.display(),
                img.size[0],
                img.size[1]
            ),
            Err(e) => println!("[screenshot] failed: {e:#}"),
        }
        if quit {
            ctx2.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        ctx2.request_repaint();
    });
}

fn save_png(path: &Path, img: &egui::ColorImage) -> anyhow::Result<()> {
    let mut bytes = Vec::with_capacity(img.pixels.len() * 4);
    for p in &img.pixels {
        bytes.extend_from_slice(&[p.r(), p.g(), p.b(), 255]);
    }
    let w = u32::try_from(img.size[0])?;
    let h = u32::try_from(img.size[1])?;
    let buf = image::RgbaImage::from_raw(w, h, bytes).context("bad screenshot size")?;
    buf.save(path)?;
    Ok(())
}

impl eframe::App for Spike {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        for ev in self.player.poll_events() {
            match ev {
                PlayerEvent::Log(l) if l.level <= self.mpv_log => {
                    println!("[mpv {:?}] {}: {}", l.level, l.prefix, redact(&l.text));
                }
                PlayerEvent::Log(_) | PlayerEvent::PropertyChanged(_) => {}
                other => println!("[event] {other:?}"),
            }
        }

        let elapsed = self.started.elapsed().as_secs_f64();
        while self.actions.first().is_some_and(|(t, _)| *t <= elapsed) && !self.quitting {
            let (_, a) = self.actions.remove(0);
            self.run_action(&ctx, a);
        }

        let st = self.player.state();
        if elapsed - self.last_status >= self.status_every {
            self.last_status = elapsed;
            println!(
                "[{elapsed:6.2}s] {}",
                status_line(&st, self.player.video_fit(), &self.video)
            );
            if let Some(e) = self.video.last_error() {
                println!("[video] error: {e}");
            }
        }

        let resp = self.video.show(ui, &self.player);
        if resp.clicked() {
            self.show_overlay = !self.show_overlay;
        }
        if resp.double_clicked() {
            let fs = ctx.input(|i| i.viewport().fullscreen.unwrap_or(false));
            ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(!fs));
        }
        if self.show_overlay {
            self.overlay(&ctx, resp.rect, &st);
        }
        ctx.input(|i| {
            if i.key_pressed(egui::Key::Space) {
                let _ = self.player.toggle_pause();
            }
            if i.key_pressed(egui::Key::ArrowRight) {
                let _ = self.player.seek_relative(10.0);
            }
            if i.key_pressed(egui::Key::ArrowLeft) {
                let _ = self.player.seek_relative(-10.0);
            }
            if i.key_pressed(egui::Key::A) {
                let _ = self
                    .player
                    .set_video_fit(self.player.video_fit().next_preset());
            }
        });
        // Keep the action clock ticking while paused/idle.
        ctx.request_repaint_after(Duration::from_millis(100));
    }

    fn on_exit(&mut self, gl: Option<&eframe::glow::Context>) {
        println!(
            "[exit] {}",
            status_line(&self.player.state(), self.player.video_fit(), &self.video)
        );
        if let Some(gl) = gl {
            self.video.destroy(gl);
        }
    }
}

fn main() -> anyhow::Result<()> {
    let args = parse_args()?;
    let policy = if args.allow_streaming {
        UrlPolicy::ALL_STREAMING
    } else {
        UrlPolicy::HTTP_ONLY
    };
    let (url, local) = match PlayableUrl::parse_with_policy(&args.input, policy) {
        Ok(u) => (u, false),
        Err(url_err) => {
            let p = Path::new(&args.input);
            if p.is_file() {
                let abs = std::fs::canonicalize(p)?;
                let u = PlayableUrl::from_local_file(&abs)
                    .map_err(|e| anyhow::anyhow!("local file rejected: {e}"))?;
                (u, true)
            } else {
                bail!("input rejected before reaching mpv: {url_err}");
            }
        }
    };
    println!("[input] {url:?} (local file: {local})");

    let config = PlayerConfig::default()
        .url_policy(policy)
        .allow_local_files(local)
        .audio_output(!args.no_audio)
        .log_level(args.mpv_log.max(LogLevel::Info))
        .user_agent(UserAgent::new(concat!(
            "opensmarters/",
            env!("CARGO_PKG_VERSION")
        ))?);
    let player = Player::new(config).map_err(|e| anyhow::anyhow!("{e}"))?;
    println!(
        "[mpv] {} | ffmpeg {} | client API {:?} | {}",
        player.mpv_version(),
        player.ffmpeg_version(),
        player.client_api_version(),
        player.libmpv_path().display()
    );
    if !player.skipped_options().is_empty() {
        println!(
            "[mpv] best-effort options not applied: {:?}",
            player.skipped_options()
        );
    }

    let options = eframe::NativeOptions {
        renderer: eframe::Renderer::Glow,
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([960.0, 540.0])
            .with_title("opensmarters spike"),
        ..Default::default()
    };
    let actions = args.actions;
    let status_every = args.status_every;
    let mpv_log = args.mpv_log;
    eframe::run_native(
        "opensmarters-spike",
        options,
        Box::new(move |cc| {
            let ctx = cc.egui_ctx.clone();
            player.set_event_notify(move || ctx.request_repaint());
            let gpa = cc
                .get_proc_address
                .clone()
                .ok_or("the glow renderer did not provide get_proc_address")?;
            player.load(&url, &LoadOptions::default())?;
            Ok(Box::new(Spike {
                player,
                video: VideoView::new(Arc::clone(&gpa)),
                started: Instant::now(),
                actions,
                status_every,
                mpv_log,
                last_status: -1.0e9,
                seek_drag: None,
                show_overlay: true,
                quitting: false,
            }))
        }),
    )
    .map_err(|e| anyhow::anyhow!("eframe: {e}"))?;
    Ok(())
}
