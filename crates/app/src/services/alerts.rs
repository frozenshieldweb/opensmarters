// SPDX-License-Identifier: GPL-3.0-or-later
//! App-wide prompts (`cx.alerts`), drawn above every screen: programme reminders
//! ("Starting in 1 min: … Watch / Dismiss", "Switching to … in 10 s") and the sleep
//! timer's "Still watching?".
//!
//! ## The queue
//!
//! A [`Prompt`] has a title, a body, 1–3 actions, an optional countdown and expiry, and a
//! **key** that de-duplicates it: pushing a prompt whose key is queued updates it instead
//! of adding a second one. The most important prompt (then the oldest) is shown as a card
//! at the bottom right (above the OSD in the player); the others wait their turn.
//!
//! Owners don't get callbacks: an action, Esc or an expiry stores an **answer** (the
//! action's id) that the owner takes with [`Alerts::take_answer`] on its next frame. The
//! owner can also update or [`dismiss`](Alerts::dismiss) its prompt at any time.
//!
//! ## Keyboard
//!
//! A prompt that needs an answer ([`Prompt::modal`]) takes the focus with
//! [`focus::trap`], unless a text field has it (typing is never interrupted; the prompt
//! takes the focus once the field loses it). While it holds the focus, [`on_frame`] (which
//! runs before the screen) routes the keys to it, so a screen's own shortcuts can't eat
//! them: ←/→/Tab move between the actions, Enter/Space activate, Esc/Backspace answer with
//! the prompt's cancel action. ↑/↓ are swallowed. When the prompt goes away, the focus
//! returns to where it was.
//!
//! ## Reminders
//!
//! The reminder scheduler and the Guide's markers live in [`crate::services::reminders`]
//! ([`Alerts::reminders`]); [`on_frame`] runs it.

use std::time::{Duration, Instant};

use eframe::egui::{Id, Key, Modifiers, Ui, pos2};

use crate::focus;
use crate::screens::AppCtx;
use crate::services::reminders::{self, Scheduler};
use crate::ui::widgets::alert::{AlertCard, CardAction};
use crate::ui::widgets::{Segmented, SettingRow, Toggle};

/// At most this many prompts wait in the queue (the least important are dropped).
pub const MAX_PROMPTS: usize = 8;
/// Unclaimed answers kept for their owners.
const MAX_ANSWERS: usize = 16;

/// One button of a prompt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Action {
    /// What the owner receives as the answer.
    pub id: &'static str,
    /// Button label (plain language).
    pub label: String,
    /// The primary button (champagne; Enter's default; at most one).
    pub primary: bool,
}

/// A visible countdown (progress bar + "Stopping in 0:23").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Countdown {
    /// When it reaches zero.
    pub until: Instant,
    /// Its full length (for the bar).
    pub total: Duration,
    /// Text before the remaining time, e.g. "Stopping in".
    pub label: &'static str,
}

/// A prompt. Build it with [`Prompt::new`] and the builder methods.
#[derive(Debug, Clone, PartialEq)]
pub struct Prompt {
    /// De-duplication key (also the owner's handle for answers).
    pub key: String,
    /// Phosphor glyph shown next to the title.
    pub icon: &'static str,
    /// Title (`title-sm`).
    pub title: String,
    /// Body (`body`, `text-2`, 3 lines at most).
    pub body: String,
    /// 1–3 actions, in display order (left to right; the primary one last).
    pub actions: Vec<Action>,
    /// The answer for Esc/Backspace.
    pub cancel: &'static str,
    /// A visible countdown.
    pub countdown: Option<Countdown>,
    /// When the prompt goes away on its own, and the answer it then gives.
    pub expires: Option<(Instant, &'static str)>,
    /// Needs an answer: takes the focus (see the [module docs](self)).
    pub modal: bool,
    /// Higher shows first.
    pub priority: u8,
}

impl Prompt {
    /// A prompt with a key, an icon and a title. Esc answers `"dismiss"` until
    /// [`cancel`](Self::cancel) says otherwise.
    pub fn new(key: impl Into<String>, icon: &'static str, title: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            icon,
            title: title.into(),
            body: String::new(),
            actions: Vec::new(),
            cancel: "dismiss",
            countdown: None,
            expires: None,
            modal: false,
            priority: 0,
        }
    }

