// SPDX-License-Identifier: GPL-3.0-or-later
//! Settings › Parental control.
//!
//! - **The page itself is protected**: while a PIN is set, it opens only after the PIN is
//!   entered (inline [`PinPrompt`], lockout shown as "Try again in 0:27"). It locks again
//!   when you leave the page.
//! - **PIN**: set (choose 4–8 digits, then confirm), change (new PIN twice; the current one
//!   was entered to open the page), turn off. Hashing (Argon2id) and the rate limit live in
//!   the store and run off the UI thread; the digit count is kept so PIN pads submit on the
//!   last digit ([`crate::services::parental::PIN_LENGTH`]).
//! - **Keywords**: the whole-word, case-insensitive list that locks matching categories
//!   (add, remove, restore defaults).
//! - **Category locks** for this profile: per category of Live TV / Movies / Series,
//!   *Auto* (keywords decide), *Lock* (always) or *Allow* (never, even if a keyword
//!   matches).
//! - **Forgot PIN**: enter the password of one of this computer's Xtream accounts (or an M3U
//!   playlist address) to remove the PIN; otherwise the documented way is deleting the
//!   app's data folder. See [`crate::services::parental`].
//!
//! While a PIN is set it is also required to add, edit or delete profiles and to import a
//! backup (profiles screen, Settings › Profiles / Account / Storage).
//!
//! Every change ends with `session.reload_parental()` so Live TV, Movies and Series apply it
//! immediately.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use eframe::egui::{self, Id, Ui};
use opensmarters_core::Category;
use opensmarters_core::parental::KeywordMatcher;
use opensmarters_core::secrecy::{ExposeSecret, SecretString};
use opensmarters_store::{
    CategoryLock, ContentKind, DEFAULT_KEYWORDS, PinOutcome, PinStatus, ProfileId, Store,
};

use super::pin_flow::{PinSetup, SetupOutcome};
use super::{SettingsPageUi, page_heading, section, value_label};
use crate::error::AppError;
use crate::focus;
use crate::screens::AppCtx;
use crate::services::parental::{
    MAX_PIN, MIN_PIN, ResetLimiter, ResetProof, lockout_text, pin_length, set_pin_length,
    verify_account_secret,
};
use crate::services::{Services, Session};
use crate::task::Loader;
use crate::ui::icons;
use crate::ui::text;
use crate::ui::theme::{color, font};
use crate::ui::widgets::{
    Button, Callout, CalloutKind, Dialog, PinPad, PinPadEvent, PinPrompt, PinPromptResult,
    SearchField, Segmented, SettingRow, TextField, feedback,
};

const KINDS: [ContentKind; 3] = [ContentKind::Live, ContentKind::Movie, ContentKind::Series];
const KIND_LABELS: [&str; 3] = ["Live TV", "Movies", "Series"];
const MAX_KEYWORD_CHARS: usize = 64;

/// What the page shows, loaded off the UI thread.
#[derive(Debug, Clone)]
struct ParentalData {
    status: PinStatus,
    len: usize,
    keywords: Vec<String>,
    locks: HashMap<ContentKind, HashMap<String, CategoryLock>>,
}

#[derive(Debug)]
struct Setup {
    flow: PinSetup,
    pad: PinPad,
    /// Changing an existing PIN (else setting the first one).
    change: bool,
}

impl Setup {
    fn new(change: bool) -> Self {
        Self {
            flow: PinSetup::new(),
            pad: PinPad::variable(Id::unique("settings.parental.pad.choose"), MIN_PIN, MAX_PIN),
            change,
        }
    }
}

#[derive(Debug)]
struct Forgot {
    text: String,
    error: Option<String>,
    check: Loader<ResetProof>,
    nothing_to_check: bool,
}

/// Results of PIN jobs.
#[derive(Debug)]
enum Job {
    Saved(SecretString),
    Removed,
    Refused(PinOutcome),
}

/// Matches of the keyword list against one section's categories (cached).
#[derive(Debug, Default)]
struct MatchCache {
    key: Option<(usize, u64, Vec<String>)>,
    matches: Vec<Option<String>>,
}

/// The parental control page.
#[derive(Debug, Default)]
pub struct ParentalPage {
    data: Loader<ParentalData>,
    /// `Some` once the page is open: the verified PIN (`None` when no PIN is set).
    unlocked: Option<Option<SecretString>>,
    gate: Option<PinPrompt>,
    setup: Option<Setup>,
    job: Loader<Job>,
    confirm_off: bool,
    forgot: Option<Forgot>,
    limiter: ResetLimiter,
    keyword: String,
    keyword_error: Option<String>,
    kind: usize,
    cats: [Loader<Arc<Vec<Category>>>; 3],
    filter: String,
    matches: MatchCache,
    loaded_for: Option<ProfileId>,
}

