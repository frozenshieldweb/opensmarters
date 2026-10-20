// SPDX-License-Identifier: GPL-3.0-or-later
//! Spatial focus navigation for keyboard, remote and mouse (`docs/DESIGN.md` §5).
//!
//! There is exactly **one** focus in the app. Arrow keys move it to the geometric nearest
//! neighbour (first inside the current *scope* — a list, rail or form — then across
//! scopes, restoring each scope's and region's last focused item). Enter/Space activate
//! the focused widget; hovering with a moving pointer moves the focus; clicking focuses
//! and activates.
//!
//! ## Using it in a widget (one call)
//!
//! ```ignore
//! let response = ui.allocate_response(size, egui::Sense::CLICK);
//! let f = focus::register(ui, &response, FocusOpts::default());
//! // paint: f.focused (ring/glow now), f.anim (0..1 for scale/glow animation)
//! if f.activated { /* do the thing */ }
//! ```
//!
//! Use `Sense::CLICK` (not `Sense::click()`): our widgets must not be egui-focusable,
//! otherwise egui's own arrow-key navigation would fight this engine.
//!
//! ## Scopes, groups, memory
//!
//! ```ignore
//! focus::scope(ui, Id::unique("home.rail.continue"), |ui| { /* cards */ });
//! ```
//! Each widget registers into the innermost scope. Moving into a scope restores the item
//! last focused there (rails remember their position). The shell's side rail and the
//! screen content are two *groups*; moving from the rail back into content restores the
//! content's last focus.
//!
//! ## Virtualised lists
//!
//! A container that only renders visible rows handles its own axis:
//! [`take_nav`] returns the pending arrow if the focus is inside the scope; the container
//! [`request`]s the next row's id (and scrolls), or calls [`give_back_nav`] at a boundary.
//! [`crate::ui::widgets::list::VirtualList`] does this for you.
//!
//! ## Modals
//!
//! A dialog calls [`trap`] every frame with its scope: navigation and the fallback stay
//! inside it. [`crate::ui::widgets::dialog`] does this.
//!
//! ## Keys
//!
//! The engine consumes arrows, Enter/Space (unless a text field is focused), Tab /
//! Shift+Tab (as Down/Up). Esc and Backspace (not in a text field) are reported to the app
//! as *back* by [`begin_frame`]. Screens that want other keys (digits, PageUp…) read them
//! with `ctx.input_mut(|i| i.consume_key(..))` in [`crate::screens::Screen::on_input`],
//! which runs before the engine.

pub mod engine;

use std::sync::{Arc, Mutex, PoisonError};

use eframe::egui::{self, Context, Id, Key, Modifiers, Rect, Response, Ui, emath::easing};

pub use engine::{Axis, Candidate, Dir, FocusEngine, NavInput, Registered};

/// The side rail's focus group (avoided by the fallback so screens focus their content).
pub fn shell_group() -> Id {
    Id::unique("focus.group.shell")
}

/// The screen content's focus group.
pub fn content_group() -> Id {
    Id::unique("focus.group.content")
}

type Shared = Arc<Mutex<FocusEngine>>;

fn key() -> Id {
    Id::unique("opensmarters.focus.engine")
}

fn shared(ctx: &Context) -> Shared {
    ctx.data_mut(|d| {
        d.get_temp_mut_or_insert_with(key(), || {
            let mut e = FocusEngine::new();
            e.set_fallback_avoid_group(Some(shell_group()));
            Arc::new(Mutex::new(e))
        })
        .clone()
    })
}

/// Runs `f` with the engine locked. Do not call egui functions that re-enter the focus
/// module inside `f`.
pub fn with<R>(ctx: &Context, f: impl FnOnce(&mut FocusEngine) -> R) -> R {
    let s = shared(ctx);
    let mut guard = s.lock().unwrap_or_else(PoisonError::into_inner);
    f(&mut guard)
}

fn pointer_key() -> Id {
    Id::unique("opensmarters.focus.pointer_enabled")
}

/// Enables or disables pointer-hover focus (the harness disables it so a resting mouse
/// cursor never moves the focus; the app also ignores the pointer for the first frames,
/// when the window appears under it).
pub fn set_pointer_enabled(ctx: &Context, on: bool) {
    ctx.data_mut(|d| d.insert_temp(pointer_key(), on));
}

fn pointer_enabled(ctx: &Context) -> bool {
    ctx.data(|d| d.get_temp::<bool>(pointer_key()))
        .unwrap_or(true)
}

/// What the app needs to know from this frame's keys.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FrameKeys {
    /// Esc, or Backspace outside a text field.
    pub back: bool,
}

