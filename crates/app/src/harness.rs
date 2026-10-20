// SPDX-License-Identifier: GPL-3.0-or-later
//! The dev/test harness: scripted key presses (`--keys`), screenshots
//! (`--screenshot-after/--screenshot-out`, `shot:<png>` steps) and timed exit. Reviewers
//! and agents use it to drive the real UI keyboard-only and look at the result.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use eframe::egui::{self, Key, Modifiers, RawInput};

/// One scripted step.
#[derive(Debug, Clone, PartialEq)]
pub enum Step {
    /// Wait this many seconds.
    Wait(f64),
    /// Press (and release) a key.
    Key(Key, Modifiers),
    /// Type text.
    Text(String),
    /// Save a screenshot.
    Shot(PathBuf),
    /// Close the app.
    Quit,
}

/// Default delay after each key so the UI settles (focus, scroll, animations).
pub const KEY_GAP_SECS: f64 = 0.18;

/// Parses a `--keys` script.
///
/// # Errors
///
/// An unknown key name or a malformed `wait:`.
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
        } else if let Some(t) = tok.strip_prefix("text:") {
            steps.push(Step::Text(t.to_owned()));
        } else if let Some(p) = tok.strip_prefix("shot:") {
            steps.push(Step::Shot(PathBuf::from(p.trim())));
        } else if tok.eq_ignore_ascii_case("quit") {
            steps.push(Step::Quit);
        } else {
            let (mods, name) = match tok.rsplit_once('+') {
                Some((m, k)) if !k.is_empty() => (parse_mods(m)?, k),
                _ => (Modifiers::NONE, tok),
            };
            steps.push(Step::Key(parse_key(name)?, mods));
        }
    }
    Ok(steps)
}

fn parse_mods(s: &str) -> Result<Modifiers, String> {
    let mut m = Modifiers::NONE;
    for part in s.split('+') {
        match part.trim().to_ascii_lowercase().as_str() {
            "shift" => m.shift = true,
            "ctrl" | "control" => {
                m.ctrl = true;
                m.command = true;
            }
            "alt" => m.alt = true,
            other => return Err(format!("unknown modifier {other:?}")),
        }
    }
    Ok(m)
}

fn parse_key(name: &str) -> Result<Key, String> {
    let alias = match name.to_ascii_lowercase().as_str() {
        "back" => Some(Key::Backspace),
        "esc" => Some(Key::Escape),
        "pgup" => Some(Key::PageUp),
        "pgdn" | "pgdown" => Some(Key::PageDown),
        "ok" | "return" => Some(Key::Enter),
        "space" => Some(Key::Space),
        _ => None,
    };
    alias
        .or_else(|| Key::from_name(name))
        .ok_or_else(|| format!("unknown key {name:?}"))
}

/// Runs a script against the app's input and takes screenshots.
#[derive(Debug, Default)]
pub struct Harness {
    steps: VecDeque<Step>,
    started: Option<Instant>,
    next_at: f64,
    final_shot: Option<(f64, PathBuf)>,
    exit_after: Option<f64>,
    closing: bool,
}

impl Harness {
    /// A harness for the given steps and final screenshot / exit time.
    pub fn new(
        steps: Vec<Step>,
        final_shot: Option<(f64, PathBuf)>,
        exit_after: Option<f64>,
    ) -> Self {
        Self {
            steps: steps.into(),
            started: None,
            next_at: 0.0,
            final_shot,
            exit_after,
            closing: false,
        }
    }

    /// True if there is anything to do (keeps the app repainting).
    pub fn is_active(&self) -> bool {
        !self.steps.is_empty() || self.final_shot.is_some() || self.exit_after.is_some()
    }

    fn elapsed(&mut self) -> f64 {
        self.started
            .get_or_insert_with(Instant::now)
            .elapsed()
            .as_secs_f64()
    }

