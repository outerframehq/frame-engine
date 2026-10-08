// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Where the sun is, when something other than the scene's Light decides.
//!
//! Two things can: the editor's own "Sun by time of day" setting, which puts
//! the sun where it would be at a chosen hour, and an editor module (see
//! [`crate::module_api::EditorModule::sun`]), which can run a full day-night
//! cycle or anything else. A module's sun wins over the time of day.
//!
//! The scene is never edited. The sun overrides the direction (and fades the
//! strength) of the sun light in the light list built for each frame, and
//! tells the sky where the sun is. Turn it off and the scene's own light is
//! back, untouched.
//!
//! The state is process-wide, for the same reason the sky and shadow
//! preferences are: the editor viewport and the Play window each have their
//! own GPU state, and both must show the same sun.

use super::{LightRaw, MAX_LIGHTS, module_api::SunOverride, sky};
use std::sync::{LazyLock, Mutex, MutexGuard};

/// The hour the time-of-day sun starts at: mid-morning, so the first time it
/// is turned on the scene is well lit.
pub(crate) const DEFAULT_HOUR: f32 = 10.0;

/// How far south of overhead the noon sun stands, in degrees. A fixed,
/// mid-latitude arc: the sun rises due east, sets due west and is highest in
/// the south at noon.
const NOON_LEAN_DEGREES: f32 = 35.0;

/// Where the sun is, and how strongly it lights the scene.
#[derive(Copy, Clone, Debug, PartialEq)]
pub(crate) struct Sun {
    /// Unit vector toward the sun (X east, Y up, Z south).
    pub direction: [f32; 3],
    /// 0 when the sun is below the horizon, 1 once it is up. Multiplies the
    /// sun light's own intensity.
    pub light_scale: f32,
}

impl Sun {
    fn from_direction(direction: [f32; 3]) -> Option<Self> {
        let length = (direction[0] * direction[0]
            + direction[1] * direction[1]
            + direction[2] * direction[2])
            .sqrt();
        if !length.is_finite() || length < 1e-6 {
            return None;
        }
        let direction = [
            direction[0] / length,
            direction[1] / length,
            direction[2] / length,
        ];
        Some(Sun {
            direction,
            light_scale: sky::sun_light_scale(direction[1]),
        })
    }
}

/// The direction of the sun at `hour` (0 to 24, 6 is sunrise, 12 is noon, 18
/// is sunset): east at 6, highest in the south at 12, west at 18, below the
/// horizon from 18 to 6.
pub(crate) fn direction_for_hour(hour: f32) -> [f32; 3] {
    let hour = if hour.is_finite() { hour } else { DEFAULT_HOUR };
    let angle = (hour - 6.0) / 12.0 * std::f32::consts::PI;
    let lean = NOON_LEAN_DEGREES.to_radians();
    [
        angle.cos(),
        angle.sin() * lean.cos(),
        angle.sin() * lean.sin(),
    ]
}

#[derive(Default)]
struct State {
    time_enabled: bool,
    hour: f32,
    module: Option<SunOverride>,
}

static STATE: LazyLock<Mutex<State>> = LazyLock::new(|| {
    Mutex::new(State {
        time_enabled: false,
        hour: DEFAULT_HOUR,
        module: None,
    })
});

fn state() -> MutexGuard<'static, State> {
    // A panic elsewhere must not stop the viewport from drawing.
    STATE.lock().unwrap_or_else(|e| e.into_inner())
}

/// Turn the time-of-day sun on or off and set its hour.
pub(crate) fn set_time_of_day(enabled: bool, hour: f32) {
    let mut s = state();
    s.time_enabled = enabled;
    s.hour = if hour.is_finite() {
        hour.clamp(0.0, 24.0)
    } else {
        DEFAULT_HOUR
    };
}

pub(crate) fn time_of_day_enabled() -> bool {
    state().time_enabled
}

pub(crate) fn time_of_day_hours() -> f32 {
    state().hour
}

/// Record the sun a module asked for this frame (None for none).
pub(crate) fn set_module_sun(sun: Option<SunOverride>) {
    state().module = sun;
}

/// The sun for this frame: a module's if it gave one, else the time of day's
/// if that is on, else None (the scene's own Light decides).
pub(crate) fn current() -> Option<Sun> {
    let s = state();
    if let Some(module) = s.module {
        if let Some(sun) = Sun::from_direction(module.direction) {
            return Some(sun);
        }
    }
    if s.time_enabled {
        return Sun::from_direction(direction_for_hour(s.hour));
    }
    None
}