/// Starts a frame: reads and consumes navigation keys. Called by the app after the
/// screen's `on_input` (so screens can claim keys first).
pub fn begin_frame(ctx: &Context) -> FrameKeys {
    let text = with(ctx, |e| e.text_input_focused());
    let pointer_on = pointer_enabled(ctx);
    let (input, back) = ctx.input_mut(|i| {
        let mut dir = None;
        for (k, d) in [
            (Key::ArrowUp, Dir::Up),
            (Key::ArrowDown, Dir::Down),
            (Key::ArrowLeft, Dir::Left),
            (Key::ArrowRight, Dir::Right),
        ] {
            if text && d.axis() == Axis::Horizontal {
                continue;
            }
            if i.consume_key(Modifiers::NONE, k) {
                dir = Some(d);
            }
        }
        if i.consume_key(Modifiers::NONE, Key::Tab) {
            dir = Some(Dir::Down);
        }
        if i.consume_key(Modifiers::SHIFT, Key::Tab) {
            dir = Some(Dir::Up);
        }
        let activate = !text
            && (i.consume_key(Modifiers::NONE, Key::Enter)
                | i.consume_key(Modifiers::NONE, Key::Space));
        let back = i.consume_key(Modifiers::NONE, Key::Escape)
            | (!text && i.consume_key(Modifiers::NONE, Key::Backspace));
        let any_input = i.events.iter().any(|e| {
            matches!(
                e,
                egui::Event::Key { pressed: true, .. }
                    | egui::Event::Text(_)
                    | egui::Event::Paste(_)
            )
        });
        let pointer_moved = i.pointer.delta() != egui::Vec2::ZERO
            || i.pointer.any_pressed()
            || i.smooth_scroll_delta != egui::Vec2::ZERO;
        (
            NavInput {
                dir,
                activate,
                pointer_moved: pointer_on && pointer_moved && i.pointer.has_pointer(),
                any_input,
            },
            back,
        )
    });
    with(ctx, |e| e.begin(input));
    FrameKeys { back }
}

/// Ends a frame (resolves navigation). Requests a repaint if the focus moved.
pub fn end_frame(ctx: &Context) {
    if with(ctx, FocusEngine::end) {
        ctx.request_repaint();
    }
}

/// Options for [`register`].
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct FocusOpts {
    /// Override the scope (default: the innermost [`scope`]).
    pub scope: Option<Id>,
    /// Preferred initial focus of the screen.
    pub autofocus: bool,
    /// A text input (keeps Left/Right/Enter/Space for editing).
    pub text: bool,
    /// Extra margin (x, y) when scrolling into view (default 16 pt each way).
    pub scroll_margin: Option<egui::Vec2>,
    /// Geometry to use instead of the response rect (e.g. the unscaled card rect).
    pub rect: Option<Rect>,
}

impl FocusOpts {
    /// Marks the widget as the screen's preferred initial focus.
    #[must_use]
    pub fn autofocus(mut self) -> Self {
        self.autofocus = true;
        self
    }

    /// Conditionally marks autofocus.
    #[must_use]
    pub fn autofocus_if(mut self, on: bool) -> Self {
        self.autofocus |= on;
        self
    }

    /// Marks a text input.
    #[must_use]
    pub fn text(mut self) -> Self {
        self.text = true;
        self
    }

    /// Registers into `scope` instead of the innermost one.
    #[must_use]
    pub fn in_scope(mut self, scope: Id) -> Self {
        self.scope = Some(scope);
        self
    }

    /// Scroll-into-view margin (same on both axes).
    #[must_use]
    pub fn scroll_margin(mut self, margin: f32) -> Self {
        self.scroll_margin = Some(egui::Vec2::splat(margin));
        self
    }

    /// Scroll-into-view margin per axis (rails use a wide x margin for look-ahead).
    #[must_use]
    pub fn scroll_margin2(mut self, margin: egui::Vec2) -> Self {
        self.scroll_margin = Some(margin);
        self
    }
}

/// The focus state of one widget this frame.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Focus {
    /// Widget id.
    pub id: Id,
    /// Focused now: draw the ring, glow, text colour.
    pub focused: bool,
    /// Activated this frame (Enter/Space/click).
    pub activated: bool,
    /// Animated focus amount 0..1 (160 ms cubic-out; instant with reduced motion). Use it
    /// for the 1.06 scale and the glow; draw the ring from `focused`.
    pub anim: f32,
}

/// Registers the widget behind `response` as focusable. Call once per frame, after
/// allocating it. See the [module docs](self).
pub fn register(ui: &Ui, response: &Response, opts: FocusOpts) -> Focus {
    let ctx = ui.ctx();
    let rect = opts.rect.unwrap_or(response.rect);
    let reg = with(ctx, |e| {
        let scope = opts.scope.unwrap_or_else(|| e.active_scope());
        let group = e.active_group();
        e.register(
            Candidate {
                id: response.id,
                rect,
                scope,
                group,
                autofocus: opts.autofocus,
                text: opts.text,
                in_modal: false,
            },
            // Text fields take the focus on click or keyboard only: a pointer passing over a
            // field must not steal typing focus.
            response.hovered() && !opts.text,
            response.clicked(),
        )
    });
    if reg.scroll_into_view {
        let target = rect.expand2(opts.scroll_margin.unwrap_or(egui::Vec2::splat(16.0)));
        ui.scroll_to_rect(target, None);
        let pass = ctx.cumulative_pass_nr();
        ctx.data_mut(|d| d.insert_temp(scroll_request_key(), (pass, target)));
    }
    Focus {
        id: response.id,
        focused: reg.focused,
        activated: reg.activated,
        anim: anim(ctx, response.id, reg.focused),
    }
}