    /// Body text.
    #[must_use]
    pub fn body(mut self, b: impl Into<String>) -> Self {
        self.body = b.into();
        self
    }

    /// Adds an action (at most three are kept).
    #[must_use]
    pub fn action(mut self, id: &'static str, label: impl Into<String>, primary: bool) -> Self {
        if self.actions.len() < 3 {
            self.actions.push(Action {
                id,
                label: label.into(),
                primary,
            });
        }
        self
    }

    /// The answer Esc gives.
    #[must_use]
    pub fn cancel(mut self, id: &'static str) -> Self {
        self.cancel = id;
        self
    }

    /// Shows a countdown.
    #[must_use]
    pub fn countdown(mut self, c: Countdown) -> Self {
        self.countdown = Some(c);
        self
    }

    /// Goes away at `at`, answering `answer`.
    #[must_use]
    pub fn expires(mut self, at: Instant, answer: &'static str) -> Self {
        self.expires = Some((at, answer));
        self
    }

    /// Needs an answer: takes the focus.
    #[must_use]
    pub fn modal(mut self) -> Self {
        self.modal = true;
        self
    }

    /// Display priority (higher first).
    #[must_use]
    pub fn priority(mut self, p: u8) -> Self {
        self.priority = p;
        self
    }
}

/// The prompt queue (pure; see the [module docs](self)).
#[derive(Debug, Default)]
pub struct Queue {
    prompts: Vec<Prompt>,
    answers: Vec<(String, &'static str)>,
}

impl Queue {
    /// Adds `p`, or updates the queued prompt with the same key (keeping its place).
    /// Returns true if it is new.
    pub fn push(&mut self, p: Prompt) -> bool {
        if let Some(existing) = self.prompts.iter_mut().find(|q| q.key == p.key) {
            *existing = p;
            self.sort();
            return false;
        }
        self.prompts.push(p);
        self.sort();
        if self.prompts.len() > MAX_PROMPTS {
            // The sort is stable: the last one is the least important (then the newest).
            self.prompts.pop();
        }
        true
    }

    fn sort(&mut self) {
        self.prompts.sort_by_key(|p| std::cmp::Reverse(p.priority));
    }

    /// Changes the queued prompt `key` in place. Returns false if it isn't queued.
    pub fn update(&mut self, key: &str, f: impl FnOnce(&mut Prompt)) -> bool {
        match self.prompts.iter_mut().find(|p| p.key == key) {
            Some(p) => {
                f(p);
                true
            }
            None => false,
        }
    }

    /// Removes the prompt `key` without an answer. Returns true if it was queued.
    pub fn dismiss(&mut self, key: &str) -> bool {
        let before = self.prompts.len();
        self.prompts.retain(|p| p.key != key);
        before != self.prompts.len()
    }

    /// Removes every prompt whose key matches, without answers.
    pub fn dismiss_where(&mut self, mut pred: impl FnMut(&str) -> bool) {
        self.prompts.retain(|p| !pred(&p.key));
    }

    /// Answers the prompt `key` with `action` (removes it). Returns false if it isn't
    /// queued (answered twice in one frame, or already gone).
    pub fn answer(&mut self, key: &str, action: &'static str) -> bool {
        if !self.dismiss(key) {
            return false;
        }
        self.answers.retain(|(k, _)| k != key);
        self.answers.push((key.to_owned(), action));
        if self.answers.len() > MAX_ANSWERS {
            self.answers.remove(0);
        }
        true
    }

    /// Takes the answer to the prompt `key`, if any.
    pub fn take_answer(&mut self, key: &str) -> Option<&'static str> {
        let i = self.answers.iter().position(|(k, _)| k == key)?;
        Some(self.answers.remove(i).1)
    }

