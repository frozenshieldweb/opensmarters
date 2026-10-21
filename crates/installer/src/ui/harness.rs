// SPDX-License-Identifier: GPL-3.0-or-later
//! Dev builds only (`dev` feature): scripted keys and screenshots, the same pattern as the
//! app's harness (`crates/app/src/harness.rs`). `--keys "wait:1,Tab,Enter,shot:a.png,quit"`.

use eframe::egui;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use eframe::egui::{Key, Modifiers, RawInput};

/// One scripted step.
#[derive(Debug, Clone, PartialEq)]
pub enum Step {
    /// Wait this many seconds.
    Wait(f64),
    /// Press and release a key.
    Key(Key, Modifiers),
    /// Save a screenshot.
    Shot(PathBuf),
    /// Close the window.
    Quit,
}

/// Parses a `--keys` script.
///
/// # Errors
///
/// An unknown key or a malformed `wait:`.
pub fn parse_script(script: &str) -> Result<Vec<Step>, String> {
    let mut steps = Vec::new();
    for raw in script.split(',') {
        let tok = raw.trim();
        if tok.is_empty() {
            continue;
        }
        if let Some(secs) = tok.strip_prefix("wait:") {
            let s: f64 = secs
                .trim()
                .parse()
                .map_err(|_| format!("bad wait {tok:?}"))?;
            steps.push(Step::Wait(s.clamp(0.0, 600.0)));
        } else if let Some(p) = tok.strip_prefix("shot:") {
            steps.push(Step::Shot(PathBuf::from(p.trim())));
        } else if tok.eq_ignore_ascii_case("quit") {
            steps.push(Step::Quit);
        } else {
            let (mods, name) = match tok.rsplit_once('+') {
                Some((m, k)) if !k.is_empty() && m.eq_ignore_ascii_case("shift") => {
                    (Modifiers::SHIFT, k)
                }
                _ => (Modifiers::NONE, tok),
            };
            let key = match name.to_ascii_lowercase().as_str() {
                "esc" => Some(Key::Escape),
                "space" => Some(Key::Space),
                _ => Key::from_name(name),
            }
            .ok_or_else(|| format!("unknown key {name:?}"))?;
            steps.push(Step::Key(key, mods));
        }
    }
    Ok(steps)
}

/// Runs a script against the window's input and takes screenshots.
#[derive(Debug, Default)]
pub struct Harness {
    steps: VecDeque<Step>,
    started: Option<Instant>,
    next_at: f64,
    final_shot: Option<(f64, PathBuf)>,
    exit_after: Option<f64>,
    pending: Arc<AtomicUsize>,
    closing: bool,
}

impl Harness {
    /// A harness for `steps`, an optional final screenshot and an exit time.
    pub fn new(
        steps: Vec<Step>,
        final_shot: Option<(f64, PathBuf)>,
        exit_after: Option<f64>,
    ) -> Self {
        Self {
            steps: steps.into(),
            final_shot,
            exit_after,
            ..Self::default()
        }
    }

    /// Injects due key events (from `raw_input_hook`). Real input is dropped so runs are
    /// deterministic. Screenshots and closing happen in [`Harness::during_pass`], because
    /// viewport commands sent before the pass starts can be lost.
    pub fn inject(&mut self, raw: &mut RawInput) {
        if self.closing {
            return;
        }
        raw.events.clear();
        // Harness windows start in the background (Windows' foreground lock), and egui hides
        // keyboard focus in an inactive window. Screenshots show what a user sees.
        raw.focused = true;
        let now = self
            .started
            .get_or_insert_with(Instant::now)
            .elapsed()
            .as_secs_f64();
        if now < self.next_at {
            return;
        }
        if let Some(Step::Key(key, modifiers)) = self.steps.front().cloned() {
            self.steps.pop_front();
            for pressed in [true, false] {
                raw.events.push(egui::Event::Key {
                    key,
                    physical_key: None,
                    pressed,
                    repeat: false,
                    modifiers,
                });
            }
            self.next_at = now + 0.3;
        }
    }

    /// Runs due waits, screenshots and the final close (call from `App::ui`).
    pub fn during_pass(&mut self, ctx: &egui::Context) {
        if self.closing {
            return;
        }
        let now = self
            .started
            .get_or_insert_with(Instant::now)
            .elapsed()
            .as_secs_f64();
        while now >= self.next_at {
            match self.steps.front().cloned() {
                None | Some(Step::Key(..)) => break,
                Some(Step::Wait(s)) => {
                    self.steps.pop_front();
                    self.next_at = now + s;
                }
                Some(Step::Shot(path)) => {
                    self.steps.pop_front();
                    shot(ctx, path, &self.pending);
                    self.next_at = now + 0.4;
                }
                Some(Step::Quit) => {
                    if self.pending.load(Ordering::Acquire) == 0 {
                        self.steps.pop_front();
                        self.closing = true;
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    } else {
                        self.next_at = now + 0.05;
                    }
                    break;
                }
            }
        }
        if let Some((at, _)) = &self.final_shot
            && now >= *at
            && let Some((_, path)) = self.final_shot.take()
        {
            shot(ctx, path, &self.pending);
            self.steps.clear();
            self.steps.push_back(Step::Quit);
            self.next_at = now + 0.4;
        }
        if self.exit_after.is_some_and(|t| now >= t) {
            self.exit_after = None;
            self.steps.clear();
            self.steps.push_back(Step::Quit);
        }
        ctx.request_repaint_after(std::time::Duration::from_millis(30));
    }
}

fn shot(ctx: &egui::Context, path: PathBuf, pending: &Arc<AtomicUsize>) {
    pending.fetch_add(1, Ordering::AcqRel);
    let pending = Arc::clone(pending);
    ctx.request_screenshot(move |img: Arc<egui::ColorImage>| {
        std::thread::spawn(move || {
            if let Err(e) = save_png(&path, &img) {
                tracing::warn!(error = %e, "screenshot failed");
            } else {
                tracing::info!(path = %path.display(), w = img.size[0], h = img.size[1], "screenshot saved");
            }
            pending.fetch_sub(1, Ordering::AcqRel);
        });
    });
    ctx.request_repaint();
}

fn save_png(path: &std::path::Path, img: &egui::ColorImage) -> anyhow::Result<()> {
    if let Some(dir) = path.parent()
        && !dir.as_os_str().is_empty()
    {
        std::fs::create_dir_all(dir)?;
    }
    let mut bytes = Vec::with_capacity(img.pixels.len() * 4);
    for p in &img.pixels {
        bytes.extend_from_slice(&[p.r(), p.g(), p.b(), 255]);
    }
    let buf = image::RgbaImage::from_raw(
        u32::try_from(img.size[0])?,
        u32::try_from(img.size[1])?,
        bytes,
    )
    .ok_or_else(|| anyhow::anyhow!("bad screenshot size"))?;
    buf.save(path)?;
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn parses_scripts() {
        let s = parse_script("wait:1,Tab,Shift+Tab,Enter,Esc,space,shot:a.png,quit").unwrap();
        assert_eq!(s[0], Step::Wait(1.0));
        assert_eq!(s[1], Step::Key(Key::Tab, Modifiers::NONE));
        assert_eq!(s[2], Step::Key(Key::Tab, Modifiers::SHIFT));
        assert_eq!(s[4], Step::Key(Key::Escape, Modifiers::NONE));
        assert_eq!(s[6], Step::Shot(PathBuf::from("a.png")));
        assert!(parse_script("Nope").is_err());
    }
}