/// Allocates `rect` (click sense, not egui-focusable) under `id` and registers it.
pub fn focusable(ui: &mut Ui, id: Id, rect: Rect, opts: FocusOpts) -> (Response, Focus) {
    let response = ui.interact(rect, id, egui::Sense::CLICK);
    let f = register(ui, &response, opts);
    (response, f)
}

fn scroll_request_key() -> Id {
    Id::unique("opensmarters.focus.scroll_request")
}

/// Re-issues this frame's scroll-into-view request to the scroll areas *around* a nested
/// one. egui's inner `ScrollArea` consumes both axes of a scroll target, so a container
/// with its own scroll area (a horizontal rail inside a vertical page) calls this right
/// after its scroll area, from its parent `ui`. [`crate::ui::widgets::Rail`] and
/// [`crate::ui::widgets::VirtualList`] already do.
pub fn forward_scroll(ui: &Ui) {
    let ctx = ui.ctx();
    let pass = ctx.cumulative_pass_nr();
    if let Some((p, rect)) = ctx.data(|d| d.get_temp::<(u64, Rect)>(scroll_request_key()))
        && p == pass
    {
        ui.scroll_to_rect(rect, None);
    }
}

/// The animated 0..1 focus amount for `id` (reduced motion: instant).
pub fn anim(ctx: &Context, id: Id, focused: bool) -> f32 {
    let secs = if crate::ui::motion::reduced(ctx) {
        0.0
    } else {
        crate::ui::theme::motion::BASE
    };
    ctx.animate_bool_with_time_and_easing(id.with("focus.anim"), focused, secs, easing::cubic_out)
}

/// Runs `add` with widgets registering into `scope`.
pub fn scope<R>(ui: &mut Ui, scope: Id, add: impl FnOnce(&mut Ui) -> R) -> R {
    with(ui.ctx(), |e| e.push_scope(scope));
    let r = add(ui);
    with(ui.ctx(), FocusEngine::pop_scope);
    r
}

/// Runs `add` with widgets registering into `group` (a region: side rail, content).
pub fn group<R>(ui: &mut Ui, group: Id, add: impl FnOnce(&mut Ui) -> R) -> R {
    with(ui.ctx(), |e| e.push_group(group));
    let r = add(ui);
    with(ui.ctx(), FocusEngine::pop_group);
    r
}

/// Focuses `id` now and scrolls it into view when it registers.
pub fn request(ctx: &Context, id: Id) {
    with(ctx, |e| e.request(id));
    ctx.request_repaint();
}

/// The focused widget's id.
pub fn current(ctx: &Context) -> Option<Id> {
    with(ctx, |e| e.current())
}

/// True if `id` has the focus.
pub fn is_focused(ctx: &Context, id: Id) -> bool {
    current(ctx) == Some(id)
}

/// True if the focused widget is inside `scope`.
pub fn is_in_scope(ctx: &Context, scope: Id) -> bool {
    with(ctx, |e| e.current_scope() == Some(scope))
}

/// True if the focused widget is in `group`.
pub fn is_in_group(ctx: &Context, group: Id) -> bool {
    with(ctx, |e| e.current_group() == Some(group))
}

/// The last focused item of `scope`.
pub fn scope_memory(ctx: &Context, scope: Id) -> Option<Id> {
    with(ctx, |e| e.scope_memory(scope))
}

/// The last focused item of `group`.
pub fn group_memory(ctx: &Context, group: Id) -> Option<Id> {
    with(ctx, |e| e.group_memory(group))
}

/// See [`FocusEngine::take_nav`].
pub fn take_nav(ctx: &Context, scope: Id, axis: Axis) -> Option<Dir> {
    with(ctx, |e| e.take_nav(scope, axis))
}

/// See [`FocusEngine::give_back_nav`].
pub fn give_back_nav(ctx: &Context, dir: Dir) {
    with(ctx, |e| e.give_back_nav(dir));
}

/// The pending (unconsumed) arrow of this frame.
pub fn pending_dir(ctx: &Context) -> Option<Dir> {
    with(ctx, |e| e.pending_dir())
}

/// Restricts navigation to `scope` this frame (modal dialogs).
pub fn trap(ctx: &Context, scope: Id) {
    with(ctx, |e| e.trap(scope));
}

/// True if a text field has the focus (typing keys belong to it).
pub fn text_input_focused(ctx: &Context) -> bool {
    with(ctx, |e| e.text_input_focused())
}

/// Clears the focus; the engine picks a fallback at the end of the frame.
pub fn clear(ctx: &Context) {
    with(ctx, FocusEngine::clear);
}