fn load_data(store: &Store, profile: ProfileId) -> Result<ParentalData, AppError> {
    let par = store.parental();
    let mut locks = HashMap::new();
    for kind in KINDS {
        locks.insert(kind, par.category_locks(profile, kind)?);
    }
    Ok(ParentalData {
        status: par.status()?,
        len: pin_length(store),
        keywords: par.keywords()?,
        locks,
    })
}

/// The per-category Auto / Lock / Allow control: a segmented control whose default
/// ("Auto") selection is a quiet `surface-4` pill, so only overrides get the cream fill and
/// stand out in a long list. Returns true if the selection changed.
fn lock_choice(ui: &mut Ui, id: Id, selected: &mut usize) -> bool {
    use eframe::egui::{Align2, CornerRadius, Rect, Sense, pos2, vec2};

    use crate::focus::FocusOpts;
    use crate::ui::{motion, paint, theme::radius};

    const OPTIONS: [&str; 3] = ["Auto", "Lock", "Allow"];
    let (rect, _) = ui.allocate_exact_size(vec2(252.0, 44.0), Sense::hover());
    let painter = ui.painter().clone();
    painter.rect_filled(rect, CornerRadius::same(radius::FULL), color::SURFACE_2);
    let inner = rect.shrink(4.0);
    let seg_w = inner.width() / OPTIONS.len() as f32;
    let pos = motion::animate_value(
        ui.ctx(),
        id.with("slide"),
        *selected as f32,
        crate::ui::theme::motion::BASE,
    );
    let pill = Rect::from_min_size(
        pos2(inner.left() + seg_w * pos, inner.top()),
        vec2(seg_w, inner.height()),
    );
    let overridden = *selected != 0;
    painter.rect_filled(
        pill,
        CornerRadius::same(radius::FULL),
        if overridden {
            color::CREAM
        } else {
            color::SURFACE_4
        },
    );
    let mut changed = false;
    for (i, label) in OPTIONS.iter().enumerate() {
        let seg = Rect::from_min_size(
            pos2(inner.left() + seg_w * i as f32, inner.top()),
            vec2(seg_w, inner.height()),
        );
        let (_, f) = focus::focusable(ui, id.with(i), seg, FocusOpts::default());
        if f.activated && *selected != i {
            *selected = i;
            changed = true;
        }
        let fg = match (i == *selected, overridden) {
            (true, true) => color::TEXT_ON_ACCENT,
            (true, false) => color::TEXT,
            _ if f.focused => color::TEXT,
            _ => color::TEXT_2,
        };
        painter.text(
            seg.center(),
            Align2::CENTER_CENTER,
            *label,
            font::button(),
            fg,
        );
        if f.focused {
            paint::focus_ring(&painter, seg, radius::FULL, 1.0);
        }
    }
    changed
}

/// Lower-cases and trims like the store; `None` with a message if unusable.
fn normalize_keyword(k: &str) -> Result<String, &'static str> {
    let k = k.trim().to_lowercase();
    if k.is_empty() {
        return Err("Type a word first.");
    }
    if k.chars().count() > MAX_KEYWORD_CHARS || k.chars().any(char::is_control) {
        return Err("Use at most 64 letters, digits or symbols.");
    }
    Ok(k)
}

impl ParentalPage {
    fn reload(&mut self, cx: &AppCtx<'_>) {
        let Some(session) = cx.session() else { return };
        let store = cx.services.store.clone();
        let profile = session.profile_id();
        self.loaded_for = Some(profile);
        self.data
            .start_blocking(cx.rt(), &cx.egui, move || load_data(&store, profile));
    }

    /// Locks the page again (leaving it, or Settings).
    fn lock(&mut self) {
        self.unlocked = None;
        self.gate = None;
        self.setup = None;
        self.confirm_off = false;
        self.forgot = None;
        self.keyword.clear();
        self.keyword_error = None;
    }

    /// Re-reads the session's parental state after a change, then the page.
    fn after_change(&mut self, cx: &AppCtx<'_>) {
        if let Some(session) = cx.session() {
            let ctx = cx.egui.clone();
            cx.services.spawn(async move {
                session.reload_parental().await;
                ctx.request_repaint();
            });
        }
        self.reload(cx);
    }

