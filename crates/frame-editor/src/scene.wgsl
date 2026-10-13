// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

// The scene: what every pass that draws into the world shares (bind group
// 0: the camera, lights, shadow map and sky) and the helpers that read it
// (fog, shadows, the level-of-detail slide). The editor's own shader is this
// followed by shader.wgsl. A module that draws with its own shader starts its
// source with this too (`module_api::SCENE_WGSL`), so it lights, shadows and
// fogs things exactly as the editor does.

// Camera (shared view-projection matrix), set once per frame.
struct Camera {
    view_proj: mat4x4<f32>,
    // xyz: the camera's position as drawn (relative to the render origin).
    // w: the tide, how far the sea stands over its still level.
    eye: vec4<f32>,
};
@group(0) @binding(0) var<uniform> camera: Camera;

// A small, fixed number of simultaneously active lights, combined
// additively in the fragment shader below. An unused slot has intensity
// 0.0 and contributes nothing, rather than tracking a separate active
// count, the simplest way to keep a fixed-size array without a dynamic
// loop bound. Every field is a vec4 (even where only 3 components are
// used) purely to keep every light's layout a clean 16-byte-aligned
// multiple, sidestepping WGSL's uniform-buffer alignment rules for vec3.
// MUST match LightRaw in main.rs field-for-field.
struct Light {
    // xyz: for a directional light, the direction toward the light; for a
    // point light, its world position. w unused.
    position_or_direction: vec4<f32>,
    // x: kind, 0.0 = directional, 1.0 = point.
    // y: range (point lights only; a linear falloff to zero at this
    //    distance, not physically-accurate inverse-square, a deliberately
    //    simple first pass).
    // z: intensity, a brightness multiplier; 0.0 means "unused slot".
    // w: 1.0 if this light casts the scene's shadow, else 0.0.
    params: vec4<f32>,
};

const MAX_LIGHTS: u32 = 4u;

// Shadows: one shadow map for the first directional light (the slot whose
// params.w is 1.0). MUST match ShadowUniform in main.rs field-for-field.
struct Shadow {
    light_view_proj: mat4x4<f32>,
    // x: 1.0 if shadows are active this frame, else 0.0.
    // y: depth bias (in the light's 0..1 depth range).
    // z: normal-offset bias, in world units.
    // w: size of one shadow-map texel in UV space (1 / map size).
    params: vec4<f32>,
    // The viewing camera, for the shadow pass (see shadow.wgsl).
    eye: vec4<f32>,
};
@group(0) @binding(2) var<uniform> shadow: Shadow;
@group(0) @binding(3) var shadow_map: texture_depth_2d;
@group(0) @binding(4) var shadow_sampler: sampler_comparison;

// The sky and the fog (see sky.rs, where every colour is decided). MUST match
// SkyUniform in sky.rs field-for-field.
struct Sky {
    // Clip space back to world space, to find the direction a pixel looks in.
    inv_view_proj: mat4x4<f32>,
    // xyz: the camera's position as drawn (relative to the render origin).
    // w: the render origin's height above the world's zero.
    eye: vec4<f32>,
    // xyz: unit direction toward the sun. w: how strongly to draw the disc.
    sun_direction: vec4<f32>,
    // rgb: the colour overhead. w: 1.0 if the sky is drawn.
    zenith: vec4<f32>,
    // rgb: the colour at the horizon, also the fog colour. w: fog density
    // per world unit (0 means no fog).
    horizon: vec4<f32>,
    // rgb: the sun's colour.
    sun_colour: vec4<f32>,
    // rgb: the colour looking straight down. w: the ambient light scale (1.0
    // by day; the day-night cycle dims it at night; 0 or less means 1.0).
    ground: vec4<f32>,
    // For liquid surfaces: x is seconds of moving water, y and z the render
    // origin's x and z wrapped to WAVE_PERIOD. w unused.
    motion: vec4<f32>,
    // The clouds: x how much of the sky they cover (0 none, 1 overcast), y
    // and z the render origin's x and z plus how far the wind has carried
    // them, wrapped to CLOUD_PERIOD. w unused.
    clouds: vec4<f32>,
    // xyz: unit direction toward the moon. w: how strongly to draw it (0
    // with no moon).
    moon: vec4<f32>,
    // x: the moon's phase (0 new, 0.5 full). y: its lit fraction. z: how
    // bright the stars are. w: the moon's light at night, 0 to 1.
    night: vec4<f32>,
    // rgb: the colour of the light: white by day, cool blue at night.
    tint: vec4<f32>,
};
@group(0) @binding(5) var<uniform> sky: Sky;

// How quickly fog thins with altitude, in world units per factor of e.
// MUST match FOG_HEIGHT_SCALE in sky.rs.
const FOG_HEIGHT_SCALE: f32 = 1200.0;
// At twilight, how far the haze away from the sun leans toward the colour
// overhead. MUST match AWAY_COOLING in sky.rs.
const AWAY_COOLING: f32 = 0.35;
// How much of the sun's colour the haze toward the sun takes on. MUST match
// SUN_HAZE in sky.rs (and the broad glow in fs_sky uses the same).
const SUN_HAZE: f32 = 0.12;