    /// Answers every prompt whose expiry is at or before `now`.
    pub fn expire(&mut self, now: Instant) {
        let due: Vec<(String, &'static str)> = self
            .prompts
            .iter()
            .filter_map(|p| match p.expires {
                Some((at, answer)) if at <= now => Some((p.key.clone(), answer)),
                _ => None,
            })
            .collect();
        for (key, answer) in due {
            self.answer(&key, answer);
        }
    }

    /// The prompt on screen.
    pub fn head(&self) -> Option<&Prompt> {
        self.prompts.first()
    }

    /// True if `key` is queued.
    pub fn contains(&self, key: &str) -> bool {
        self.prompts.iter().any(|p| p.key == key)
    }

    /// Queued prompts.
    pub fn len(&self) -> usize {
        self.prompts.len()
    }

    /// True if nothing is queued.
    pub fn is_empty(&self) -> bool {
        self.prompts.is_empty()
    }

    /// The earliest expiry or countdown end (to schedule a repaint).
    fn next_deadline(&self) -> Option<Instant> {
        self.prompts
            .iter()
            .flat_map(|p| [p.expires.map(|e| e.0), p.countdown.map(|c| c.until)])
            .flatten()
            .min()
    }
}

/// Prompt queue, focus handling and the reminder scheduler (`cx.alerts`).
#[derive(Debug, Default)]
pub struct Alerts {
    queue: Queue,
    /// Programme reminders: scheduler, firing, the Guide's markers.
    pub reminders: Scheduler,
    /// The prompt holding the focus (set while it is drawn).
    trap: Option<String>,
    /// Where the focus was before the prompt took it.
    focus_return: Option<Id>,
    /// Enter/Space arrived for the prompt this frame.
    activate: bool,
    /// The focus goes back here at the start of next frame (a prompt answered while it was
    /// drawn still traps the focus until the end of that frame).
    return_next: Option<Option<Id>>,
}

impl Alerts {
    /// Shows (or updates) a prompt. Returns true if it is new.
    pub fn push(&mut self, p: Prompt) -> bool {
        self.queue.push(p)
    }

    /// Changes a queued prompt in place.
    pub fn update(&mut self, key: &str, f: impl FnOnce(&mut Prompt)) -> bool {
        self.queue.update(key, f)
    }

    /// Removes a prompt without an answer.
    pub fn dismiss(&mut self, key: &str) -> bool {
        self.queue.dismiss(key)
    }

    /// Removes every prompt whose key matches.
    pub fn dismiss_where(&mut self, pred: impl FnMut(&str) -> bool) {
        self.queue.dismiss_where(pred);
    }

    /// Takes the answer to `key` (the id of the action chosen, the cancel action for Esc,
    /// or the expiry answer).
    pub fn take_answer(&mut self, key: &str) -> Option<&'static str> {
        self.queue.take_answer(key)
    }

    /// True if `key` is queued.
    pub fn contains(&self, key: &str) -> bool {
        self.queue.contains(key)
    }

    /// True while a prompt holds the focus.
    pub fn has_focus(&self) -> bool {
        self.trap.is_some()
    }
}

fn scope_id(key: &str) -> Id {
    Id::unique(("alert", key))
}

fn action_id(key: &str, action: &str) -> Id {
    Id::unique(("alert.action", key, action))
}

/// Called once per frame before the screen: runs the reminder scheduler, expires prompts
/// and routes keys to a prompt that holds the focus.
pub fn on_frame(cx: &mut AppCtx<'_>) {
    let now = Instant::now();
    match cx.alerts.return_next.take() {
        Some(Some(id)) => focus::request(&cx.egui, id),
        Some(None) => focus::clear(&cx.egui),
        None => {}
    }
    reminders::on_frame(cx, now);
    cx.alerts.queue.expire(now);
    if let Some(at) = cx.alerts.queue.next_deadline() {
        let wait = at
            .saturating_duration_since(now)
            .min(Duration::from_millis(250));
        cx.egui.request_repaint_after(wait);
    }
    route_keys(cx);
}