    fn poll_job(&mut self, cx: &mut AppCtx<'_>) {
        if !self.job.poll() {
            if let Some(e) = self.job.error().cloned() {
                tracing::warn!(error = %e, "parental change failed");
                cx.toasts
                    .error(format!("Couldn't save the change. {}", e.user_message()));
                self.job = Loader::new();
                self.setup = None;
            }
            return;
        }
        match self.job.take() {
            Some(Job::Saved(pin)) => {
                let changed = self.setup.as_ref().is_some_and(|s| s.change);
                cx.toasts.success(if changed {
                    "PIN changed."
                } else {
                    "PIN set. Parental control is on."
                });
                self.unlocked = Some(Some(pin));
                self.setup = None;
                self.after_change(cx);
                focus::request(&cx.egui, Id::unique("settings.parental.change"));
            }
            Some(Job::Removed) => {
                cx.toasts
                    .success("Parental control is off. Your words and locks are kept.");
                self.unlocked = Some(None);
                self.after_change(cx);
                focus::request(&cx.egui, Id::unique("settings.parental.set"));
            }
            Some(Job::Refused(outcome)) => {
                // The PIN changed elsewhere since the page was opened: ask again.
                let msg = match outcome {
                    PinOutcome::LockedOut { until } => {
                        lockout_text(crate::services::parental::remaining_until(
                            until,
                            opensmarters_store::Timestamp::now(),
                        ))
                    }
                    _ => "The PIN was changed meanwhile. Enter it again.".to_owned(),
                };
                cx.toasts.error(msg);
                self.lock();
                self.reload(cx);
            }
            None => {}
        }
    }

    fn save_pin(&mut self, cx: &AppCtx<'_>, new: SecretString, change: bool) {
        let store = cx.services.store.clone();
        let current = self.unlocked.clone().flatten();
        self.job.start_blocking(cx.rt(), &cx.egui, move || {
            let par = store.parental();
            let len = new.expose_secret().len();
            if change {
                let Some(current) = current else {
                    return Err(AppError::msg("Enter the current PIN first."));
                };
                let outcome = par.change_pin(&current, &new)?;
                if !outcome.is_ok() {
                    return Ok(Job::Refused(outcome));
                }
            } else {
                par.set_pin(&new)?;
            }
            set_pin_length(&store, Some(len))?;
            Ok(Job::Saved(new))
        });
    }

    fn remove_pin(&mut self, cx: &AppCtx<'_>) {
        let store = cx.services.store.clone();
        let Some(current) = self.unlocked.clone().flatten() else {
            return;
        };
        self.job.start_blocking(cx.rt(), &cx.egui, move || {
            let outcome = store.parental().remove_pin(&current)?;
            if !outcome.is_ok() {
                return Ok(Job::Refused(outcome));
            }
            set_pin_length(&store, None)?;
            Ok(Job::Removed)
        });
    }

    // ---------------------------------------------------------------------------------
    // Sections
    // ---------------------------------------------------------------------------------

