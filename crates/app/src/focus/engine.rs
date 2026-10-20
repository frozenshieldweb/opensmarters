// SPDX-License-Identifier: GPL-3.0-or-later
//! The focus engine: pure state machine, no egui calls (unit-testable). The egui glue is
//! in [`super`].

use std::collections::HashMap;

use eframe::egui::{Id, Rect};

/// A navigation direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Dir {
    /// Arrow up / D-pad up.
    Up,
    /// Arrow down.
    Down,
    /// Arrow left.
    Left,
    /// Arrow right.
    Right,
}

impl Dir {
    /// The axis this direction moves along.
    pub fn axis(self) -> Axis {
        match self {
            Dir::Up | Dir::Down => Axis::Vertical,
            Dir::Left | Dir::Right => Axis::Horizontal,
        }
    }

    /// `-1` for Up/Left, `+1` for Down/Right.
    pub fn sign(self) -> isize {
        match self {
            Dir::Up | Dir::Left => -1,
            Dir::Down | Dir::Right => 1,
        }
    }
}

/// A layout axis.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Axis {
    /// Left/Right.
    Horizontal,
    /// Up/Down.
    Vertical,
}

/// One focusable widget registered this frame.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Candidate {
    /// Widget id (stable across frames).
    pub id: Id,
    /// Unscaled screen rect (not clipped: off-screen items in scroll areas still count).
    pub rect: Rect,
    /// Innermost focus scope (list, rail, form…).
    pub scope: Id,
    /// Outer region (the side rail vs the screen content).
    pub group: Id,
    /// Preferred initial focus when nothing else is focused.
    pub autofocus: bool,
    /// A text input: Left/Right/Enter/Space go to the text field, not the engine.
    pub text: bool,
    /// Registered inside the trapped (modal) scope, possibly in a nested scope. Set by
    /// [`FocusEngine::register`]; callers pass `false`.
    pub in_modal: bool,
}

/// Keys the engine reacts to this frame (already filtered by the glue).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct NavInput {
    /// Arrow pressed (last one wins).
    pub dir: Option<Dir>,
    /// Enter/Space/OK pressed.
    pub activate: bool,
    /// The pointer moved (hover may move focus).
    pub pointer_moved: bool,
    /// Any key or text input this frame (the user is interacting: a provisional
    /// fallback focus becomes final).
    pub any_input: bool,
}

/// What [`FocusEngine::register`] reports for one widget.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Registered {
    /// The widget has the focus (draw the focus treatment).
    pub focused: bool,
    /// Activated this frame (Enter/Space while focused, or clicked).
    pub activated: bool,
    /// The widget just received focus by keyboard/request: scroll it into view.
    pub scroll_into_view: bool,
}

/// Frames a focused id may be missing (e.g. a virtualised row scrolling in, a screen
/// switching) before the engine falls back to another candidate.
pub const MISSING_GRACE_FRAMES: u32 = 3;

/// The focus state machine. One per app (see [`super`] for the egui glue).
///
/// Per frame: [`begin`](Self::begin) → widgets [`register`](Self::register) → containers
/// may [`take_nav`](Self::take_nav) → [`end`](Self::end) resolves navigation using this
/// frame's geometry. The newly focused widget draws focused on the next frame (a repaint
/// is requested), which is invisible at 60 fps.
#[derive(Debug, Default)]
pub struct FocusEngine {
    current: Option<Id>,
    current_scope: Option<Id>,
    current_group: Option<Id>,
    current_is_text: bool,
    frame: Vec<Candidate>,
    scope_stack: Vec<Id>,
    group_stack: Vec<Id>,
    scope_memory: HashMap<Id, Id>,
    group_memory: HashMap<Id, Id>,
    pending_dir: Option<Dir>,
    pending_activate: bool,
    pointer_moved: bool,
    scroll_to: Option<Id>,
    trap: Option<Id>,
    trap_next: Option<Id>,
    missing_frames: u32,
    changed: bool,
    avoid_group: Option<Id>,
    /// The focus was placed by the fallback and the user has not moved it since: a
    /// better candidate (autofocus, or content instead of the side rail) that appears
    /// later (data finished loading) takes over.
    provisional: bool,
    /// The user pressed a key this frame (see [`FocusEngine::begin`]).
    interacting: bool,
}

impl FocusEngine {
    /// A fresh engine with nothing focused.
    pub fn new() -> Self {
        Self::default()
    }

    /// The group avoided by the fallback (the side rail), so a new screen focuses its
    /// own content first.
    pub fn set_fallback_avoid_group(&mut self, group: Option<Id>) {
        self.avoid_group = group;
    }