/// Keys for the prompt that holds the focus (before the screen and the focus engine).
fn route_keys(cx: &mut AppCtx<'_>) {
    let Some(p) = cx.alerts.queue.head() else {
        return;
    };
    if cx.alerts.trap.as_deref() != Some(p.key.as_str()) || focus::text_input_focused(&cx.egui) {
        return;
    }
    let key = p.key.clone();
    let cancel = p.cancel;
    let ids: Vec<Id> = p.actions.iter().map(|a| action_id(&key, a.id)).collect();
    let (back, enter, step) = cx.egui.input_mut(|i| {
        let back = i.consume_key(Modifiers::NONE, Key::Escape)
            | i.consume_key(Modifiers::NONE, Key::Backspace);
        let enter =
            i.consume_key(Modifiers::NONE, Key::Enter) | i.consume_key(Modifiers::NONE, Key::Space);
        let prev = i.consume_key(Modifiers::NONE, Key::ArrowLeft)
            | i.consume_key(Modifiers::SHIFT, Key::Tab);
        let next = i.consume_key(Modifiers::NONE, Key::ArrowRight)
            | i.consume_key(Modifiers::NONE, Key::Tab);
        // Up/Down have nowhere to go inside a one-row card; keep them from the page.
        let _ = i.consume_key(Modifiers::NONE, Key::ArrowUp)
            | i.consume_key(Modifiers::NONE, Key::ArrowDown);
        let step: isize = match (prev, next) {
            (true, false) => -1,
            (false, true) => 1,
            _ => 0,
        };
        (back, enter, step)
    });
    if back {
        cx.alerts.queue.answer(&key, cancel);
        if !cx.alerts.queue.head().is_some_and(|n| n.modal) {
            release_focus(cx);
        }
        return;
    }
    if enter {
        cx.alerts.activate = true;
    }
    if step != 0 && !ids.is_empty() {
        let cur = focus::current(&cx.egui)
            .and_then(|c| ids.iter().position(|id| *id == c))
            .unwrap_or(ids.len() - 1);
        let n = ids.len() as isize;
        let next = (cur as isize + step).clamp(0, n - 1) as usize;
        focus::request(&cx.egui, ids[next]);
    }
}

/// Gives the focus back after a prompt that held it went away.
fn release_focus(cx: &mut AppCtx<'_>) {
    if cx.alerts.trap.take().is_none() {
        return;
    }
    match cx.alerts.focus_return.take() {
        Some(id) => focus::request(&cx.egui, id),
        None => focus::clear(&cx.egui),
    }
}

