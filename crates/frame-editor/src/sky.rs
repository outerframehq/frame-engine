// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! The sky behind the scene and the fog in front of it.
//!
//! Everything that decides a colour is here, on the CPU, where it can be
//! tested without a GPU. The shader (`shader.wgsl`, `vs_sky`/`fs_sky` and the
//! fog step at the end of the entity shaders) only turns the numbers in
//! [`SkyUniform`] into pixels.
//!
//! The sun comes from the scene's own light: the sky is lit by the direction
//! of the first directional Light, so turning that light turns the sun, and
//! lowering it to the horizon gives a sunset and then night. Fog is tinted
//! the horizon colour so distant ground dissolves into the sky with no seam.

use glam::{Mat4, Vec3, Vec4};

/// How much of each sky colour is the vivid version rather than the natural
/// one. The same 65 percent vivid, 35 percent natural look as the terrain
/// palette, so the sky and the ground belong to one world.
pub(crate) const VIVID: f32 = 0.65;

/// Fog thickness. Fog reaches about 63 percent at `1 / density` world units,
/// and the density is `fog_amount / FOG_REFERENCE_DISTANCE`. 1.0 puts that
/// point 1800 units out, which suits a map with a view distance of a few
/// thousand units.
pub(crate) const FOG_REFERENCE_DISTANCE: f32 = 1800.0;

/// The most fog the preference allows (a multiplier on the default).
pub(crate) const FOG_AMOUNT_MAX: f32 = 4.0;
/// The fog a fresh install starts with.
pub(crate) const FOG_AMOUNT_DEFAULT: f32 = 1.0;

/// The sky's data for the shader. Must match `Sky` in `shader.wgsl` field for
/// field. Every field is a vec4 or mat4 to keep the layout simple.
#[repr(C)]
#[derive(Copy, Clone, Debug, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub(crate) struct SkyUniform {
    /// Turns a clip-space point back into a world-space point, to find the
    /// direction each pixel looks in.
    pub inv_view_proj: [[f32; 4]; 4],
    /// xyz: the camera's world position. w: unused.
    pub eye: [f32; 4],
    /// xyz: unit direction toward the sun. w: how strongly to draw the sun
    /// disc (0 when there is no light to be the sun, or it is below the
    /// horizon).
    pub sun_direction: [f32; 4],
    /// rgb: the colour straight overhead. w: 1.0 if the sky is drawn.
    pub zenith: [f32; 4],
    /// rgb: the colour at the horizon, also the fog colour. w: fog density
    /// per world unit (0 for no fog).
    pub horizon: [f32; 4],
    /// rgb: the colour of the sun and its glow. w: unused.
    pub sun_colour: [f32; 4],
    /// rgb: the colour looking straight down, below the horizon. w: unused.
    pub ground: [f32; 4],
}

type Rgb = [f32; 3];

fn mix(a: Rgb, b: Rgb, t: f32) -> Rgb {
    let t = t.clamp(0.0, 1.0);
    [
        a[0] + (b[0] - a[0]) * t,
        a[1] + (b[1] - a[1]) * t,
        a[2] + (b[2] - a[2]) * t,
    ]
}