    /// Starts a frame.
    pub fn begin(&mut self, input: NavInput) {
        self.frame.clear();
        self.scope_stack.clear();
        self.group_stack.clear();
        self.pending_dir = input.dir;
        self.pending_activate = input.activate;
        self.pointer_moved = input.pointer_moved;
        self.interacting = input.any_input || input.dir.is_some() || input.activate;
        if input.any_input {
            self.provisional = false;
        }
        self.trap_next = None;
        self.changed = false;
    }

    /// The focused id, if any.
    pub fn current(&self) -> Option<Id> {
        self.current
    }

    /// The scope of the focused widget (as of its last registration).
    pub fn current_scope(&self) -> Option<Id> {
        self.current_scope
    }

    /// The group of the focused widget (as of its last registration).
    pub fn current_group(&self) -> Option<Id> {
        self.current_group
    }

    /// True if the focused widget is a text input.
    pub fn text_input_focused(&self) -> bool {
        self.current.is_some() && self.current_is_text
    }

    /// True if the focus changed during the last [`end`](Self::end) or by a request.
    pub fn changed(&self) -> bool {
        self.changed
    }

    /// Focuses `id` now (it should register within [`MISSING_GRACE_FRAMES`]) and scrolls
    /// it into view.
    pub fn request(&mut self, id: Id) {
        if self.current != Some(id) {
            self.changed = true;
        }
        self.current = Some(id);
        self.scroll_to = Some(id);
        self.missing_frames = 0;
        self.pending_activate = false;
        self.provisional = false;
    }

    /// Clears the focus (the fallback will pick something at the end of the frame).
    pub fn clear(&mut self) {
        self.provisional = false;
        self.current = None;
        self.current_scope = None;
        self.current_group = None;
        self.current_is_text = false;
    }

    /// The last focused id inside `scope`, if any.
    pub fn scope_memory(&self, scope: Id) -> Option<Id> {
        self.scope_memory.get(&scope).copied()
    }

    /// Sets the remembered item of `group` (e.g. the side rail remembers the current
    /// section, so moving into it lands there).
    pub fn remember(&mut self, group: Id, id: Id) {
        self.group_memory.insert(group, id);
    }

    /// The last focused id inside `group`, if any.
    pub fn group_memory(&self, group: Id) -> Option<Id> {
        self.group_memory.get(&group).copied()
    }

    /// Pushes a scope for the widgets registered until [`pop_scope`](Self::pop_scope).
    pub fn push_scope(&mut self, scope: Id) {
        self.scope_stack.push(scope);
    }

    /// Pops the innermost scope.
    pub fn pop_scope(&mut self) {
        self.scope_stack.pop();
    }

    /// Pushes a group (region).
    pub fn push_group(&mut self, group: Id) {
        self.group_stack.push(group);
    }

    /// Pops the innermost group.
    pub fn pop_group(&mut self) {
        self.group_stack.pop();
    }

    /// The innermost scope being registered into.
    pub fn active_scope(&self) -> Id {
        self.scope_stack.last().copied().unwrap_or(Id::NULL)
    }

    /// The innermost group being registered into.
    pub fn active_group(&self) -> Id {
        self.group_stack.last().copied().unwrap_or(Id::NULL)
    }

    /// Restricts navigation and the fallback to `scope` (modal dialogs). Call every frame
    /// while the modal is open.
    pub fn trap(&mut self, scope: Id) {
        self.trap_next = Some(scope);
    }

    /// Lets a container (virtualised list, grid) handle a pending arrow along `axis` itself,
    /// if the focus is inside `scope`. The container then calls
    /// [`request`](Self::request) with the new item (or does nothing at a boundary, and
    /// should return the direction with [`give_back_nav`](Self::give_back_nav)).
    pub fn take_nav(&mut self, scope: Id, axis: Axis) -> Option<Dir> {
        let dir = self.pending_dir?;
        if dir.axis() == axis && self.current_scope == Some(scope) && self.current.is_some() {
            self.pending_dir = None;
            Some(dir)
        } else {
            None
        }
    }

    /// Returns a direction taken with [`take_nav`](Self::take_nav) that the container could
    /// not use (e.g. Up on its first row), so geometric navigation handles it.
    pub fn give_back_nav(&mut self, dir: Dir) {
        if self.pending_dir.is_none() {
            self.pending_dir = Some(dir);
        }
    }

