// SPDX-License-Identifier: GPL-3.0-or-later
//! The back stack. Generic over the screen payload so the stack rules are unit-tested
//! without egui.

use eframe::egui::Id;

use super::route::Route;

/// A navigation request from a screen (queued in [`Navigator`], applied after the frame).
#[derive(Debug, Clone)]
pub enum NavCommand {
    /// Open a route on top of the current one (no-op if it is already on top).
    Push(Route),
    /// Replace the current route.
    Replace(Route),
    /// Go back one step (Esc/Backspace). No-op on the root.
    Back,
    /// Clear the stack and open a route (profile switched, signed in…).
    Reset(Route),
    /// Switch side-rail section: the stack becomes `[Home, route]` (or `[Home]`).
    Section(Route),
}

/// The command queue screens write to (`cx.nav`).
#[derive(Debug, Default)]
pub struct Navigator {
    commands: Vec<NavCommand>,
}

impl Navigator {
    /// Pushes `route`.
    pub fn push(&mut self, route: Route) {
        self.commands.push(NavCommand::Push(route));
    }

    /// Replaces the current route.
    pub fn replace(&mut self, route: Route) {
        self.commands.push(NavCommand::Replace(route));
    }

    /// Goes back.
    pub fn back(&mut self) {
        self.commands.push(NavCommand::Back);
    }

    /// Resets the stack to `route`.
    pub fn reset(&mut self, route: Route) {
        self.commands.push(NavCommand::Reset(route));
    }

    /// Switches to a side-rail section.
    pub fn section(&mut self, route: Route) {
        self.commands.push(NavCommand::Section(route));
    }

    /// Takes the queued commands.
    pub fn drain(&mut self) -> Vec<NavCommand> {
        std::mem::take(&mut self.commands)
    }

    /// True if nothing is queued.
    pub fn is_empty(&self) -> bool {
        self.commands.is_empty()
    }
}

/// One stack entry.
#[derive(Debug)]
pub struct Entry<S> {
    /// The route.
    pub route: Route,
    /// The screen instance (keeps its state while covered).
    pub screen: S,
    /// Focus to restore when the entry becomes the top again.
    pub saved_focus: Option<Id>,
}

/// What [`Router::apply`] changed; the app runs the lifecycle hooks from it.
#[derive(Debug)]
pub struct Change<S> {
    /// Entries removed from the stack (call `on_leave` then drop).
    pub removed: Vec<Entry<S>>,
    /// The previous top is still in the stack but covered (call `on_leave`, save focus).
    pub covered_previous: bool,
    /// The top entry changed (call `on_enter` on the new top, restore its focus).
    pub top_changed: bool,
}

impl<S> Default for Change<S> {
    fn default() -> Self {
        Self {
            removed: Vec::new(),
            covered_previous: false,
            top_changed: false,
        }
    }
}

/// The back stack of screens.
#[derive(Debug)]
pub struct Router<S> {
    stack: Vec<Entry<S>>,
}

impl<S> Default for Router<S> {
    fn default() -> Self {
        Self { stack: Vec::new() }
    }
}

impl<S> Router<S> {
    /// An empty router.
    pub fn new() -> Self {
        Self::default()
    }

    /// The top entry.
    pub fn top(&self) -> Option<&Entry<S>> {
        self.stack.last()
    }

    /// The top entry, mutably.
    pub fn top_mut(&mut self) -> Option<&mut Entry<S>> {
        self.stack.last_mut()
    }

    /// The routes from bottom to top.
    pub fn routes(&self) -> impl Iterator<Item = &Route> {
        self.stack.iter().map(|e| &e.route)
    }

    /// Stack depth.
    pub fn len(&self) -> usize {
        self.stack.len()
    }

    /// True if empty.
    pub fn is_empty(&self) -> bool {
        self.stack.is_empty()
    }

    /// All entries (bottom to top), mutably (e.g. to notify every screen).
    pub fn entries_mut(&mut self) -> impl Iterator<Item = &mut Entry<S>> {
        self.stack.iter_mut()
    }

    /// The entry at `index` (0 = bottom).
    pub fn entry_mut(&mut self, index: usize) -> Option<&mut Entry<S>> {
        self.stack.get_mut(index)
    }

    /// The route below the top, if any.
    pub fn below_top(&self) -> Option<&Route> {
        self.stack
            .len()
            .checked_sub(2)
            .and_then(|i| self.stack.get(i))
            .map(|e| &e.route)
    }

