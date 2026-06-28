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
}

/// Which logical buttons are currently held. Plain runtime state, refreshed by
/// the host as input events arrive and read by systems on each tick. It is not
/// part of the World and is never serialized.
#[derive(Default)]
pub struct InputState {
    held: HashSet<Button>,
}

impl InputState {
    pub fn new() -> Self {
        InputState {
            held: HashSet::new(),
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
}