    /// The pending arrow, if any (not consumed).
    pub fn pending_dir(&self) -> Option<Dir> {
        self.pending_dir
    }

    /// Consumes the pending activation (Enter) without a widget, e.g. for a screen-level
    /// shortcut. Returns whether one was pending.
    pub fn take_activate(&mut self) -> bool {
        std::mem::take(&mut self.pending_activate)
    }

    /// Registers a candidate. `hovered`/`clicked` come from the widget's egui response.
    pub fn register(&mut self, cand: Candidate, hovered: bool, clicked: bool) -> Registered {
        let mut cand = cand;
        let modal = self.trap_next.or(self.trap);
        cand.in_modal = modal
            .is_some_and(|t| t == cand.scope || t == cand.group || self.scope_stack.contains(&t));
        let allowed = self.trap.is_none() || cand.in_modal;
        if allowed && hovered && self.pointer_moved && self.current != Some(cand.id) {
            self.current = Some(cand.id);
            self.changed = true;
            self.provisional = false;
        }
        let mut out = Registered::default();
        if self.current == Some(cand.id) {
            out.focused = true;
            self.current_scope = Some(cand.scope);
            self.current_group = Some(cand.group);
            self.current_is_text = cand.text;
            self.missing_frames = 0;
            self.scope_memory.insert(cand.scope, cand.id);
            self.group_memory.insert(cand.group, cand.id);
            if self.pending_activate && !cand.text {
                self.pending_activate = false;
                out.activated = true;
            }
            if self.scroll_to == Some(cand.id) {
                self.scroll_to = None;
                out.scroll_into_view = true;
            }
        }
        if clicked && allowed {
            self.provisional = false;
            if self.current != Some(cand.id) {
                self.current = Some(cand.id);
                self.changed = true;
                self.current_scope = Some(cand.scope);
                self.current_group = Some(cand.group);
                self.current_is_text = cand.text;
            }
            out.focused = true;
            // Clicking a text field focuses it; it is not an activation.
            out.activated = !cand.text;
        }
        self.frame.push(cand);
        out
    }

    /// Ends the frame: resolves a pending arrow geometrically, applies focus memory, and
    /// falls back to a sensible candidate when the focused widget disappeared.
    /// Returns true if the focus changed (request a repaint).
    pub fn end(&mut self) -> bool {
        self.trap = self.trap_next.take();
        let mut changed = self.changed;
        let in_trap = |c: &Candidate, trap: Option<Id>| trap.is_none() || c.in_modal;
        let trap = self.trap;

        if let Some(dir) = self.pending_dir.take()
            && let Some(cur) = self.current
            && let Some(src) = self.frame.iter().find(|c| c.id == cur).copied()
        {
            let pool: Vec<&Candidate> = self
                .frame
                .iter()
                .filter(|c| c.id != src.id && in_trap(c, trap))
                .collect();
            self.provisional = false;
            if let Some(target) = self.navigate(&src, dir, &pool)
                && self.current != Some(target)
            {
                self.current = Some(target);
                self.scroll_to = Some(target);
                changed = true;
            }
        }

        // A provisional (fallback) focus yields to a better candidate that appeared.
        if self.provisional
            && let Some(better) = self.fallback(trap)
            && Some(better.id) != self.current
        {
            let cur_avoided = self
                .current
                .and_then(|id| self.frame.iter().find(|c| c.id == id))
                .is_some_and(|c| Some(c.group) == self.avoid_group);
            let cur_auto = self
                .current
                .and_then(|id| self.frame.iter().find(|c| c.id == id))
                .is_some_and(|c| c.autofocus);
            if (better.autofocus && !cur_auto)
                || (cur_avoided && Some(better.group) != self.avoid_group)
            {
                self.current = Some(better.id);
                self.scroll_to = Some(better.id);
                changed = true;
            }
        }

        // Keep the memory up to date and detect a vanished focus.
        let present = self
            .current
            .and_then(|id| self.frame.iter().find(|c| c.id == id).copied());
        match present {
            Some(c) if in_trap(&c, trap) => {
                self.current_scope = Some(c.scope);
                self.current_group = Some(c.group);
                self.current_is_text = c.text;
                self.missing_frames = 0;
            }
            _ => {
                let trapped_out = present.is_some();
                self.missing_frames = self.missing_frames.saturating_add(1);
                if (self.current.is_none()
                    || trapped_out
                    || self.missing_frames > MISSING_GRACE_FRAMES)
                    && let Some(fallback) = self.fallback(trap)
                {
                    // Patience: while the screen's content has nothing focusable yet (still
                    // loading), don't park the focus on the side rail (it would expand over
                    // the page and steal the screen's initial focus); wait for the content,
                    // unless the user presses a key, which then lands on the rail.
                    let only_avoided = trap.is_none()
                        && self.avoid_group.is_some()
                        && Some(fallback.group) == self.avoid_group;
                    if only_avoided && !self.interacting {
                        if self.current.take().is_some() {
                            changed = true;
                        }
                        self.current_scope = None;
                        self.current_group = None;
                        self.current_is_text = false;
                        self.missing_frames = 0;
                    } else {
                        self.current = Some(fallback.id);
                        self.current_scope = Some(fallback.scope);
                        self.current_group = Some(fallback.group);
                        self.current_is_text = fallback.text;
                        self.scroll_to = Some(fallback.id);
                        self.missing_frames = 0;
                        self.provisional = !self.interacting;
                        changed = true;
                    }
                }
            }
        }
        self.pending_activate = false;
        self.changed = changed;
        changed
    }