/// Draws the prompt on screen (after the screen and the side rail). The card is its own
/// foreground area, so the app's root `Ui` is not needed.
pub fn show(cx: &mut AppCtx<'_>, _ui: &mut Ui) {
    let ctx = cx.egui.clone();
    let activate = std::mem::take(&mut cx.alerts.activate);
    let Some(p) = cx.alerts.queue.head().cloned() else {
        release_focus(cx);
        return;
    };
    let holds = cx.alerts.trap.as_deref() == Some(p.key.as_str());
    if cx.alerts.trap.is_some() && !holds && !p.modal {
        // The prompt that held the focus is gone and this one doesn't need it.
        release_focus(cx);
    }
    let take_focus = p.modal && (holds || !focus::text_input_focused(&ctx));
    if take_focus {
        if !holds {
            if cx.alerts.trap.is_none() {
                cx.alerts.focus_return = focus::current(&ctx);
            }
            cx.alerts.trap = Some(p.key.clone());
            let first = p
                .actions
                .iter()
                .find(|a| a.primary)
                .or_else(|| p.actions.first());
            if let Some(a) = first {
                focus::request(&ctx, action_id(&p.key, a.id));
            }
        }
        focus::trap(&ctx, scope_id(&p.key));
    }

    // Bottom right; in the player, above the OSD's controls.
    let safe = cx.layout.safe_area();
    let tv = cx.layout.tv;
    // In TV mode the safe area already keeps it off the edges.
    let (inset_x, inset_y) = if tv {
        (0.0, 0.0)
    } else {
        (cx.layout.margin, 32.0)
    };
    let bottom = if cx.sleep.player_on_top(&ctx) {
        safe.bottom() - if tv { 150.0 } else { 196.0 }
    } else {
        safe.bottom() - inset_y
    };
    let anchor = pos2(
        cx.layout.content.right().min(safe.right()) - inset_x,
        bottom,
    );
    let width = 420.0_f32.min(cx.layout.content.width() - 2.0 * inset_x.max(16.0));
    let countdown = p.countdown.map(|c| {
        let left = c.until.saturating_duration_since(Instant::now());
        let frac = if c.total.is_zero() {
            0.0
        } else {
            (left.as_secs_f32() / c.total.as_secs_f32()).clamp(0.0, 1.0)
        };
        (frac, c.label, clock(left))
    });
    let actions: Vec<CardAction<'_>> = p
        .actions
        .iter()
        .map(|a| CardAction {
            id: action_id(&p.key, a.id),
            label: &a.label,
            primary: a.primary,
        })
        .collect();
    let more = cx.alerts.queue.len().saturating_sub(1);
    let chosen = AlertCard {
        id: scope_id(&p.key),
        icon: p.icon,
        title: &p.title,
        body: &p.body,
        actions: &actions,
        countdown,
        more,
        activate: activate && take_focus,
    }
    .show(&ctx, anchor, width);
    if let Some(i) = chosen
        && let Some(a) = p.actions.get(i)
    {
        cx.alerts.queue.answer(&p.key, a.id);
        // A next prompt that needs an answer takes the focus over next frame (and gives it
        // back to the same place); otherwise it goes back at the start of next frame,
        // before the owner acts on the answer (and any navigation saves the focus).
        if !cx.alerts.queue.head().is_some_and(|n| n.modal) && cx.alerts.trap.take().is_some() {
            cx.alerts.return_next = Some(cx.alerts.focus_return.take());
        }
    }
    if p.countdown.is_some() {
        ctx.request_repaint_after(Duration::from_millis(200));
    }
}

/// "0:23" / "1:05".
fn clock(left: Duration) -> String {
    let s = left.as_secs() + u64::from(left.subsec_nanos() > 0);
    format!("{}:{:02}", s / 60, s % 60)
}