    fn setup_ui(&mut self, cx: &mut AppCtx<'_>, ui: &mut Ui) {
        let Some(setup) = &mut self.setup else { return };
        let busy = self.job.is_loading();
        let mut cancel = false;
        let mut entered = None;
        ui.vertical_centered(|ui| {
            ui.add_space(8.0);
            let title = if setup.change && !setup.flow.is_confirming() {
                "Choose a new PIN"
            } else {
                setup.flow.title()
            };
            if let PinPadEvent::Submitted(pin) = setup.pad.show(ui, title) {
                entered = Some(pin);
            }
            ui.add_space(8.0);
            let (msg, c) = match setup.flow.error() {
                Some(e) => (e, color::DANGER),
                None => (setup.flow.hint(), color::TEXT_3),
            };
            ui.label(egui::RichText::new(msg).font(font::caption()).color(c));
            ui.add_space(16.0);
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 12.0;
                let choosing = !setup.flow.is_confirming();
                let cont =
                    Button::primary(if busy { "Saving…" } else { "Continue" }).icon(icons::CHECK);
                let back = Button::ghost("Cancel");
                let w = back.size(ui).x
                    + if choosing {
                        cont.size(ui).x + 12.0
                    } else {
                        0.0
                    };
                ui.add_space(((ui.available_width() - w) / 2.0).max(0.0));
                if back
                    .id(Id::unique("settings.parental.setup.cancel"))
                    .show(ui)
                    .activated
                {
                    cancel = true;
                }
                if choosing
                    && cont
                        .enabled(setup.pad.can_submit() && !busy)
                        .id(setup.pad.submit_id())
                        .show(ui)
                        .activated
                {
                    entered = setup.pad.submit();
                }
            });
        });
        if cancel && !busy {
            self.setup = None;
            return;
        }
        let Some(pin) = entered else { return };
        let Some(setup) = &mut self.setup else { return };
        match setup.flow.enter(pin) {
            SetupOutcome::Confirm => {
                let (n, _) = setup.flow.pad_range();
                setup.pad = PinPad::new(Id::unique("settings.parental.pad.confirm"), n);
            }
            SetupOutcome::Invalid => setup.pad.reset(),
            SetupOutcome::Mismatch => {
                setup.pad =
                    PinPad::variable(Id::unique("settings.parental.pad.choose"), MIN_PIN, MAX_PIN);
            }
            SetupOutcome::Done(pin) => {
                setup.pad.set_busy(true);
                let change = setup.change;
                self.save_pin(cx, pin, change);
            }
        }
    }

    fn pin_section(&mut self, cx: &mut AppCtx<'_>, ui: &mut Ui, data: &ParentalData) {
        let w = ui.available_width();
        if data.status.is_set {
            Callout::new(
                CalloutKind::Success,
                "On:",
                "categories with a locked word in their name, or that you lock below, ask for the PIN.",
            )
            .width(w)
            .show(ui);
        } else {
            Callout::new(
                CalloutKind::Info,
                "Off:",
                "nothing is locked. Set a PIN to lock adult categories on every profile.",
            )
            .width(w)
            .show(ui);
        }
        ui.add_space(8.0);
        if data.status.is_set {
            SettingRow::new("PIN")
                .description("Needed for locked categories, this page and profile changes")
                .show(ui, |ui| {
                    value_label(ui, &format!("{} digits", data.len), color::TEXT_2);
                });
            ui.add_space(12.0);
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 12.0;
                if Button::secondary("Change PIN")
                    .icon(icons::KEY)
                    .id(Id::unique("settings.parental.change"))
                    .show(ui)
                    .activated
                {
                    self.setup = Some(Setup::new(true));
                }
                if Button::danger("Turn off parental control")
                    .icon(icons::LOCK_SIMPLE_OPEN)
                    .id(Id::unique("settings.parental.off"))
                    .show(ui)
                    .activated
                {
                    self.confirm_off = true;
                }
            });
        } else if Button::primary("Set a PIN")
            .icon(icons::LOCK_SIMPLE)
            .autofocus()
            .id(Id::unique("settings.parental.set"))
            .show(ui)
            .activated
        {
            self.setup = Some(Setup::new(false));
        }
        let _ = cx;
    }

    fn keywords_section(&mut self, cx: &mut AppCtx<'_>, ui: &mut Ui, data: &mut ParentalData) {
        section(
            ui,
            "Locked words",
            Some(
                "Categories whose name contains one of these words ask for the PIN. Whole words, \
                 any case: \u{201c}adult\u{201d} locks \u{201c}ADULT | Films\u{201d} but not \
                 \u{201c}Adulthood\u{201d}.",
            ),
        );
        // Chips: activate to remove.
        let mut remove = None;
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = egui::vec2(8.0, 8.0);
            if data.keywords.is_empty() {
                ui.label(
                    egui::RichText::new("No words: only categories you lock below are locked.")
                        .font(font::body())
                        .color(color::TEXT_3),
                );
            }
            for k in &data.keywords {
                if Button::secondary(text::cap(k, 40).into_owned())
                    .trailing(icons::X)
                    .small()
                    .id(Id::unique(("settings.parental.kw", k.as_str())))
                    .show(ui)
                    .activated
                {
                    remove = Some(k.clone());
                }
            }
        });
        ui.add_space(12.0);
        let mut add = false;
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 12.0;
            let r = TextField::new(Id::unique("settings.parental.kw-input"), "")
                .placeholder("Add a word, e.g. \u{201c}xxx\u{201d}")
                .width(300.0)
                .char_limit(MAX_KEYWORD_CHARS)
                .error(self.keyword_error.as_deref())
                .show(ui, &mut self.keyword);
            if r.changed {
                self.keyword_error = None;
            }
            add |= r.submitted;
            add |= Button::secondary("Add")
                .icon(icons::PLUS)
                .id(Id::unique("settings.parental.kw-add"))
                .show(ui)
                .activated;
            if Button::ghost("Restore defaults")
                .icon(icons::ARROW_COUNTER_CLOCKWISE)
                .id(Id::unique("settings.parental.kw-reset"))
                .show(ui)
                .activated
            {
                data.keywords = DEFAULT_KEYWORDS.iter().map(|s| (*s).to_owned()).collect();
                data.keywords.sort();
                self.spawn_store(cx, "restore the default words", |s| {
                    s.parental().reset_keywords()
                });
                cx.toasts.info("Default words restored.");
            }
        });
        if add {
            match normalize_keyword(&self.keyword) {
                Ok(k) if data.keywords.contains(&k) => {
                    self.keyword_error = Some("That word is already in the list.".into());
                }
                Ok(_) if data.keywords.len() >= 200 => {
                    self.keyword_error = Some("The list is full (200 words).".into());
                }
                Ok(k) => {
                    data.keywords.push(k.clone());
                    data.keywords.sort();
                    self.keyword.clear();
                    self.spawn_store(cx, "add the word", move |s| {
                        s.parental().add_keyword(&k).map(|_| ())
                    });
                }
                Err(e) => self.keyword_error = Some(e.to_owned()),
            }
        }
        if let Some(k) = remove {
            data.keywords.retain(|x| *x != k);
            cx.toasts
                .info(format!("\u{201c}{}\u{201d} removed.", text::cap(&k, 40)));
            self.spawn_store(cx, "remove the word", move |s| {
                s.parental().remove_keyword(&k).map(|_| ())
            });
        }
    }

    /// Runs a store change in the background, then reloads the session's parental state.
    fn spawn_store<F>(&self, cx: &AppCtx<'_>, what: &'static str, f: F)
    where
        F: FnOnce(&Store) -> Result<(), opensmarters_store::StoreError> + Send + 'static,
    {
        let services: Services = cx.services.clone();
        let session = cx.session();
        let ctx = cx.egui.clone();
        cx.services.spawn(async move {
            let store = services.store.clone();
            let ok = matches!(
                tokio::task::spawn_blocking(move || f(&store)).await,
                Ok(Ok(()))
            );
            if !ok {
                tracing::warn!("{what} failed");
                services.report_background_error(what);
            }
            if let Some(s) = session {
                s.reload_parental().await;
            }
            ctx.request_repaint();
        });
    }

    fn locks_section(
        &mut self,
        cx: &mut AppCtx<'_>,
        ui: &mut Ui,
        session: &Arc<Session>,
        data: &mut ParentalData,
    ) {
        section(
            ui,
            "Categories on this profile",
            Some(
                "Auto: locked when a word above matches. Lock: always asks for the PIN. \
                 Allow: never locked, even if a word matches.",
            ),
        );
        let xtream = session.is_xtream();
        let labels: &[&str] = if xtream {
            &KIND_LABELS
        } else {
            &KIND_LABELS[..1]
        };
        let mut kind = self.kind.min(labels.len() - 1);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 16.0;
            Segmented::new(
                Id::unique("settings.parental.kind"),
                labels,
                120.0 * labels.len() as f32,
            )
            .show(ui, &mut kind);
            SearchField::new(Id::unique("settings.parental.filter"), "Filter categories")
                .size(260.0, 44.0)
                .show(ui, &mut self.filter);
        });
        if kind != self.kind {
            self.kind = kind;
        }
        ui.add_space(8.0);
        // Load the section's categories.
        let loader = &mut self.cats[kind];
        loader.poll();
        if loader.is_idle() {
            let s = Arc::clone(session);
            loader.start(cx.rt(), &cx.egui, move |_emit| async move {
                match kind {
                    0 => s
                        .live_catalog()
                        .await
                        .map(|c| Arc::new(c.categories.clone())),
                    1 => s.vod_categories().await,
                    _ => s.series_categories().await,
                }
            });
        }
        let Some(cats) = loader.value().cloned() else {
            if let Some(e) = loader.error() {
                Callout::new(
                    CalloutKind::Danger,
                    "Couldn't load the categories:",
                    e.user_message(),
                )
                .width(ui.available_width())
                .show(ui);
            } else if loader.show_skeleton() {
                for i in 0..4 {
                    let (r, _) = ui.allocate_exact_size(
                        egui::vec2(ui.available_width(), 56.0),
                        egui::Sense::hover(),
                    );
                    feedback::skeleton(ui.ctx(), ui.painter(), r.shrink2(egui::vec2(0.0, 8.0)), 8);
                    let _ = i;
                }
            }
            return;
        };
        // Keyword matches, cached per (section, list, words).
        let key = (kind, loader.generation(), data.keywords.clone());
        if self.matches.key.as_ref() != Some(&key) {
            let m = KeywordMatcher::new(data.keywords.iter().map(String::as_str));
            self.matches.matches = cats
                .iter()
                .map(|c| m.matching_keyword(&c.name).map(str::to_owned))
                .collect();
            self.matches.key = Some(key);
        }
        let ck = KINDS[kind];
        let filter = self.filter.trim().to_lowercase();
        let mut shown = 0;
        let mut changes = Vec::new();
        for (i, c) in cats.iter().enumerate() {
            if !filter.is_empty() && !c.name.to_lowercase().contains(&filter) {
                continue;
            }
            shown += 1;
            let lock = data
                .locks
                .get(&ck)
                .and_then(|m| m.get(c.id.as_str()))
                .copied();
            let matched = self.matches.matches.get(i).cloned().flatten();
            let mut status = match (lock, &matched) {
                (Some(CategoryLock::Locked), _) => "Locked: always asks for the PIN".to_owned(),
                (Some(CategoryLock::Unlocked), Some(_)) => {
                    "Allowed (a word matches, but you allowed it)".to_owned()
                }
                (Some(CategoryLock::Unlocked), None) => "Allowed: never locked".to_owned(),
                (None, Some(k)) => {
                    format!("Locked by the word \u{201c}{}\u{201d}", text::cap(k, 24))
                }
                (None, None) => "Not locked".to_owned(),
            };
            let would_lock = matches!(
                (lock, &matched),
                (Some(CategoryLock::Locked), _) | (None, Some(_))
            );
            if !data.status.is_set && would_lock {
                let mut chars = status.chars();
                let first = chars.next().map(|c| c.to_lowercase().to_string());
                status = format!(
                    "Once a PIN is set: {}{}",
                    first.unwrap_or_default(),
                    chars.as_str()
                );
            }
            let name = text::cap(&c.name, 80);
            let mut sel = match lock {
                None => 0,
                Some(CategoryLock::Locked) => 1,
                Some(CategoryLock::Unlocked) => 2,
            };
            SettingRow::new(&name).description(&status).show(ui, |ui| {
                if lock_choice(
                    ui,
                    Id::unique(("settings.parental.lock", kind, c.id.as_str())),
                    &mut sel,
                ) {
                    let new = match sel {
                        1 => Some(CategoryLock::Locked),
                        2 => Some(CategoryLock::Unlocked),
                        _ => None,
                    };
                    changes.push((c.id.as_str().to_owned(), new));
                }
            });
        }
        if shown == 0 {
            ui.label(
                egui::RichText::new(if cats.is_empty() {
                    "This profile has no categories here."
                } else {
                    "No category matches the filter."
                })
                .font(font::body())
                .color(color::TEXT_3),
            );
        }
        let profile = session.profile_id();
        for (id, new) in changes {
            let entry = data.locks.entry(ck).or_default();
            match new {
                Some(l) => {
                    entry.insert(id.clone(), l);
                }
                None => {
                    entry.remove(&id);
                }
            }
            self.spawn_store(cx, "save the category lock", move |s| {
                s.parental().set_category_lock(profile, ck, &id, new)
            });
        }
    }

    fn dialogs(&mut self, cx: &mut AppCtx<'_>, ui: &Ui) {
        if self.confirm_off {
            let mut close = false;
            Dialog::new(
                Id::unique("settings.parental.off.dialog"),
                "Turn off parental control?",
            )
            .body(
                "Locked categories will open without a PIN on every profile. Your words and \
                 category locks are kept for when you set a PIN again.",
            )
            .show(ui.ctx(), |ui| {
                if Button::danger("Turn off")
                    .icon(icons::LOCK_SIMPLE_OPEN)
                    .id(Id::unique("settings.parental.off.yes"))
                    .show(ui)
                    .activated
                {
                    self.remove_pin(cx);
                    close = true;
                }
                if Button::secondary("Keep it on")
                    .autofocus()
                    .id(Id::unique("settings.parental.off.no"))
                    .show(ui)
                    .activated
                {
                    close = true;
                }
            });
            if close {
                self.confirm_off = false;
                focus::request(&cx.egui, Id::unique("settings.parental.off"));
            }
        }
        self.forgot_dialog(cx, ui);
    }

    fn forgot_dialog(&mut self, cx: &mut AppCtx<'_>, ui: &Ui) {
        let Some(f) = &mut self.forgot else { return };
        // Result of a check.
        if f.check.poll() {
            match f.check.take() {
                Some(ResetProof::Matched) => {
                    self.limiter.succeeded();
                    self.forgot = None;
                    self.unlocked = Some(None);
                    self.gate = None;
                    cx.toasts
                        .success("PIN removed. Parental control is off until you set a new PIN.");
                    self.after_change(cx);
                    return;
                }
                Some(ResetProof::NoMatch) => {
                    self.limiter.failed(Instant::now());
                    f.error = Some(
                        "That doesn't match any account password or playlist address on this \
                         computer."
                            .into(),
                    );
                }
                Some(ResetProof::NothingToCheck) => f.nothing_to_check = true,
                None => {}
            }
        } else if let Some(e) = f.check.error().cloned() {
            f.error = Some(e.user_message());
            f.check = Loader::new();
        }
        let data_dir = cx.services.paths.data_dir.as_ref().map_or_else(
            || "(not saved: this is a temporary session)".to_owned(),
            |p| p.display().to_string(),
        );
        let wait = self.limiter.wait(Instant::now());
        let mut close = false;
        let mut submit = false;
        let busy = f.check.is_loading();
        Dialog::new(Id::unique("settings.parental.forgot"), "Forgot your PIN?")
            .width(560.0)
            .show_custom(ui.ctx(), |ui| {
                ui.label(
                    egui::RichText::new("Forgot your PIN?")
                        .font(font::title())
                        .color(color::TEXT),
                );
                ui.add_space(6.0);
                let body = if f.nothing_to_check {
                    format!(
                        "None of your profiles has a password or playlist address to check. The \
                         only way to reset the PIN is to close opensmarters and delete its data \
                         folder, which also removes every profile and setting:\n{data_dir}"
                    )
                } else {
                    format!(
                        "To remove the PIN, type the password of one of your Xtream accounts on \
                         this computer, or the full address of an M3U playlist. Nothing is sent \
                         anywhere: it's compared with what's stored in Windows Credential \
                         Manager.\n\nIf you can't, close opensmarters and delete its data folder \
                         (this removes every profile and setting):\n{data_dir}"
                    )
                };
                ui.label(
                    egui::RichText::new(body)
                        .font(font::body())
                        .color(color::TEXT_2),
                );
                ui.add_space(16.0);
                if !f.nothing_to_check {
                    let err = match wait {
                        Some(d) => Some(format!("Too many tries. {}", lockout_text(d))),
                        None => f.error.clone(),
                    };
                    let r = TextField::new(
                        Id::unique("settings.parental.forgot.field"),
                        "Account password or playlist address",
                    )
                    .password(true)
                    .autofocus(true)
                    .width(ui.available_width())
                    .char_limit(4096)
                    .error(err.as_deref())
                    .show(ui, &mut f.text);
                    if r.changed {
                        f.error = None;
                    }
                    submit |= r.submitted;
                    if wait.is_some() {
                        ui.ctx()
                            .request_repaint_after(std::time::Duration::from_millis(250));
                    }
                }
                ui.add_space(20.0);
                let w = ui.available_width();
                ui.allocate_ui_with_layout(
                    egui::vec2(w, 44.0),
                    egui::Layout::right_to_left(egui::Align::Center),
                    |ui| {
                        ui.spacing_mut().item_spacing.x = 12.0;
                        if !f.nothing_to_check
                            && Button::primary(if busy {
                                "Checking…"
                            } else {
                                "Remove the PIN"
                            })
                            .icon(icons::LOCK_SIMPLE_OPEN)
                            .enabled(!busy && wait.is_none() && !f.text.is_empty())
                            .id(Id::unique("settings.parental.forgot.ok"))
                            .show(ui)
                            .activated
                        {
                            submit = true;
                        }
                        if Button::secondary("Cancel")
                            .id(Id::unique("settings.parental.forgot.cancel"))
                            .show(ui)
                            .activated
                        {
                            close = true;
                        }
                    },
                );
            });
        if close {
            self.forgot = None;
            return;
        }
        if submit && !busy && wait.is_none() && !f.text.is_empty() {
            let attempt = SecretString::from(std::mem::take(&mut f.text));
            let store = cx.services.store.clone();
            f.check.start_blocking(cx.rt(), &cx.egui, move || {
                let proof = verify_account_secret(&store, &attempt)?;
                if proof == ResetProof::Matched {
                    store.parental().force_remove_pin()?;
                    set_pin_length(&store, None)?;
                }
                Ok(proof)
            });
        }
    }
}