    fn fallback(&self, trap: Option<Id>) -> Option<Candidate> {
        let pool: Vec<&Candidate> = self
            .frame
            .iter()
            .filter(|c| trap.is_none() || c.in_modal)
            .collect();
        if let Some(a) = pool.iter().find(|c| c.autofocus) {
            return Some(**a);
        }
        let top_left = |cands: &mut dyn Iterator<Item = &&Candidate>| {
            cands
                .min_by(|a, b| {
                    (a.rect.top(), a.rect.left())
                        .partial_cmp(&(b.rect.top(), b.rect.left()))
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
                .map(|c| **c)
        };
        let avoid = self.avoid_group;
        top_left(&mut pool.iter().filter(|c| Some(c.group) != avoid))
            .or_else(|| top_left(&mut pool.iter()))
    }

    fn navigate(&self, src: &Candidate, dir: Dir, pool: &[&Candidate]) -> Option<Id> {
        // 1. Within the same scope.
        let same: Vec<&Candidate> = pool
            .iter()
            .copied()
            .filter(|c| c.scope == src.scope)
            .collect();
        if let Some(best) = best_in_direction(src.rect, dir, same.iter().copied()) {
            return Some(best.id);
        }
        // 2. Across scopes: geometric best, then prefer the target region's memory. Up/Down
        //    never leave the region (the side rail is beside the page: pressing Down at the
        //    bottom of a page must not jump into the rail's bottom items).
        let reachable: Vec<&Candidate> = pool
            .iter()
            .copied()
            .filter(|c| dir.axis() == Axis::Horizontal || c.group == src.group)
            .collect();
        let best = best_in_direction(src.rect, dir, reachable.iter().copied())?;
        let remembered = |mem: Option<Id>| {
            mem.and_then(|id| pool.iter().find(|c| c.id == id))
                .filter(|c| is_in_direction(src.rect, c.rect, dir))
                .map(|c| c.id)
        };
        if best.group != src.group
            && let Some(id) = remembered(self.group_memory.get(&best.group).copied())
        {
            return Some(id);
        }
        if best.scope != src.scope
            && let Some(id) = remembered(self.scope_memory.get(&best.scope).copied())
        {
            return Some(id);
        }
        Some(best.id)
    }
}

/// True if `to` lies in direction `dir` from `from`: its centre is beyond `from`'s
/// centre and its near edge overlaps `from` by at most half of the smaller extent (so a
/// big neighbour that merely overlaps sideways does not count).
pub fn is_in_direction(from: Rect, to: Rect, dir: Dir) -> bool {
    const EPS: f32 = 0.5;
    let tol_x = 0.5 * from.width().min(to.width());
    let tol_y = 0.5 * from.height().min(to.height());
    match dir {
        Dir::Right => to.center().x > from.center().x + EPS && to.left() >= from.right() - tol_x,
        Dir::Left => to.center().x < from.center().x - EPS && to.right() <= from.left() + tol_x,
        Dir::Down => to.center().y > from.center().y + EPS && to.top() >= from.bottom() - tol_y,
        Dir::Up => to.center().y < from.center().y - EPS && to.bottom() <= from.top() + tol_y,
    }
}

/// Distance score of moving from `from` to `to` in `dir` (lower is better), or `None` if
/// `to` is not in that direction. Edge gap along the axis, plus twice the orthogonal gap
/// (zero when the two overlap on the orthogonal axis, i.e. `to` is "in the beam"), plus a
/// small centre-alignment tiebreak.
pub fn score(from: Rect, to: Rect, dir: Dir) -> Option<f32> {
    if !is_in_direction(from, to, dir) {
        return None;
    }
    let (major, (a0, a1), (b0, b1), (ca, cb)) = match dir {
        Dir::Right => (
            to.left() - from.right(),
            (from.top(), from.bottom()),
            (to.top(), to.bottom()),
            (from.center().y, to.center().y),
        ),
        Dir::Left => (
            from.left() - to.right(),
            (from.top(), from.bottom()),
            (to.top(), to.bottom()),
            (from.center().y, to.center().y),
        ),
        Dir::Down => (
            to.top() - from.bottom(),
            (from.left(), from.right()),
            (to.left(), to.right()),
            (from.center().x, to.center().x),
        ),
        Dir::Up => (
            from.top() - to.bottom(),
            (from.left(), from.right()),
            (to.left(), to.right()),
            (from.center().x, to.center().x),
        ),
    };
    let major = major.max(0.0);
    let minor_gap = if b0 > a1 {
        b0 - a1
    } else if a0 > b1 {
        a0 - b1
    } else {
        0.0
    };
    Some(major + 2.0 * minor_gap + 0.05 * (ca - cb).abs())
}

/// The best candidate in `dir` from `from` (see [`score`]). Ties keep the earlier
/// candidate (registration order = reading order).
pub fn best_in_direction<'a>(
    from: Rect,
    dir: Dir,
    candidates: impl Iterator<Item = &'a Candidate>,
) -> Option<&'a Candidate> {
    let mut best: Option<(&Candidate, f32)> = None;
    for c in candidates {
        if let Some(s) = score(from, c.rect, dir)
            && best.is_none_or(|(_, b)| s < b)
        {
            best = Some((c, s));
        }
    }
    best.map(|(c, _)| c)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, deprecated)]
