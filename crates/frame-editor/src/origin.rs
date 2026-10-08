// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! The render origin: how a world far from (0, 0, 0) is drawn precisely.
//!
//! Entity positions are `f64`, so the world stays exact at planet scale. The
//! graphics card works in `f32`, which is only fine near the camera (an `f32`
//! has about seven digits, so its steps grow with distance: roughly 6 cm at
//! 1,000 km out and 2 m at 20,000 km). The fix is to draw everything relative
//! to a point near the camera: subtract the origin from each position in `f64`,
//! where it is exact, and only then drop to `f32`, where the result is a small
//! number again.
//!
//! The origin moves in whole steps, not continuously. If it followed the
//! camera exactly, every object's drawn position would change a little each
//! frame (and cached geometry could never be reused). Snapped to a grid, it
//! only changes when the camera crosses into a new cell, and every position
//! relative to it stays well inside `f32`'s precise range.

use glam::{DVec3, Vec3};

/// How far apart the possible origins are, in world units. Large enough that
/// the origin rarely changes, small enough that nothing near the camera is
/// ever more than a few hundred units from it.
pub(crate) const ORIGIN_STEP: f64 = 256.0;

/// The origin to draw with when the camera is at `reference`: the nearest
/// point on the origin grid. Anything within half a step of the world's
/// (0, 0, 0) gets the origin (0, 0, 0), so small worlds draw exactly as they
/// always did.
pub(crate) fn snap(reference: DVec3) -> DVec3 {
    let fix = |v: f64| {
        if v.is_finite() {
            (v / ORIGIN_STEP).round() * ORIGIN_STEP
        } else {
            0.0
        }
    };
    DVec3::new(fix(reference.x), fix(reference.y), fix(reference.z))
}

/// A world position as the graphics card sees it: relative to `origin`.
pub(crate) fn relative(position: DVec3, origin: DVec3) -> Vec3 {
    (position - origin).as_vec3()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn near_the_world_origin_nothing_moves() {
        assert_eq!(snap(DVec3::new(0.0, 0.0, 0.0)), DVec3::ZERO);
        assert_eq!(snap(DVec3::new(100.0, -127.0, 50.0)), DVec3::ZERO);
        let p = DVec3::new(12.5, 3.25, -7.75);
        assert_eq!(relative(p, snap(DVec3::ZERO)), Vec3::new(12.5, 3.25, -7.75));
    }

    #[test]
    fn the_origin_is_a_whole_number_of_steps_and_only_moves_in_steps() {
        let a = snap(DVec3::new(1000.0, 0.0, 0.0));
        assert_eq!(a.x % ORIGIN_STEP, 0.0);
        assert_eq!(
            a,
            snap(DVec3::new(1010.0, 0.0, 0.0)),
            "a small move keeps the origin"
        );
        assert_ne!(a, snap(DVec3::new(1000.0 + ORIGIN_STEP, 0.0, 0.0)));
    }

    #[test]
    fn a_camera_is_never_far_from_its_origin() {
        for v in [
            0.0,
            1.0,
            127.9,
            128.1,
            5000.0,
            -5000.0,
            2.0e10,
            -2.0e10 + 77.0,
        ] {
            let eye = DVec3::new(v, v * 0.5, -v);
            let rel = relative(eye, snap(eye));
            assert!(
                rel.abs().max_element() <= (ORIGIN_STEP * 0.5) as f32,
                "{v}: {rel:?}"
            );
        }
    }

    #[test]
    fn a_thing_near_the_camera_is_exact_even_at_planet_distance() {
        // 20,000 km out, in world units of 1 m. An f32 position here would
        // round to the nearest 2 m; relative to the origin it is exact.
        let eye = DVec3::new(2.0e7, 0.0, 0.0);
        let thing = DVec3::new(2.0e7 + 5.25, 1.5, -3.125);
        let rel = relative(thing, snap(eye));
        let origin = snap(eye);
        assert_eq!(
            DVec3::new(rel.x as f64, rel.y as f64, rel.z as f64) + origin,
            thing
        );
        // The old way, for contrast: an f32 position that far out is off by
        // metres.
        let naive = 2.0e7_f64 + 5.25;
        assert!((naive as f32 as f64 - naive).abs() > 0.2);
    }

    #[test]
    fn the_drawn_distance_between_two_things_does_not_depend_on_the_origin() {
        let a = DVec3::new(1.0e6 + 3.0, 10.0, 1.0e6 - 2.0);
        let b = DVec3::new(1.0e6 + 8.5, 12.0, 1.0e6 - 2.0);
        let d1 = relative(a, snap(a)) - relative(b, snap(a));
        let moved = snap(a + DVec3::new(ORIGIN_STEP * 3.0, 0.0, 0.0));
        let d2 = relative(a, moved) - relative(b, moved);
        assert_eq!(d1, d2);
        assert_eq!(d1, Vec3::new(-5.5, -2.0, 0.0));
    }

    #[test]
    fn a_bad_reference_gives_the_world_origin() {
        assert_eq!(snap(DVec3::new(f64::NAN, f64::INFINITY, 0.0)), DVec3::ZERO);
    }
}