/// Point the scene's sun light at `sun`. The shadow-casting directional light
/// is the sun, or the first directional light if none casts shadows. Its
/// direction is replaced and its strength faded with the sun's height. Other
/// lights are left alone. Does nothing without a sun or without a directional
/// light.
pub(crate) fn apply_to_lights(lights: &mut [LightRaw; MAX_LIGHTS], sun: Option<Sun>) {
    let Some(sun) = sun else {
        return;
    };
    let directional = |l: &LightRaw| l.params[0] < 0.5 && l.params[2] > 0.0;
    let index = lights
        .iter()
        .position(|l| directional(l) && l.params[3] > 0.5)
        .or_else(|| lights.iter().position(directional));
    let Some(index) = index else {
        return;
    };
    let light = &mut lights[index];
    light.position_or_direction = [
        sun.direction[0],
        sun.direction[1],
        sun.direction[2],
        light.position_or_direction[3],
    ];
    light.params[2] *= sun.light_scale;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn directional(direction: [f32; 3], intensity: f32, casts_shadow: bool) -> LightRaw {
        LightRaw {
            position_or_direction: [direction[0], direction[1], direction[2], 0.0],
            params: [0.0, 0.0, intensity, if casts_shadow { 1.0 } else { 0.0 }],
        }
    }

    fn point(position: [f32; 3], intensity: f32) -> LightRaw {
        LightRaw {
            position_or_direction: [position[0], position[1], position[2], 0.0],
            params: [1.0, 10.0, intensity, 0.0],
        }
    }

    fn empty() -> [LightRaw; MAX_LIGHTS] {
        use bytemuck::Zeroable;
        [LightRaw::zeroed(); MAX_LIGHTS]
    }

    #[test]
    fn the_sun_rises_in_the_east_and_sets_in_the_west() {
        let rise = direction_for_hour(6.0);
        assert!(
            (rise[0] - 1.0).abs() < 1e-5 && rise[1].abs() < 1e-5,
            "{rise:?}"
        );
        let set = direction_for_hour(18.0);
        assert!(
            (set[0] + 1.0).abs() < 1e-5 && set[1].abs() < 1e-5,
            "{set:?}"
        );
    }

    #[test]
    fn noon_is_high_and_to_the_south() {
        let noon = direction_for_hour(12.0);
        assert!(noon[1] > 0.8, "{noon:?}");
        assert!(noon[2] > 0.0, "south is +Z: {noon:?}");
        assert!(noon[0].abs() < 1e-5);
    }

    #[test]
    fn midnight_is_below_the_horizon() {
        let midnight = direction_for_hour(0.0);
        assert!(midnight[1] < -0.8, "{midnight:?}");
    }

    #[test]
    fn every_hour_gives_a_unit_direction() {
        for h in 0..=48 {
            let d = direction_for_hour(h as f32 * 0.5);
            let len = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt();
            assert!(
                (len - 1.0).abs() < 1e-5,
                "hour {} gave {d:?}",
                h as f32 * 0.5
            );
        }
        let d = direction_for_hour(f32::NAN);
        assert!(d.iter().all(|v| v.is_finite()));
    }

    #[test]
    fn a_noon_sun_is_fully_lit_and_a_midnight_sun_gives_no_light() {
        let noon = Sun::from_direction(direction_for_hour(12.0)).unwrap();
        assert_eq!(noon.light_scale, 1.0);
        let midnight = Sun::from_direction(direction_for_hour(0.0)).unwrap();
        assert_eq!(midnight.light_scale, 0.0);
    }

    #[test]
    fn a_module_direction_is_normalised_and_a_zero_one_is_ignored() {
        let sun = Sun::from_direction([0.0, 3.0, 4.0]).unwrap();
        assert_eq!(sun.direction, [0.0, 0.6, 0.8]);
        assert_eq!(Sun::from_direction([0.0, 0.0, 0.0]), None);
        assert_eq!(Sun::from_direction([f32::NAN, 1.0, 0.0]), None);
    }

    #[test]
    fn the_sun_turns_the_shadow_casting_light() {
        let mut lights = empty();
        lights[0] = point([0.0, 5.0, 0.0], 1.0);
        lights[1] = directional([0.0, 1.0, 0.0], 0.5, false);
        lights[2] = directional([1.0, 0.0, 0.0], 0.8, true);
        let sun = Sun {
            direction: [0.0, 0.6, 0.8],
            light_scale: 0.5,
        };
        apply_to_lights(&mut lights, Some(sun));
        assert_eq!(
            lights[0],
            point([0.0, 5.0, 0.0], 1.0),
            "point light untouched"
        );
        assert_eq!(
            lights[1],
            directional([0.0, 1.0, 0.0], 0.5, false),
            "a second directional light is not the sun"
        );
        assert_eq!(lights[2].position_or_direction[..3], [0.0, 0.6, 0.8]);
        assert!((lights[2].params[2] - 0.4).abs() < 1e-6, "0.8 * 0.5");
        assert_eq!(lights[2].params[3], 1.0, "still the shadow caster");
    }

    #[test]
    fn without_a_shadow_caster_the_first_directional_light_is_the_sun() {
        let mut lights = empty();
        lights[0] = point([0.0, 5.0, 0.0], 1.0);
        lights[1] = directional([0.0, 1.0, 0.0], 1.0, false);
        let sun = Sun {
            direction: [1.0, 0.0, 0.0],
            light_scale: 1.0,
        };
        apply_to_lights(&mut lights, Some(sun));
        assert_eq!(lights[1].position_or_direction[..3], [1.0, 0.0, 0.0]);
    }

    #[test]
    fn the_light_goes_out_when_the_sun_is_down() {
        let mut lights = empty();
        lights[0] = directional([0.0, 1.0, 0.0], 1.0, true);
        let sun = Sun {
            direction: [0.0, -0.5, 0.8],
            light_scale: 0.0,
        };
        apply_to_lights(&mut lights, Some(sun));
        assert_eq!(lights[0].params[2], 0.0);
    }

    #[test]
    fn with_no_sun_or_no_sun_light_the_lights_are_unchanged() {
        let mut lights = empty();
        lights[0] = directional([0.0, 1.0, 0.0], 1.0, true);
        let original = lights;
        apply_to_lights(&mut lights, None);
        assert_eq!(lights, original);

        let mut only_points = empty();
        only_points[0] = point([1.0, 2.0, 3.0], 1.0);
        let original = only_points;
        let sun = Sun {
            direction: [0.0, 1.0, 0.0],
            light_scale: 1.0,
        };
        apply_to_lights(&mut only_points, Some(sun));
        assert_eq!(only_points, original);
    }
}