mod tests {
    use super::*;
    use eframe::egui::{pos2, vec2};

    fn r(x: f32, y: f32, w: f32, h: f32) -> Rect {
        Rect::from_min_size(pos2(x, y), vec2(w, h))
    }

    fn cand(name: &str, rect: Rect, scope: &str) -> Candidate {
        Candidate {
            id: Id::unique(name),
            rect,
            scope: Id::unique(scope),
            group: Id::unique("content"),
            autofocus: false,
            text: false,
            in_modal: false,
        }
    }

    #[test]
    fn grid_prefers_aligned_neighbour() {
        // 3x2 grid of 100x150 posters, 16 pt gaps.
        let mut cs = Vec::new();
        for row in 0..2 {
            for col in 0..3 {
                cs.push(cand(
                    &format!("{row}-{col}"),
                    r(col as f32 * 116.0, row as f32 * 166.0, 100.0, 150.0),
                    "grid",
                ));
            }
        }
        let from = cs[1].rect; // 0-1
        let down = best_in_direction(from, Dir::Down, cs.iter()).unwrap();
        assert_eq!(down.id, Id::unique("1-1"));
        let right = best_in_direction(from, Dir::Right, cs.iter()).unwrap();
        assert_eq!(right.id, Id::unique("0-2"));
        let left = best_in_direction(from, Dir::Left, cs.iter()).unwrap();
        assert_eq!(left.id, Id::unique("0-0"));
        assert!(best_in_direction(from, Dir::Up, cs.iter()).is_none());
    }

    #[test]
    fn beam_beats_closer_diagonal() {
        let from = r(0.0, 0.0, 100.0, 40.0);
        let in_beam_far = cand("far", r(400.0, 10.0, 100.0, 40.0), "s");
        let diagonal_near = cand("diag", r(120.0, 200.0, 100.0, 40.0), "s");
        let cs = [diagonal_near, in_beam_far];
        assert_eq!(
            best_in_direction(from, Dir::Right, cs.iter()).unwrap().id,
            Id::unique("far")
        );
    }

    #[test]
    fn sideways_overlap_is_not_up() {
        // A rail item that overlaps a search field vertically is not "above" it.
        let field = r(256.0, 106.0, 200.0, 52.0);
        let rail_item = r(12.0, 87.0, 208.0, 48.0);
        assert!(!is_in_direction(field, rail_item, Dir::Up));
        assert!(is_in_direction(field, rail_item, Dir::Left));
    }

    #[test]
    fn overlapping_items_are_not_in_direction() {
        let from = r(0.0, 0.0, 100.0, 100.0);
        assert!(!is_in_direction(
            from,
            r(10.0, 10.0, 50.0, 50.0),
            Dir::Right
        ));
        assert!(is_in_direction(
            from,
            r(60.0, 0.0, 100.0, 100.0),
            Dir::Right
        ));
        assert!(!is_in_direction(
            from,
            r(60.0, 0.0, 100.0, 100.0),
            Dir::Left
        ));
    }

