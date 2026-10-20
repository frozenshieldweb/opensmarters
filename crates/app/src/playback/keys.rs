// SPDX-License-Identifier: GPL-3.0-or-later
//! Channel keys.
//!
//! | Step | Fullscreen player (TV-remote convention) | Live TV list (follows the list) |
//! |---|---|---|
//! | channel **+1** (next in the list, usually the higher number) | PageUp, Ctrl+↑, media "next" | Ctrl+↓, media "next" |
//! | channel **−1** (previous) | PageDown, Ctrl+↓, media "previous" | Ctrl+↑, media "previous" |
//!
//! - **Fullscreen player:** the TV remote's CH▲/CH▼ (PageUp/PageDown), plus ↑/↓ (and
//!   →/← while the OSD is hidden) for live channels — handled by the player, which knows
//!   whether the OSD is up.
//! - **Live TV:** the list is on screen, so Ctrl+↓ moves the playing channel *down the
//!   list* (to the next row), like the highlight; PageUp/PageDown page the list (it can
//!   hold ~10 000 channels).
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
        // In the Live list Ctrl+arrows follow the list (↓ = next row); in the player
        // they follow the remote (↑ = channel up).
        Key::ArrowUp if ctrl && !mods.alt => {
            Some(if surface == Surface::LiveList { -1 } else { 1 })
        }
        Key::ArrowDown if ctrl && !mods.alt => {
            Some(if surface == Surface::LiveList { 1 } else { -1 })
        }
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
    fn ctrl_arrows_follow_the_list_in_live_and_the_remote_in_the_player() {
        assert_eq!(
            channel_step(Key::ArrowDown, CTRL, Surface::LiveList),
            Some(1)
        );
        assert_eq!(
            channel_step(Key::ArrowUp, CTRL, Surface::LiveList),
            Some(-1)
        );
        assert_eq!(channel_step(Key::ArrowUp, CTRL, Surface::Player), Some(1));
        assert_eq!(
            channel_step(Key::ArrowDown, CTRL, Surface::Player),
            Some(-1)
        );
        for s in [Surface::LiveList, Surface::Player] {
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
        // Live list: Ctrl+↑ twice (−1 each) and media "previous" (−1).
        assert_eq!(take_from(&mut events, Surface::LiveList), Some(-3));
        assert_eq!(take_from(&mut events, Surface::LiveList), None);
        // PageUp and the plain arrow are left for the list.
        assert_eq!(events.len(), 2);
        assert_eq!(take_from(&mut events, Surface::Player), Some(1));
        assert_eq!(events.len(), 1);
    }
}
