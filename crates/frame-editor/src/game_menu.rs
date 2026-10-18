// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! A game's own menu, shown by a game build before play and again when the
//! player presses Esc twice. The engine draws nothing of it: the game draws
//! its screens with egui over its running world, and says what to do next.
//! Start a game with one through `run_player_with_menu`.
//!
//! A menu can also start a `GameSession`: play where something other than
//! this program decides how the player's character moves, a multiplayer
//! server say. The world still runs here (the ground, water and sky), but
//! the host hands the player's input to the session instead of moving the
//! character itself.

use frame_engine::input::InputState;
use frame_engine::world::World;

/// The player settings the host applies while the game runs. A menu edits
/// them in `GameMenu::show`; changes take effect straight away. Keeping them
/// between runs is the game's job.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GameSettings {
    /// How fast the mouse turns the view, 1.0 being the default.
    pub look_sensitivity: f32,
    /// Moving the mouse up looks down.
    pub invert_look_y: bool,
    /// Overall volume, 0.0 (silent) to 1.0 (full).
    pub volume: f32,
}

impl Default for GameSettings {
    fn default() -> Self {
        GameSettings {
            look_sensitivity: 1.0,
            invert_look_y: false,
            volume: 1.0,
        }
    }
}

impl GameSettings {
    /// The same settings with every value in range.
    pub fn sanitized(self) -> GameSettings {
        let fix = |v: f32, default: f32, lo: f32, hi: f32| {
            if v.is_finite() {
                v.clamp(lo, hi)
            } else {
                default
            }
        };
        GameSettings {
            look_sensitivity: fix(self.look_sensitivity, 1.0, 0.1, 5.0),
            invert_look_y: self.invert_look_y,
            volume: fix(self.volume, 1.0, 0.0, 1.0),
        }
    }
}

/// What the player chose in the menu this frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GameMenuAction {
    /// Keep showing the menu.
    Stay,
    /// Close the menu and play: in the session the menu hands over in
    /// `GameMenu::take_session` if it has one, or else on this computer.
    Play,
    /// End the session being played, if any, and stay in the menu.
    Leave,
    /// Close the game.
    Quit,
}

/// Where the camera looks from while the menu is open.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MenuView {
    /// The camera's position in the world.
    pub eye: [f64; 3],
    /// The point it looks at.
    pub target: [f64; 3],
}

/// A game's menu. The world keeps running behind it (water flows, clouds
/// drift, the clock moves) but takes no input from the player while it is
/// open.
pub trait GameMenu {
    /// Draw the menu for this frame. `world` is the running game world;
    /// `settings` are the ones in use, which the menu may change.
    fn show(
        &mut self,
        ctx: &egui::Context,
        world: &World,
        settings: &mut GameSettings,
    ) -> GameMenuAction;

    /// Where to look from while the menu is open, `seconds` after it first
    /// opened. None shows the player's own view.
    fn view(&mut self, world: &World, seconds: f64) -> Option<MenuView> {
        let _ = (world, seconds);
        None
    }

    /// Asked when the menu says `Play`: a session to play in, or None to
    /// play on this computer. A session already running carries on.
    fn take_session(&mut self) -> Option<Box<dyn GameSession>> {
        None
    }

    /// Sounds the menu wants played now, by file name under `assets/` (a
    /// click, say), asked once a frame. They play as heard from the
    /// listener, at the player's volume. The default is none.
    fn take_sounds(&mut self) -> Vec<String> {
        Vec::new()
    }

    /// The session being played ended by itself (`GameSession::finished`)
    /// and the menu is showing again; `reason` says why, for the player.
    fn session_ended(&mut self, reason: &str) {
        let _ = reason;
    }
}

/// Play that something else drives: a connection to a multiplayer server,
/// say. While a session runs, the host does not move `Controlled` entities
/// itself (no movement from input, no actions, no physics step); each tick
/// it gives the session the player's input and the world instead, and the
/// session moves what it is told to. Everything else in the world (modules,
/// scripts, the sky) runs as usual.
pub trait GameSession {
    /// Once a simulation tick, before the world's other systems run, with
    /// the buttons the player holds this tick (none while the menu shows).
    fn tick(&mut self, world: &mut World, input: &InputState);

    /// Why the session has ended by itself (the server went away, say), or
    /// None while it runs. The host then calls `end` and shows the menu.
    fn finished(&self) -> Option<String>;

    /// The session is over: tidy what it put in the world.
    fn end(&mut self, world: &mut World);
}

/// A volume from 0.0 to 1.0 as decibels for the audio mixer: silence at the
/// bottom, no change at full.
pub(crate) fn volume_decibels(volume: f32) -> f32 {
    const SILENCE: f32 = -60.0;
    if !volume.is_finite() || volume <= 0.001 {
        return SILENCE;
    }
    (20.0 * volume.min(1.0).log10()).max(SILENCE)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_out_of_range_are_pulled_back() {
        let s = GameSettings {
            look_sensitivity: f32::NAN,
            invert_look_y: true,
            volume: 4.0,
        }
        .sanitized();
        assert_eq!(s.look_sensitivity, 1.0);
        assert!(s.invert_look_y);
        assert_eq!(s.volume, 1.0);
        assert_eq!(
            GameSettings {
                look_sensitivity: 0.0,
                ..GameSettings::default()
            }
            .sanitized()
            .look_sensitivity,
            0.1
        );
    }

    #[test]
    fn volume_maps_to_decibels() {
        assert_eq!(volume_decibels(1.0), 0.0);
        assert_eq!(volume_decibels(0.0), -60.0);
        assert!((volume_decibels(0.5) + 6.02).abs() < 0.01);
        assert_eq!(volume_decibels(f32::NAN), -60.0);
    }

    #[test]
    fn a_menu_shows_the_players_view_unless_it_says_otherwise() {
        struct Plain;
        impl GameMenu for Plain {
            fn show(
                &mut self,
                _: &egui::Context,
                _: &World,
                _: &mut GameSettings,
            ) -> GameMenuAction {
                GameMenuAction::Stay
            }
        }
        assert_eq!(Plain.view(&World::default(), 3.0), None);
    }
}