    fn frame(e: &mut FocusEngine, input: NavInput, cands: &[Candidate]) -> Vec<Registered> {
        e.begin(input);
        let out = cands.iter().map(|c| e.register(*c, false, false)).collect();
        e.end();
        out
    }

    #[test]
    fn fallback_prefers_autofocus_then_top_left_outside_avoided_group() {
        let mut e = FocusEngine::new();
        let rail = Candidate {
            group: Id::unique("rail"),
            ..cand("nav-home", r(0.0, 0.0, 200.0, 48.0), "rail")
        };
        let a = cand("a", r(300.0, 100.0, 100.0, 40.0), "body");
        let b = cand("b", r(300.0, 20.0, 100.0, 40.0), "body");
        e.set_fallback_avoid_group(Some(Id::unique("rail")));
        frame(&mut e, NavInput::default(), &[rail, a, b]);
        assert_eq!(e.current(), Some(Id::unique("b")));

        let mut e = FocusEngine::new();
        let auto = Candidate {
            autofocus: true,
            ..cand("auto", r(300.0, 500.0, 100.0, 40.0), "body")
        };
        frame(&mut e, NavInput::default(), &[a, b, auto]);
        assert_eq!(e.current(), Some(Id::unique("auto")));
    }

    #[test]
    fn provisional_fallback_yields_to_content_that_loads_later() {
        let mut e = FocusEngine::new();
        e.set_fallback_avoid_group(Some(Id::new("rail")));
        let rail = Candidate {
            group: Id::new("rail"),
            ..cand("nav-search", r(0.0, 0.0, 200.0, 48.0), "rail")
        };
        // Content still loading: only the rail is focusable. The engine waits instead of
        // parking the focus on the rail (which would expand over the page).
        frame(&mut e, NavInput::default(), &[rail]);
        assert_eq!(e.current(), None);
        // Content appears: focus moves into it.
        let card = cand("card", r(300.0, 500.0, 100.0, 150.0), "rail-movies");
        frame(&mut e, NavInput::default(), &[rail, card]);
        assert_eq!(e.current(), Some(card.id));
        // The hero's autofocus button appears later still: it wins.
        let play = Candidate {
            autofocus: true,
            ..cand("play", r(300.0, 400.0, 100.0, 44.0), "hero")
        };
        frame(&mut e, NavInput::default(), &[rail, card, play]);
        assert_eq!(e.current(), Some(play.id));
        // Once the user navigates, nothing moves the focus by itself.
        frame(
            &mut e,
            NavInput {
                dir: Some(Dir::Down),
                ..Default::default()
            },
            &[rail, card, play],
        );
        assert_eq!(e.current(), Some(card.id));
        let late = Candidate {
            autofocus: true,
            ..cand("late", r(300.0, 50.0, 100.0, 44.0), "x")
        };
        frame(&mut e, NavInput::default(), &[rail, card, play, late]);
        assert_eq!(e.current(), Some(card.id));
    }

    #[test]
    fn rail_only_screen_waits_until_a_key_is_pressed() {
        let mut e = FocusEngine::new();
        e.set_fallback_avoid_group(Some(Id::new("rail")));
        let rail = Candidate {
            group: Id::new("rail"),
            ..cand("nav-search", r(0.0, 0.0, 200.0, 48.0), "rail")
        };
        for _ in 0..10 {
            frame(&mut e, NavInput::default(), &[rail]);
            assert_eq!(e.current(), None);
        }
        // Any key: the user wants to do something, so the rail gets the focus (for good).
        frame(
            &mut e,
            NavInput {
                dir: Some(Dir::Down),
                any_input: true,
                ..Default::default()
            },
            &[rail],
        );
        assert_eq!(e.current(), Some(rail.id));
        let play = Candidate {
            autofocus: true,
            ..cand("play", r(300.0, 400.0, 100.0, 44.0), "hero")
        };
        frame(&mut e, NavInput::default(), &[rail, play]);
        assert_eq!(
            e.current(),
            Some(rail.id),
            "a chosen rail focus is not stolen"
        );
    }