    /// Applies one command. `make` builds a screen for a new route.
    pub fn apply(&mut self, cmd: NavCommand, make: &mut dyn FnMut(&Route) -> S) -> Change<S> {
        let mut change = Change::default();
        let entry = |route: Route, make: &mut dyn FnMut(&Route) -> S| Entry {
            screen: make(&route),
            route,
            saved_focus: None,
        };
        match cmd {
            NavCommand::Push(route) => {
                if self.top().is_some_and(|t| t.route.same_page(&route)) {
                    return change;
                }
                change.covered_previous = !self.stack.is_empty();
                self.stack.push(entry(route, make));
                change.top_changed = true;
            }
            NavCommand::Replace(route) => {
                if let Some(old) = self.stack.pop() {
                    change.removed.push(old);
                }
                self.stack.push(entry(route, make));
                change.top_changed = true;
            }
            NavCommand::Back => {
                if self.stack.len() > 1 {
                    if let Some(old) = self.stack.pop() {
                        change.removed.push(old);
                    }
                    change.top_changed = true;
                }
            }
            NavCommand::Reset(route) => {
                change.removed.extend(self.stack.drain(..).rev());
                self.stack.push(entry(route, make));
                change.top_changed = true;
            }
            NavCommand::Section(route) => {
                if self.top().is_some_and(|t| t.route.same_page(&route)) {
                    return change;
                }
                // Keep an existing Home at the bottom (it keeps its state); drop the rest.
                let keep_home = self
                    .stack
                    .first()
                    .is_some_and(|e| matches!(e.route, Route::Home));
                let keep = usize::from(keep_home);
                change.removed.extend(self.stack.drain(keep..).rev());
                if !keep_home {
                    self.stack.push(entry(Route::Home, make));
                }
                if !matches!(route, Route::Home) {
                    // A kept Home that was the top is now covered.
                    change.covered_previous = keep_home && change.removed.is_empty();
                    self.stack.push(entry(route, make));
                }
                change.top_changed = true;
            }
        }
        change
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(r: &Router<String>) -> Vec<String> {
        r.routes().map(ToString::to_string).collect()
    }

    fn make(route: &Route) -> String {
        route.to_string()
    }

    #[test]
    fn push_back_and_root() {
        let mut r = Router::new();
        r.apply(NavCommand::Reset(Route::Home), &mut make);
        let c = r.apply(NavCommand::Push(Route::live()), &mut make);
        assert!(c.covered_previous && c.top_changed);
        // Pushing the same page again is a no-op.
        let c = r.apply(NavCommand::Push(Route::live()), &mut make);
        assert!(!c.top_changed);
        assert_eq!(names(&r), ["home", "live"]);
        let c = r.apply(NavCommand::Back, &mut make);
        assert_eq!(c.removed.len(), 1);
        assert_eq!(names(&r), ["home"]);
        // Back on the root does nothing.
        let c = r.apply(NavCommand::Back, &mut make);
        assert!(!c.top_changed && c.removed.is_empty());
        assert_eq!(names(&r), ["home"]);
    }

    #[test]
    fn section_switch_keeps_home_and_drops_the_rest() {
        let mut r = Router::new();
        r.apply(NavCommand::Reset(Route::Home), &mut make);
        r.apply(NavCommand::Push(Route::live()), &mut make);
        r.apply(NavCommand::Push(Route::Guide), &mut make);
        let c = r.apply(
            NavCommand::Section(Route::Movies { category: None }),
            &mut make,
        );
        assert_eq!(c.removed.len(), 2, "live and guide are dropped");
        assert_eq!(names(&r), ["home", "movies"]);
        r.apply(NavCommand::Section(Route::Home), &mut make);
        assert_eq!(names(&r), ["home"]);
        // From a non-Home root (e.g. profiles), a section builds Home underneath.
        r.apply(NavCommand::Reset(Route::Profiles), &mut make);
        r.apply(NavCommand::Section(Route::Favourites), &mut make);
        assert_eq!(names(&r), ["home", "favourites"]);
    }

    #[test]
    fn replace_and_reset() {
        let mut r = Router::new();
        r.apply(NavCommand::Reset(Route::Profiles), &mut make);
        r.apply(
            NavCommand::Push(Route::Onboarding { edit: None }),
            &mut make,
        );
        let c = r.apply(NavCommand::Replace(Route::Home), &mut make);
        assert_eq!(c.removed.len(), 1);
        assert_eq!(names(&r), ["profiles", "home"]);
        let c = r.apply(NavCommand::Reset(Route::Home), &mut make);
        assert_eq!(c.removed.len(), 2);
        assert_eq!(names(&r), ["home"]);
    }
}