// The horizon colour looking in `direction` (a unit vector): cooled toward
// the colour overhead on the side away from the sun at twilight
// (sky.sun_colour.w is how much twilight there is).
fn horizon_colour(direction: vec3<f32>) -> vec3<f32> {
    let toward = max(dot(direction, sky.sun_direction.xyz), 0.0);
    let away = (1.0 - toward) * sky.sun_colour.w * AWAY_COOLING;
    return mix(sky.horizon.rgb, sky.zenith.rgb, away);
}

// The haze colour looking in `direction`: the horizon colour there, warmed
// by the sun's broad glow on its side, exactly as the sky draws it, so
// distant ground fades into the sky behind it with no seam. MUST match
// fog_colour in sky.rs.
fn fog_colour(direction: vec3<f32>) -> vec3<f32> {
    let toward = max(dot(direction, sky.sun_direction.xyz), 0.0);
    let glow = pow(toward, 6.0) * SUN_HAZE * sky.sun_direction.w;
    return horizon_colour(direction) + sky.sun_colour.rgb * glow;
}

// Blend a lit colour toward the horizon colour with distance from the camera,
// so far-off ground dissolves into the sky. Exponential squared: clear up
// close, thickening quickly. The fog is thick in the low air and thins with
// altitude, so a summit far away stays visible while lowland the same
// distance off fades out; the thickness is averaged along the line of sight
// (the exact integral of an exponential falloff). Mirrors fog_factor_between
// in sky.rs.
fn apply_fog(colour: vec3<f32>, world_position: vec3<f32>) -> vec3<f32> {
    let density = sky.horizon.w;
    // The near haze (aerial perspective) builds up from close range; the
    // fog, squared, stays clear until far out. See AERIAL_RANGE in sky.rs.
    let aerial = sky.motion.w;
    if (density <= 0.0 && aerial <= 0.0) {
        return colour;
    }
    let k = 1.0 / FOG_HEIGHT_SCALE;
    let h1 = max(sky.eye.y + sky.eye.w, 0.0);
    let h2 = max(world_position.y + sky.eye.w, 0.0);
    var thin = exp(-k * h1);
    if (abs(h2 - h1) > 1.0) {
        thin = (exp(-k * h1) - exp(-k * h2)) / (k * (h2 - h1));
    }
    let offset = world_position - sky.eye.xyz;
    let distance = length(offset);
    let x = distance * density * thin;
    let amount = 1.0 - exp(-(x * x) - distance * aerial * thin);
    let direction = offset / max(distance, 1e-6);
    return mix(colour, fog_colour(direction), amount);
}

// 1.0 = fully lit, 0.0 = fully in shadow. Anything outside the shadow map's
// area counts as lit. A 3x3 grid of comparison samples (each of which is
// itself bilinear-filtered by the sampler) softens the edge. Uses the
// "Level" sample variant, which has no uniform-control-flow requirement, so
// it is safe to call from inside the light loop.
fn shadow_factor(world_position: vec3<f32>, normal: vec3<f32>) -> f32 {
    if (shadow.params.x < 0.5) {
        return 1.0;
    }
    let offset_position = world_position + normal * shadow.params.z;
    let clip = shadow.light_view_proj * vec4<f32>(offset_position, 1.0);
    let ndc = clip.xyz / clip.w;
    let uv = vec2<f32>(ndc.x * 0.5 + 0.5, 0.5 - ndc.y * 0.5);
    if (uv.x < 0.0 || uv.x > 1.0 || uv.y < 0.0 || uv.y > 1.0 || ndc.z > 1.0 || ndc.z < 0.0) {
        return 1.0;
    }
    let reference = ndc.z - shadow.params.y;
    let texel = shadow.params.w;
    var sum: f32 = 0.0;
    for (var dy: i32 = -1; dy <= 1; dy = dy + 1) {
        for (var dx: i32 = -1; dx <= 1; dx = dx + 1) {
            let offset = vec2<f32>(f32(dx), f32(dy)) * texel;
            sum = sum + textureSampleCompareLevel(shadow_map, shadow_sampler, uv + offset, reference);
        }
    }
    return sum / 9.0;
}

// The colour of the light on everything lit this frame (see `tint` in Sky):
// multiply a lit colour by it. White when the sky has not set it.
fn light_tint() -> vec3<f32> {
    let t = sky.tint.rgb;
    return select(t, vec3<f32>(1.0, 1.0, 1.0), t.r + t.g + t.b <= 0.0);
}

struct Lights {
    lights: array<Light, 4>,
};
@group(0) @binding(1) var<uniform> lights: Lights;

// How far through its morph a vertex at `world` is, 0 to 1, seen from `eye`.
// Distance is across the ground, the larger of x and z, so the bands are
// squares. MUST match `morph_amount` in shadow.wgsl.
fn morph_amount(world: vec3<f32>, eye: vec3<f32>, band: vec2<f32>) -> f32 {
    if (band.y <= band.x) {
        return 0.0;
    }
    let d = max(abs(world.x - eye.x), abs(world.z - eye.z));
    return clamp((d - band.x) / (band.y - band.x), 0.0, 1.0);
}