    #[test]
    fn up_down_never_jump_between_rail_and_page() {
        let mut e = FocusEngine::new();
        let rail_settings = Candidate {
            group: Id::new("rail"),
            ..cand("nav-settings", r(0.0, 900.0, 200.0, 48.0), "rail")
        };
        let rail_home = Candidate {
            group: Id::new("rail"),
            ..cand("nav-home", r(0.0, 100.0, 200.0, 48.0), "rail")
        };
        let last = Candidate {
            autofocus: true,
            ..cand("last-button", r(300.0, 600.0, 120.0, 44.0), "page")
        };
        let cands = [rail_home, rail_settings, last];
        frame(&mut e, NavInput::default(), &cands);
        assert_eq!(e.current(), Some(last.id));
        frame(
            &mut e,
            NavInput {
                dir: Some(Dir::Down),
                ..Default::default()
            },
            &cands,
        );
        assert_eq!(e.current(), Some(last.id), "Down at the bottom stays put");
        frame(
            &mut e,
            NavInput {
                dir: Some(Dir::Left),
                ..Default::default()
            },
            &cands,
        );
        assert!(
            e.current() == Some(rail_home.id) || e.current() == Some(rail_settings.id),
            "Left still reaches the rail"
        );
    }

    #[test]
    fn hero_autofocus_wins_over_rail_when_it_loads_last() {
        let mut e = FocusEngine::new();
        e.set_fallback_avoid_group(Some(Id::new("rail")));
        let rail = Candidate {
            group: Id::new("rail"),
            ..cand("nav-search", r(0.0, 0.0, 200.0, 48.0), "rail")
        };
        let play = Candidate {
            autofocus: true,
            ..cand("play", r(300.0, 400.0, 100.0, 44.0), "hero")
        };
        for _ in 0..5 {
            frame(&mut e, NavInput::default(), &[rail]);
        }
        frame(&mut e, NavInput::default(), &[rail, play]);
        assert_eq!(e.current(), Some(play.id));
    }

    #[test]
    fn arrow_moves_focus_and_activation_reaches_focused_widget() {
        let mut e = FocusEngine::new();
        let a = cand("a", r(0.0, 0.0, 100.0, 40.0), "s");
        let b = cand("b", r(0.0, 60.0, 100.0, 40.0), "s");
        frame(&mut e, NavInput::default(), &[a, b]);
        assert_eq!(e.current(), Some(a.id));
        frame(
            &mut e,
            NavInput {
                dir: Some(Dir::Down),
                ..Default::default()
            },
            &[a, b],
        );
        assert_eq!(e.current(), Some(b.id));
        let regs = frame(
            &mut e,
            NavInput {
                activate: true,
                ..Default::default()
            },
            &[a, b],
        );
        assert!(!regs[0].activated);
        assert!(regs[1].activated && regs[1].focused);
    }

    #[test]
    fn crossing_scopes_restores_scope_memory() {
        let mut e = FocusEngine::new();
        // Two horizontal rails; the lower one remembers its 3rd card.
        let top: Vec<Candidate> = (0..4)
            .map(|i| {
                cand(
                    &format!("t{i}"),
                    r(i as f32 * 120.0, 0.0, 100.0, 60.0),
                    "rail-top",
                )
            })
            .collect();
        let bottom: Vec<Candidate> = (0..4)
            .map(|i| {
                cand(
                    &format!("b{i}"),
                    r(i as f32 * 120.0, 100.0, 100.0, 60.0),
                    "rail-bot",
                )
            })
            .collect();
        let all: Vec<Candidate> = top.iter().chain(bottom.iter()).copied().collect();
        e.request(Id::unique("b2"));
        frame(&mut e, NavInput::default(), &all);
        e.request(Id::unique("t0"));
        frame(&mut e, NavInput::default(), &all);
        frame(
            &mut e,
            NavInput {
                dir: Some(Dir::Down),
                ..Default::default()
            },
            &all,
        );
        assert_eq!(e.current(), Some(Id::unique("b2")), "rail focus memory");
    }

    #[test]
    fn crossing_groups_restores_group_memory() {
        let mut e = FocusEngine::new();
        let rail_item = Candidate {
            group: Id::unique("rail"),
            ..cand("nav", r(0.0, 0.0, 200.0, 48.0), "nav")
        };
        let near = cand("near", r(250.0, 0.0, 100.0, 48.0), "cats");
        let far = cand("far", r(400.0, 300.0, 100.0, 48.0), "list");
        let all = [rail_item, near, far];
        e.request(far.id);
        frame(&mut e, NavInput::default(), &all);
        e.request(rail_item.id);
        frame(&mut e, NavInput::default(), &all);
        frame(
            &mut e,
            NavInput {
                dir: Some(Dir::Right),
                ..Default::default()
            },
            &all,
        );
        assert_eq!(
            e.current(),
            Some(far.id),
            "returns to the content's last focus"
        );
    }

