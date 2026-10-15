// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use std::collections::HashSet;

/// A logical input button, named in engine-neutral terms. The host (the editor)
/// decides which physical key maps to each one; the engine never knows what a
/// keyboard is.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub enum Button {
    Up,
    Down,
    Left,
    Right,
    Jump,
    /// Run instead of walk, while held.
    Run,
    /// Crouch, while held.
    Crouch,
    /// The main action, while held: use what is in the hand (dig with a
    /// spade, say). The game decides what it does.
    Use,
    /// The second action, while held (pile ground with a spade, say).
    UseAlt,
}

/// Which logical buttons are currently held. Plain runtime state, refreshed by
/// the host as input events arrive and read by systems on each tick. It is not
/// part of the World and is never serialized.
#[derive(Default)]
pub struct InputState {
    held: HashSet<Button>,
    /// Where the player wants to look (yaw, pitch), when the host sends it
    /// with the buttons: a multiplayer client's view, say. None leaves the
    /// view as it is.
    look: Option<(f32, f32)>,
}

impl InputState {
    pub fn new() -> Self {
        InputState {
            held: HashSet::new(),
            look: None,
        }
    }

    /// Record a button as held or released.
    pub fn set(&mut self, button: Button, pressed: bool) {
        if pressed {
            self.held.insert(button);
        } else {
            self.held.remove(&button);
        }
    }

    /// Is this button currently held?
    pub fn is_held(&self, button: Button) -> bool {
        self.held.contains(&button)
    }

    /// Ask to look this way (yaw and pitch in radians, see `world::Look`).
    pub fn set_look(&mut self, yaw: f32, pitch: f32) {
        self.look = Some((yaw, pitch));
    }

    /// The view asked for with `set_look`, if any.
    pub fn look(&self) -> Option<(f32, f32)> {
        self.look
    }
}