    /// Injects due input events (call from `raw_input_hook`).
    pub fn inject(&mut self, ctx: &egui::Context, raw: &mut RawInput) {
        if !self.is_active() || self.closing {
            return;
        }
        // Scripted runs are deterministic: real mouse/keyboard input on the machine (a
        // resting cursor, a stray click, typing) is dropped. Screenshots are delivered by
        // viewport-command callbacks, not input events, so they are unaffected.
        raw.events.clear();
        let now = self.elapsed();
        // At most one key per frame so each press acts on the updated focus.
        while now >= self.next_at {
            let Some(step) = self.steps.pop_front() else {
                break;
            };
            match step {
                Step::Wait(s) => self.next_at = now + s,
                Step::Key(key, modifiers) => {
                    for pressed in [true, false] {
                        raw.events.push(egui::Event::Key {
                            key,
                            physical_key: None,
                            pressed,
                            repeat: false,
                            modifiers,
                        });
                    }
                    if key == Key::Space {
                        // egui also expects a text event for typing into fields.
                    }
                    self.next_at = now + KEY_GAP_SECS;
                    break;
                }
                Step::Text(t) => {
                    raw.events.push(egui::Event::Text(t));
                    self.next_at = now + KEY_GAP_SECS;
                    break;
                }
                Step::Shot(path) => {
                    screenshot(ctx, path, false);
                    self.next_at = now + 0.3;
                    break;
                }
                Step::Quit => {
                    self.closing = true;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    break;
                }
            }
        }
        if let Some((at, _)) = &self.final_shot
            && now >= *at
            && self.steps.is_empty()
            && let Some((_, path)) = self.final_shot.take()
        {
            self.closing = true;
            screenshot(ctx, path, true);
        }
        if self.exit_after.is_some_and(|t| now >= t) {
            self.closing = true;
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        ctx.request_repaint_after(std::time::Duration::from_millis(30));
    }
}

/// Requests a screenshot of the next rendered frame and saves it as PNG (on a worker
/// thread). With `quit`, closes the app afterwards.
pub fn screenshot(ctx: &egui::Context, path: PathBuf, quit: bool) {
    let ctx2 = ctx.clone();
    ctx.request_screenshot(move |img: Arc<egui::ColorImage>| {
        let ctx3 = ctx2.clone();
        std::thread::spawn(move || {
            match save_png(&path, &img) {
                Ok(()) => tracing::info!(path = %path.display(), w = img.size[0], h = img.size[1], "screenshot saved"),
                Err(e) => tracing::warn!(error = %e, "screenshot failed"),
            }
            if quit {
                ctx3.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            ctx3.request_repaint();
        });
    });
    ctx.request_repaint();
}

fn save_png(path: &Path, img: &egui::ColorImage) -> anyhow::Result<()> {
    if let Some(dir) = path.parent()
        && !dir.as_os_str().is_empty()
    {
        std::fs::create_dir_all(dir)?;
    }
    let mut bytes = Vec::with_capacity(img.pixels.len() * 4);
    for p in &img.pixels {
        bytes.extend_from_slice(&[p.r(), p.g(), p.b(), 255]);
    }
    let w = u32::try_from(img.size[0])?;
    let h = u32::try_from(img.size[1])?;
    let buf = image::RgbaImage::from_raw(w, h, bytes)
        .ok_or_else(|| anyhow::anyhow!("bad screenshot size"))?;
    buf.save(path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_scripts() {
        let s =
            parse_script("wait:2, Down,Down ,Enter,Esc,Back,Shift+Tab,5,text:ab,shot:x.png,quit")
                .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(s[0], Step::Wait(2.0));
        assert_eq!(s[1], Step::Key(Key::ArrowDown, Modifiers::NONE));
        assert_eq!(s[3], Step::Key(Key::Enter, Modifiers::NONE));
        assert_eq!(s[4], Step::Key(Key::Escape, Modifiers::NONE));
        assert_eq!(s[5], Step::Key(Key::Backspace, Modifiers::NONE));
        assert_eq!(s[6], Step::Key(Key::Tab, Modifiers::SHIFT));
        assert_eq!(s[7], Step::Key(Key::Num5, Modifiers::NONE));
        assert_eq!(s[8], Step::Text("ab".into()));
        assert_eq!(s[9], Step::Shot(PathBuf::from("x.png")));
        assert_eq!(s[10], Step::Quit);
        assert!(parse_script("Nope").is_err());
        assert!(parse_script("wait:x").is_err());
    }
}