    #[test]
    fn take_nav_lets_lists_handle_their_own_axis() {
        let mut e = FocusEngine::new();
        let a = cand("row-0", r(0.0, 0.0, 100.0, 40.0), "list");
        frame(&mut e, NavInput::default(), &[a]);
        e.begin(NavInput {
            dir: Some(Dir::Down),
            ..Default::default()
        });
        assert_eq!(e.take_nav(Id::unique("other"), Axis::Vertical), None);
        assert_eq!(e.take_nav(Id::unique("list"), Axis::Horizontal), None);
        assert_eq!(
            e.take_nav(Id::unique("list"), Axis::Vertical),
            Some(Dir::Down)
        );
        assert_eq!(e.pending_dir(), None);
        e.give_back_nav(Dir::Down);
        assert_eq!(e.pending_dir(), Some(Dir::Down));
        e.register(a, false, false);
        e.end();
    }

    #[test]
    fn missing_focus_survives_grace_frames_then_falls_back() {
        let mut e = FocusEngine::new();
        let a = cand("a", r(0.0, 0.0, 100.0, 40.0), "s");
        frame(&mut e, NavInput::default(), &[a]);
        e.request(Id::unique("offscreen-row"));
        for _ in 0..MISSING_GRACE_FRAMES {
            frame(&mut e, NavInput::default(), &[a]);
            assert_eq!(e.current(), Some(Id::unique("offscreen-row")));
        }
        frame(&mut e, NavInput::default(), &[a]);
        assert_eq!(e.current(), Some(a.id));
    }

    #[test]
    fn trap_confines_navigation_and_fallback() {
        let mut e = FocusEngine::new();
        let page = cand("page", r(0.0, 0.0, 100.0, 40.0), "page");
        let ok = cand("ok", r(0.0, 200.0, 100.0, 40.0), "dialog");
        let cancel = cand("cancel", r(120.0, 200.0, 100.0, 40.0), "dialog");
        e.request(page.id);
        frame(&mut e, NavInput::default(), &[page, ok, cancel]);
        // Dialog opens: trap it. The focus moves into the dialog.
        e.begin(NavInput::default());
        e.trap(Id::unique("dialog"));
        for c in [page, ok, cancel] {
            e.register(c, false, false);
        }
        e.end();
        assert_eq!(e.current(), Some(ok.id));
        // Up would leave the dialog: blocked.
        e.begin(NavInput {
            dir: Some(Dir::Up),
            ..Default::default()
        });
        e.trap(Id::unique("dialog"));
        for c in [page, ok, cancel] {
            e.register(c, false, false);
        }
        e.end();
        assert_eq!(e.current(), Some(ok.id));
    }

    #[test]
    fn trap_includes_nested_scopes() {
        let mut e = FocusEngine::new();
        let page = cand("page", r(0.0, 0.0, 100.0, 40.0), "page");
        let key = cand("key-1", r(0.0, 200.0, 72.0, 72.0), "dialog.keys");
        let cancel = cand("cancel", r(0.0, 300.0, 100.0, 40.0), "dialog");
        e.request(page.id);
        for _ in 0..2 {
            e.begin(NavInput::default());
            e.trap(Id::new("dialog"));
            e.register(page, false, false);
            e.push_scope(Id::new("dialog"));
            e.push_scope(Id::new("dialog.keys"));
            e.register(key, false, false);
            e.pop_scope();
            e.register(cancel, false, false);
            e.pop_scope();
            e.end();
        }
        assert_eq!(
            e.current(),
            Some(key.id),
            "the keypad inside the dialog is reachable"
        );
    }

    #[test]
    fn hover_moves_focus_only_when_pointer_moved() {
        let mut e = FocusEngine::new();
        let a = cand("a", r(0.0, 0.0, 100.0, 40.0), "s");
        let b = cand("b", r(0.0, 60.0, 100.0, 40.0), "s");
        frame(&mut e, NavInput::default(), &[a, b]);
        e.begin(NavInput::default());
        e.register(a, false, false);
        let rb = e.register(b, true, false);
        e.end();
        assert!(!rb.focused, "a resting pointer must not steal focus");
        e.begin(NavInput {
            pointer_moved: true,
            ..Default::default()
        });
        e.register(a, false, false);
        let rb = e.register(b, true, false);
        e.end();
        assert!(rb.focused);
        assert_eq!(e.current(), Some(b.id));
    }
}