fn smoothstep(lo: f32, hi: f32, x: f32) -> f32 {
    let t = ((x - lo) / (hi - lo)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// A colour that is `VIVID` of the way from the natural value to the vivid one.
fn tone(natural: Rgb, vivid: Rgb) -> Rgb {
    mix(natural, vivid, VIVID)
}

/// The sky for a sun whose direction has the given height (the y component of
/// the unit vector toward the sun: 1 is overhead, 0 on the horizon, below 0
/// under it).
#[derive(Copy, Clone, Debug, PartialEq)]
pub(crate) struct SkyColours {
    pub zenith: Rgb,
    pub horizon: Rgb,
    pub ground: Rgb,
    pub sun: Rgb,
    /// 0 when the sun is well below the horizon, 1 once it is up.
    pub sun_strength: f32,
}

impl SkyColours {
    pub(crate) fn for_sun_height(height: f32) -> SkyColours {
        let height = if height.is_finite() {
            height.clamp(-1.0, 1.0)
        } else {
            0.5
        };
        // 0 at night, 1 by day, with a quick change around the horizon.
        let day = smoothstep(-0.12, 0.28, height);
        // Peaks as the sun sits on the horizon.
        let twilight = (-(height / 0.2).powi(2)).exp();

        let zenith_day = tone([0.25, 0.45, 0.76], [0.16, 0.40, 0.96]);
        let horizon_day = tone([0.72, 0.80, 0.88], [0.60, 0.82, 0.99]);
        let zenith_night = [0.015, 0.025, 0.07];
        let horizon_night = [0.05, 0.07, 0.15];
        let dusk = tone([0.92, 0.58, 0.40], [1.0, 0.50, 0.28]);

        let zenith = mix(zenith_night, zenith_day, day);
        let mut horizon = mix(horizon_night, horizon_day, day);
        // Sunrise and sunset warm the horizon (and so the fog with it).
        horizon = mix(horizon, dusk, twilight * 0.55);
        // Below the horizon the ground haze is a darker horizon colour.
        let ground = [horizon[0] * 0.6, horizon[1] * 0.6, horizon[2] * 0.62];

        // The sun is white overhead, orange at the horizon.
        let sun = mix([1.0, 0.96, 0.82], [1.0, 0.55, 0.24], twilight);
        let sun_strength = smoothstep(-0.06, 0.04, height);
        SkyColours {
            zenith,
            horizon,
            ground,
            sun,
            sun_strength,
        }
    }
}

/// The fog density per world unit for a fog amount (0 turns fog off).
pub(crate) fn fog_density(amount: f32) -> f32 {
    if !amount.is_finite() {
        return 0.0;
    }
    amount.clamp(0.0, FOG_AMOUNT_MAX) / FOG_REFERENCE_DISTANCE
}

/// How much of the fog colour covers something `distance` away: 0 is none,
/// 1 is all of it. Mirrors the fog step in `shader.wgsl`.
#[cfg(test)]
pub(crate) fn fog_factor(distance: f32, density: f32) -> f32 {
    let x = distance.max(0.0) * density;
    1.0 - (-(x * x)).exp()
}

/// Where the camera is, found from its view-projection matrix: the eye is the
/// one point that every perspective projection sends to w = 0.
pub(crate) fn eye_from_view_proj(view_proj: &Mat4) -> Vec3 {
    let inverse = view_proj.inverse();
    let h = inverse * Vec4::new(0.0, 0.0, 1.0, 0.0);
    let eye = if h.w.abs() > 1e-12 {
        h.truncate() / h.w
    } else {
        // An orthographic matrix has no single eye; fall back to the
        // near-plane centre so nothing is NaN.
        let n = inverse * Vec4::new(0.0, 0.0, 0.0, 1.0);
        n.truncate() / n.w
    };
    // A broken matrix (all zeros, NaN) puts the eye at the origin rather than
    // poisoning the fog with NaN.
    if eye.is_finite() { eye } else { Vec3::ZERO }
}

/// Everything the shader needs for one camera. `sun` is the direction toward
/// the sun (any length), or None when the scene has no directional light.
/// `enabled` false gives a sky-less, fog-less frame (the old plain
/// background).
pub(crate) fn sky_uniform(
    view_proj: [[f32; 4]; 4],
    sun: Option<[f32; 3]>,
    enabled: bool,
    fog_amount: f32,
) -> SkyUniform {
    let vp = Mat4::from_cols_array_2d(&view_proj);
    let inverse = vp.inverse();
    let eye = eye_from_view_proj(&vp);
    let sun_dir = sun
        .map(Vec3::from_array)
        .filter(|d| d.is_finite() && d.length_squared() > 1e-12)
        .map(|d| d.normalize());
    // With no light to act as the sun it is a fixed late-morning sky, with no
    // sun disc drawn.
    let direction = sun_dir.unwrap_or_else(|| Vec3::new(0.35, 0.8, 0.45).normalize());
    let colours = SkyColours::for_sun_height(direction.y);
    let disc = if sun_dir.is_some() {
        colours.sun_strength
    } else {
        0.0
    };
    SkyUniform {
        inv_view_proj: if inverse.is_finite() {
            inverse.to_cols_array_2d()
        } else {
            Mat4::IDENTITY.to_cols_array_2d()
        },
        eye: [eye.x, eye.y, eye.z, 0.0],
        sun_direction: [direction.x, direction.y, direction.z, disc],
        zenith: [
            colours.zenith[0],
            colours.zenith[1],
            colours.zenith[2],
            if enabled { 1.0 } else { 0.0 },
        ],
        horizon: [
            colours.horizon[0],
            colours.horizon[1],
            colours.horizon[2],
            if enabled {
                fog_density(fog_amount)
            } else {
                0.0
            },
        ],
        sun_colour: [colours.sun[0], colours.sun[1], colours.sun[2], 0.0],
        ground: [colours.ground[0], colours.ground[1], colours.ground[2], 0.0],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn luma(c: Rgb) -> f32 {
        0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2]
    }

    #[test]
    fn the_uniform_has_a_layout_the_shader_can_read() {
        // A mat4 and six vec4s, with no padding to go wrong.
        assert_eq!(std::mem::size_of::<SkyUniform>(), 64 + 6 * 16);
        assert_eq!(std::mem::size_of::<SkyUniform>() % 16, 0);
    }

    #[test]
    fn day_is_bright_blue_and_night_is_dark() {
        let noon = SkyColours::for_sun_height(0.9);
        let night = SkyColours::for_sun_height(-0.6);
        assert!(
            noon.zenith[2] > noon.zenith[0],
            "a blue sky: {:?}",
            noon.zenith
        );
        assert!(luma(noon.horizon) > 0.6);
        assert!(luma(night.zenith) < 0.05, "night zenith {:?}", night.zenith);
        assert!(luma(night.horizon) < 0.15);
        assert!(luma(noon.zenith) > 4.0 * luma(night.zenith));
    }

    #[test]
    fn sunset_warms_the_horizon_and_the_sun() {
        let noon = SkyColours::for_sun_height(0.9);
        let dusk = SkyColours::for_sun_height(0.0);
        // More red than blue at the horizon at dusk, the opposite by day.
        assert!(dusk.horizon[0] > dusk.horizon[2], "{:?}", dusk.horizon);
        assert!(noon.horizon[2] > noon.horizon[0], "{:?}", noon.horizon);
        // The sun itself goes from near white to orange.
        assert!(
            dusk.sun[2] < noon.sun[2] - 0.2,
            "{:?} {:?}",
            dusk.sun,
            noon.sun
        );
    }

    #[test]
    fn the_sun_only_shows_when_it_is_above_the_horizon() {
        assert_eq!(SkyColours::for_sun_height(-0.5).sun_strength, 0.0);
        assert_eq!(SkyColours::for_sun_height(0.5).sun_strength, 1.0);
        let low = SkyColours::for_sun_height(0.0).sun_strength;
        assert!(low > 0.0 && low < 1.0, "half-risen: {low}");
    }

    #[test]
    fn the_sky_changes_smoothly_with_the_sun() {
        // No jump larger than a small step as the sun sweeps across the sky (the
        // steepest part is the quick change around the horizon).
        let mut last = SkyColours::for_sun_height(-1.0);
        let mut h = -1.0f32;
        while h < 1.0 {
            h += 0.01;
            let now = SkyColours::for_sun_height(h);
            for k in 0..3 {
                assert!(
                    (now.zenith[k] - last.zenith[k]).abs() < 0.05,
                    "zenith jumps at {h}"
                );
                assert!(
                    (now.horizon[k] - last.horizon[k]).abs() < 0.05,
                    "horizon jumps at {h}"
                );
            }
            last = now;
        }
    }

    #[test]
    fn bad_sun_heights_do_not_produce_nan_colours() {
        for h in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, 5.0, -5.0] {
            let c = SkyColours::for_sun_height(h);
            for v in c
                .zenith
                .iter()
                .chain(&c.horizon)
                .chain(&c.ground)
                .chain(&c.sun)
            {
                assert!(v.is_finite() && *v >= 0.0 && *v <= 1.0, "{h}: {v}");
            }
        }
    }

    #[test]
    fn fog_grows_with_distance_and_with_the_amount() {
        let d = fog_density(1.0);
        assert_eq!(fog_factor(0.0, d), 0.0);
        let near = fog_factor(100.0, d);
        let mid = fog_factor(1800.0, d);
        let far = fog_factor(8000.0, d);
        assert!(near < 0.01, "almost no fog up close: {near}");
        assert!(
            (mid - 0.632).abs() < 0.01,
            "63 percent at the reference distance: {mid}"
        );
        assert!(far > 0.99, "everything is fogged out far away: {far}");
        assert!(fog_factor(1000.0, fog_density(2.0)) > fog_factor(1000.0, fog_density(1.0)));
        assert_eq!(fog_density(0.0), 0.0);
        assert_eq!(fog_density(f32::NAN), 0.0);
        assert_eq!(fog_density(100.0), FOG_AMOUNT_MAX / FOG_REFERENCE_DISTANCE);
    }

    #[test]
    fn the_eye_is_recovered_from_the_view_projection() {
        let eye = Vec3::new(12.0, 34.0, -56.0);
        let view = Mat4::look_at_rh(eye, eye + Vec3::new(0.3, -0.2, -1.0), Vec3::Y);
        let proj = Mat4::perspective_rh(1.0, 16.0 / 9.0, 0.1, 5000.0);
        let found = eye_from_view_proj(&(proj * view));
        assert!((found - eye).length() < 0.05, "{found:?} vs {eye:?}");
        // And from a camera that is looking straight down, too.
        let view = Mat4::look_at_rh(Vec3::new(0.0, 100.0, 0.0), Vec3::ZERO, Vec3::Z);
        let found = eye_from_view_proj(&(proj * view));
        assert!(
            (found - Vec3::new(0.0, 100.0, 0.0)).length() < 0.05,
            "{found:?}"
        );
    }

    #[test]
    fn the_uniform_carries_the_sun_the_eye_and_the_switch() {
        let eye = Vec3::new(0.0, 50.0, 200.0);
        let vp = Mat4::perspective_rh(1.0, 1.5, 0.1, 4000.0)
            * Mat4::look_at_rh(eye, Vec3::ZERO, Vec3::Y);
        let u = sky_uniform(vp.to_cols_array_2d(), Some([0.0, 2.0, 0.0]), true, 1.0);
        assert_eq!(
            &u.sun_direction[..3],
            &[0.0, 1.0, 0.0],
            "normalised toward the light"
        );
        assert_eq!(u.sun_direction[3], 1.0, "the sun is up");
        assert_eq!(u.zenith[3], 1.0, "sky on");
        assert!(u.horizon[3] > 0.0, "fog on");
        assert!((Vec3::new(u.eye[0], u.eye[1], u.eye[2]) - eye).length() < 0.05);

        let off = sky_uniform(vp.to_cols_array_2d(), Some([0.0, 2.0, 0.0]), false, 1.0);
        assert_eq!(off.zenith[3], 0.0, "sky off");
        assert_eq!(off.horizon[3], 0.0, "and no fog with it");

        // No light in the scene: still a sky, but no sun disc is drawn.
        let none = sky_uniform(vp.to_cols_array_2d(), None, true, 1.0);
        assert_eq!(none.sun_direction[3], 0.0);
        assert!(none.sun_direction[1] > 0.0, "a daytime sky");
        // A zero-length direction counts as no light too.
        let zero = sky_uniform(vp.to_cols_array_2d(), Some([0.0; 3]), true, 1.0);
        assert_eq!(zero.sun_direction[3], 0.0);
    }

    #[test]
    fn a_broken_matrix_never_puts_nan_in_the_uniform() {
        let u = sky_uniform([[0.0; 4]; 4], Some([0.0, 1.0, 0.0]), true, 1.0);
        let bytes: &[f32] = bytemuck::cast_slice(std::slice::from_ref(&u));
        assert!(bytes.iter().all(|v| v.is_finite()), "{u:?}");
    }

    #[test]
    fn a_sun_below_the_horizon_hides_the_disc_and_darkens_the_sky() {
        let vp = Mat4::IDENTITY.to_cols_array_2d();
        let u = sky_uniform(vp, Some([0.3, -0.8, 0.2]), true, 1.0);
        assert_eq!(u.sun_direction[3], 0.0);
        assert!(luma([u.zenith[0], u.zenith[1], u.zenith[2]]) < 0.05);
    }
}