/// The settings of reminders and the sleep timer, for Settings › Recordings & downloads:
/// when reminders appear (at the start, 1 min or 5 min before) and whether the sleep timer
/// fades the sound out.
pub fn settings_rows(cx: &mut AppCtx<'_>, ui: &mut Ui) {
    let lead = cx.alerts.reminders.lead_secs();
    SettingRow::new("Programme reminders")
        .description("When a reminder set in the Guide appears")
        .show(ui, |ui| {
            let mut sel = reminders::LEAD_CHOICES
                .iter()
                .position(|s| *s == lead)
                .unwrap_or(1);
            let changed = Segmented::new(
                Id::unique("settings.reminder-lead"),
                &["At the start", "1 min before", "5 min before"],
                420.0,
            )
            .show(ui, &mut sel);
            if changed && let Some(secs) = reminders::LEAD_CHOICES.get(sel) {
                cx.alerts.reminders.set_lead_secs(cx.services, *secs);
            }
        });
    SettingRow::new("Sleep timer fades out")
        .description("Lowers the sound over the last 30 seconds before it stops")
        .show(ui, |ui| {
            let mut on = cx.sleep.fade_out();
            Toggle::new(Id::unique("settings.sleep-fade")).show(ui, &mut on);
            if on != cx.sleep.fade_out() {
                cx.sleep.set_fade_out(cx.services, on);
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prompt(key: &str, priority: u8) -> Prompt {
        Prompt::new(key, "", format!("title {key}"))
            .action("dismiss", "Dismiss", false)
            .action("watch", "Watch", true)
            .priority(priority)
    }

    #[test]
    fn same_key_updates_instead_of_duplicating() {
        let mut q = Queue::default();
        assert!(q.push(prompt("reminder:1", 0)));
        assert!(q.push(prompt("reminder:2", 0)));
        assert!(!q.push(prompt("reminder:1", 0).body("updated")));
        assert_eq!(q.len(), 2);
        // It kept its place (first in, shown first) and took the new content.
        let head = q.head().unwrap_or_else(|| panic!("empty"));
        assert_eq!(head.key, "reminder:1");
        assert_eq!(head.body, "updated");
    }

    #[test]
    fn priority_orders_the_queue() {
        let mut q = Queue::default();
        q.push(prompt("reminder:1", 0));
        q.push(prompt("sleep", 10));
        q.push(prompt("reminder:2", 0));
        let keys: Vec<&str> = q.prompts.iter().map(|p| p.key.as_str()).collect();
        assert_eq!(keys, ["sleep", "reminder:1", "reminder:2"]);
    }

    #[test]
    fn answers_go_to_their_owner_once() {
        let mut q = Queue::default();
        q.push(prompt("sleep", 10));
        assert!(q.answer("sleep", "keep"));
        assert!(!q.answer("sleep", "stop"), "already answered");
        assert!(!q.contains("sleep"));
        assert_eq!(q.take_answer("reminder:1"), None);
        assert_eq!(q.take_answer("sleep"), Some("keep"));
        assert_eq!(q.take_answer("sleep"), None);
    }

    #[test]
    fn dismiss_removes_without_an_answer() {
        let mut q = Queue::default();
        q.push(prompt("sleep", 10));
        q.push(prompt("reminder:3", 0));
        assert!(q.dismiss("sleep"));
        assert!(!q.dismiss("sleep"));
        assert_eq!(q.take_answer("sleep"), None);
        q.dismiss_where(|k| k.starts_with("reminder:"));
        assert!(q.is_empty());
    }

    #[test]
    fn expiry_answers_with_its_action() {
        let t0 = Instant::now();
        let mut q = Queue::default();
        q.push(prompt("reminder:1", 0).expires(t0 + Duration::from_secs(10), "switch"));
        q.push(prompt("reminder:2", 0));
        q.expire(t0 + Duration::from_secs(9));
        assert_eq!(q.len(), 2);
        q.expire(t0 + Duration::from_secs(10));
        assert_eq!(q.len(), 1);
        assert_eq!(q.take_answer("reminder:1"), Some("switch"));
        assert_eq!(q.next_deadline(), None);
    }

    #[test]
    fn the_queue_is_bounded() {
        let mut q = Queue::default();
        q.push(prompt("sleep", 10));
        for i in 0..20 {
            q.push(prompt(&format!("reminder:{i}"), 0));
        }
        assert_eq!(q.len(), MAX_PROMPTS);
        assert!(q.contains("sleep"), "the important one stays");
        assert!(
            q.contains("reminder:0"),
            "the oldest stay; newer ones wait or drop"
        );
        for i in 0..40 {
            q.push(prompt(&format!("x:{i}"), 0));
            q.answer(&format!("x:{i}"), "dismiss");
        }
        assert!(q.answers.len() <= MAX_ANSWERS);
    }

    #[test]
    fn at_most_three_actions() {
        let p = Prompt::new("k", "", "t")
            .action("a", "A", false)
            .action("b", "B", false)
            .action("c", "C", true)
            .action("d", "D", false);
        assert_eq!(p.actions.len(), 3);
        assert_eq!(p.cancel, "dismiss");
    }

    #[test]
    fn clock_rounds_up() {
        assert_eq!(clock(Duration::from_millis(22_100)), "0:23");
        assert_eq!(clock(Duration::from_secs(65)), "1:05");
        assert_eq!(clock(Duration::ZERO), "0:00");
    }
}
