// SPDX-License-Identifier: GPL-3.0-or-later
//! Channel keys: **one convention everywhere**, the TV remote's.
//!
//! | Step | Keys |
//! |---|---|
//! | channel **+1** (next in the list, usually the higher number) | PageUp, Ctrl+↑, media "next track" |
//! | channel **−1** (previous) | PageDown, Ctrl+↓, media "previous track" |
//!
//! - **Fullscreen player:** all of the above, plus ↑/↓ (and →/← while the OSD is hidden)
//!   for live channels — handled by the player, which knows whether the OSD is up.
//! - **Live TV:** PageUp/PageDown page the channel list (it can hold ~10 000 channels);
//!   Ctrl+↑/↓ and the media keys zap the preview.
//!
//! Zapping stops at the ends of the list ([`super::ZapEdge`]): no wrap from the first
//! channel to the last.

use eframe::egui::{self, Event, Key, Modifiers};

/// Where the keys are pressed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Surface {
    /// Live TV's list + preview: PageUp/PageDown belong to the list.
    LiveList,
    /// The fullscreen player.
    Player,
}

/// The channel step a key press means on `surface` (`None` = not a channel key there).
pub fn channel_step(key: Key, mods: Modifiers, surface: Surface) -> Option<isize> {
    let ctrl = mods.ctrl || mods.command;
    let plain = !ctrl && !mods.alt;
    match key {
        Key::MediaTrackNext => Some(1),
        Key::MediaTrackPrevious => Some(-1),
        Key::ArrowUp if ctrl && !mods.alt => Some(1),
        Key::ArrowDown if ctrl && !mods.alt => Some(-1),
        Key::PageUp if plain && surface == Surface::Player => Some(1),
        Key::PageDown if plain && surface == Surface::Player => Some(-1),
        _ => None,
    }
}

/// Consumes this frame's channel keys for `surface` and returns the summed step (`None`
/// if there were none). Call from `Screen::on_input`, before the focus engine.
pub fn take_channel_step(ctx: &egui::Context, surface: Surface) -> Option<isize> {
    ctx.input_mut(|i| take_from(&mut i.events, surface))
}

/// Removes the channel key events for `surface` from `events`; returns the summed step.
fn take_from(events: &mut Vec<Event>, surface: Surface) -> Option<isize> {
    let mut step = 0isize;
    let mut any = false;
    events.retain(|e| match e {
        Event::Key {
            key,
            pressed: true,
            modifiers,
            ..
        } => match channel_step(*key, *modifiers, surface) {
            Some(s) => {
                step += s;
                any = true;
                false
            }
            None => true,
        },
        // Drop the matching releases too, so nothing else reacts to half a press.
        Event::Key {
            key,
            pressed: false,
            modifiers,
            ..
        } => channel_step(*key, *modifiers, surface).is_none(),
        _ => true,
    });
    any.then_some(step)
}

#[cfg(test)]
mod tests {
    use super::*;

    const CTRL: Modifiers = Modifiers {
        alt: false,
        ctrl: true,
        shift: false,
        mac_cmd: false,
        command: true,
    };

    #[test]
    fn one_convention_on_both_surfaces() {
        for s in [Surface::LiveList, Surface::Player] {
            assert_eq!(channel_step(Key::ArrowUp, CTRL, s), Some(1), "{s:?}");
            assert_eq!(channel_step(Key::ArrowDown, CTRL, s), Some(-1), "{s:?}");
            assert_eq!(
                channel_step(Key::MediaTrackNext, Modifiers::NONE, s),
                Some(1)
            );
            assert_eq!(
                channel_step(Key::MediaTrackPrevious, Modifiers::NONE, s),
                Some(-1)
            );
            // Plain arrows move the focus (the player handles them itself).
            assert_eq!(channel_step(Key::ArrowUp, Modifiers::NONE, s), None);
        }
        // PageUp = channel +1 in the player, like CH+ on a remote...
        assert_eq!(
            channel_step(Key::PageUp, Modifiers::NONE, Surface::Player),
            Some(1)
        );
        assert_eq!(
            channel_step(Key::PageDown, Modifiers::NONE, Surface::Player),
            Some(-1)
        );
        // ...and pages the list in Live TV.
        assert_eq!(
            channel_step(Key::PageUp, Modifiers::NONE, Surface::LiveList),
            None
        );
        assert_eq!(
            channel_step(Key::PageDown, Modifiers::NONE, Surface::LiveList),
            None
        );
        assert_eq!(channel_step(Key::PageUp, CTRL, Surface::Player), None);
    }

    #[test]
    fn consumes_only_channel_keys() {
        let key = |key, modifiers| Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers,
        };
        let mut events = vec![
            key(Key::ArrowUp, CTRL),
            key(Key::ArrowUp, CTRL),
            key(Key::MediaTrackPrevious, Modifiers::NONE),
            key(Key::PageUp, Modifiers::NONE),
            key(Key::ArrowDown, Modifiers::NONE),
        ];
        assert_eq!(take_from(&mut events, Surface::LiveList), Some(1));
        assert_eq!(take_from(&mut events, Surface::LiveList), None);
        // PageUp and the plain arrow are left for the list.
        assert_eq!(events.len(), 2);
        assert_eq!(take_from(&mut events, Surface::Player), Some(1));
        assert_eq!(events.len(), 1);
    }
}