impl SettingsPageUi for ParentalPage {
    fn on_enter(&mut self, cx: &mut AppCtx<'_>) {
        self.lock();
        self.reload(cx);
    }

    fn on_leave(&mut self, _cx: &mut AppCtx<'_>) {
        self.lock();
    }

    fn on_back(&mut self, _cx: &mut AppCtx<'_>) -> bool {
        if self.forgot.is_some() {
            self.forgot = None;
            return true;
        }
        if self.confirm_off {
            self.confirm_off = false;
            return true;
        }
        if self.setup.is_some() && !self.job.is_loading() {
            self.setup = None;
            return true;
        }
        false
    }

    fn ui(&mut self, cx: &mut AppCtx<'_>, ui: &mut Ui) {
        let Some(session) = cx.session() else {
            page_heading(ui, "Parental control", Some("Open a profile first."));
            return;
        };
        if self.loaded_for != Some(session.profile_id()) {
            self.lock();
            self.reload(cx);
        }
        self.data.poll();
        self.poll_job(cx);
        // Behind the PIN (or choosing one) the keypad needs the room, also in TV mode.
        // (The selected section in the list says where we are.)
        let keypad = self.setup.is_some() || self.unlocked.is_none();
        if !keypad {
            page_heading(
                ui,
                "Parental control",
                Some(
                    "Lock adult categories behind a PIN. The PIN and the words apply to every \
                     profile on this computer; category locks belong to this profile.",
                ),
            );
        }
        if self.setup.is_some() {
            self.setup_ui(cx, ui);
            return;
        }
        let Some(mut data) = self.data.value().cloned() else {
            if let Some(e) = self.data.error() {
                Callout::new(CalloutKind::Danger, "Couldn't load:", e.user_message())
                    .width(ui.available_width())
                    .show(ui);
            }
            return;
        };
        if !data.status.is_set {
            self.unlocked = Some(None);
        }
        if self.unlocked.is_none() {
            if self.forgot.is_some() {
                // The pad would swallow digits typed into the dialog's field.
                ui.add_space(24.0);
            } else {
                let gate = self.gate.get_or_insert_with(|| {
                    PinPrompt::new(Id::unique("settings.parental.gate")).with_forgot()
                });
                match gate.show_inline(ui, cx.services, "Enter your PIN to continue") {
                    PinPromptResult::Verified(pin) => {
                        self.unlocked = Some(pin);
                        self.gate = None;
                        // Continue in the page (the keypad disappears).
                        focus::request(&cx.egui, Id::unique("settings.parental.change"));
                    }
                    PinPromptResult::Forgot => {
                        self.forgot = Some(Forgot {
                            text: String::new(),
                            error: None,
                            check: Loader::new(),
                            nothing_to_check: false,
                        });
                    }
                    PinPromptResult::Cancelled | PinPromptResult::Pending => {}
                }
            }
            self.dialogs(cx, ui);
            return;
        }

        self.pin_section(cx, ui, &data);
        ui.add_space(32.0);
        self.keywords_section(cx, ui, &mut data);
        ui.add_space(32.0);
        self.locks_section(cx, ui, &session, &mut data);
        ui.add_space(32.0);
        section(ui, "Good to know", None);
        for (i, (lead, body)) in [
            (
                "What the PIN protects:",
                "locked categories (until you close the app), this page, adding, editing or \
                 deleting profiles, and importing a backup.",
            ),
            (
                "If you forget it:",
                "close this page and choose \u{201c}Forgot your PIN?\u{201d} when it asks for the PIN. \
                 You'll need an account password or playlist address stored on this computer.",
            ),
            (
                "Limits:",
                "after 5 wrong PINs it waits 30 seconds, then longer each time. It stops \
                 guessing at the keyboard; someone who can delete the app's files can always \
                 start over.",
            ),
        ]
        .into_iter()
        .enumerate()
        {
            let w = ui.available_width();
            super::readable(ui, Id::unique(("settings.parental.note", i)), |ui| {
                Callout::new(CalloutKind::Info, lead, body).width(w).show(ui);
            });
            ui.add_space(8.0);
        }
        // Keep local edits (optimistic) until the next reload.
        if let Some(v) = self.data.value_mut() {
            *v = data;
        }
        self.dialogs(cx, ui);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keyword_input_is_normalised() {
        assert_eq!(normalize_keyword("  XXX ").ok().as_deref(), Some("xxx"));
        assert!(normalize_keyword("   ").is_err());
        assert!(normalize_keyword(&"a".repeat(65)).is_err());
        assert!(normalize_keyword("a\u{7}b").is_err());
    }
}
