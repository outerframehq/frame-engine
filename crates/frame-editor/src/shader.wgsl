// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

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
    // The cloud layer: x how much of the sky it covers (0 none, 1 overcast),
    // y and z the render origin's x and z wrapped to CLOUD_PERIOD. w unused.
    clouds: vec4<f32>,
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

struct Lights {
    lights: array<Light, 4>,
};
@group(0) @binding(1) var<uniform> lights: Lights;

// This mesh's own material: a base-color texture (bound below) plus
// roughness/metalness, one per imported model rather than per entity (see
// world::MeshMaterial's doc comment in main.rs/world/mod.rs for why). Bound
// fresh before each mesh's draw call, since different meshes in the same
// frame can have different materials.
struct MaterialParams {
    // x: roughness (0 smooth .. 1 rough). y: metalness (0 .. 1 metal).
    // z: 1.0 if material_texture holds a real texture, 0.0 for the shared
    // default (a textureless mesh samples pure white, a harmless no-op).
    // w: how choppy a liquid surface's waves are (0: not a liquid).
    params: vec4<f32>,
    // x: ground detail, 0 to 1 (see MaterialLook). yzw unused.
    extra: vec4<f32>,
};
@group(1) @binding(0) var material_texture: texture_2d<f32>;
@group(1) @binding(1) var material_sampler: sampler;
@group(1) @binding(2) var<uniform> material: MaterialParams;

// Per-vertex mesh data (matches MeshVertex in main.rs). Position is in the
// primitive's local space, roughly unit-sized, centred on the origin, and is
// blown up to world size below. Bound at vertex-buffer slot 0.
struct VertexInput {
    @location(0) position: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) uv: vec2<f32>,
    // x: height offset when fully morphed. y, z: the distance band. All zero
    // for a mesh that does not morph (see VertexMorph in module_api.rs).
    @location(9) morph: vec3<f32>,
    @location(10) morph_normal: vec3<f32>,
    // The texture coordinates when fully morphed; equal to `uv` otherwise.
    @location(11) morph_uv: vec2<f32>,
    // For liquid surfaces (see WaterFlow in module_api.rs), each -1 to 1:
    // x and z the velocity over FLOW_RANGE, then the tide rule (1 follows the
    // tide, -1 floods where the tide is higher, 0 still), then for flooding
    // water the still sea level less the vertex's height, over TIDE_REACH.
    @location(12) water: vec4<f32>,
};

// MUST match FLOW_RANGE and TIDE_REACH in module_api.rs.
const FLOW_RANGE: f32 = 16.0;
const TIDE_REACH: f32 = 512.0;

// Per-entity instance data (matches InstanceRaw in main.rs). Bound at slot 1.
// Locations continue after the mesh attributes above.
struct InstanceInput {
    @location(3) position: vec3<f32>,
    @location(4) color: vec3<f32>,
    @location(5) selected: f32,
    @location(6) scale: vec3<f32>,
    @location(7) emissive: f32,
    // x: yaw, y: pitch, z: roll, in radians (world::Rotation).
    @location(8) rotation: vec3<f32>,
};

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) color: vec3<f32>,
    @location(1) selected: f32,
    @location(2) emissive: f32,
    @location(3) world_position: vec3<f32>,
    @location(4) world_normal: vec3<f32>,
    @location(5) uv: vec2<f32>,
    // A liquid surface's velocity across the ground (x, z), world units a
    // second. Zero for everything else.
    @location(6) flow: vec2<f32>,
};

// World size of a primitive at scale 1. Primitives are generated at ~unit size
// in main.rs and scaled up by this, so a default entity is exactly the size the
// cube always was. NOTE: must match MESH_SIZE in main.rs (render and pick agree).
const MESH_SIZE: f32 = 8.0;

// Rotate a vector from the entity's own space into world space by yaw, pitch
// and roll: roll first (around the forward axis), then pitch (around the
// left-right axis), then yaw (around the vertical axis). Yaw turns clockwise
// seen from above (yaw 0 faces -Z, yaw pi/2 faces +X); positive pitch tips the
// nose up; positive roll leans the right side down. With pitch and roll at 0
// this is exactly the old yaw-only rotation. MUST match `Rotation::matrix` in
// the engine's world module (and the same function in shadow.wgsl).
fn rotate_by(v: vec3<f32>, angles: vec3<f32>) -> vec3<f32> {
    let cr = cos(angles.z);
    let sr = sin(angles.z);
    let rolled = vec3<f32>(v.x * cr + v.y * sr, -v.x * sr + v.y * cr, v.z);
    let cp = cos(angles.y);
    let sp = sin(angles.y);
    let pitched = vec3<f32>(rolled.x, rolled.y * cp - rolled.z * sp, rolled.y * sp + rolled.z * cp);
    let cy = cos(angles.x);
    let sy = sin(angles.x);
    return vec3<f32>(pitched.x * cy - pitched.z * sy, pitched.y, pitched.x * sy + pitched.z * cy);
}

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

@vertex
fn vs_main(vertex: VertexInput, instance: InstanceInput) -> VertexOutput {
    // Per-axis scale (component-wise), then the entity's rotation, then place
    // at the entity's position. Scale first so rotation spins the
    // already-sized shape rather than an elongated axis.
    let unmorphed = instance.position
        + rotate_by(vertex.position * MESH_SIZE * instance.scale, instance.rotation);
    // A level-of-detail mesh slides toward its coarser shape with distance.
    let t = morph_amount(unmorphed, camera.eye.xyz, vertex.morph.yz);
    let local = vertex.position + vec3<f32>(0.0, vertex.morph.x * t, 0.0);
    let scaled = local * MESH_SIZE * instance.scale;
    let rotated = rotate_by(scaled, instance.rotation);
    var world_pos = instance.position + rotated;
    // Liquid surfaces follow the tide: the sea rises and falls with it, and
    // water it can reach is lifted where it stands higher.
    let size = MESH_SIZE * instance.scale.y;
    let tide = camera.eye.w;
    if (vertex.water.z > 0.5) {
        world_pos.y = world_pos.y + tide;
    } else if (vertex.water.z < -0.5) {
        world_pos.y = world_pos.y + max(vertex.water.w * TIDE_REACH * size + tide, 0.0);
    }
    var normal = vertex.normal;
    if (t > 0.0) {
        normal = normalize(mix(vertex.normal, vertex.morph_normal, t));
    }

    // The mesh's own normal, rotated the same way the shape was, so shading
    // stays correct as an entity turns. A diagonal scale leaves an
    // axis-aligned cube normal untouched, and a uniformly-scaled sphere keeps
    // correct normals too; a *non-uniformly* scaled sphere shades
    // approximately, which is fine here. Actual lighting now happens per
    // fragment, not here, so a point light's falloff varies smoothly across
    // a face instead of only being evaluated at each corner.
    let rotated_normal = rotate_by(normal, instance.rotation);

    var out: VertexOutput;
    out.clip_position = camera.view_proj * vec4<f32>(world_pos, 1.0);
    out.color = instance.color;
    out.selected = instance.selected;
    out.emissive = instance.emissive;
    out.world_position = world_pos;
    out.world_normal = rotated_normal;
    out.uv = mix(vertex.uv, vertex.morph_uv, t);
    let velocity = vec3<f32>(vertex.water.x, 0.0, vertex.water.y) * FLOW_RANGE;
    out.flow = rotate_by(velocity * MESH_SIZE * instance.scale, instance.rotation).xz;
    return out;
}

// The lit colour of one fragment, shared by the opaque and transparent entry
// points below.
fn shade_fragment(in: VertexOutput) -> vec3<f32> {
    // Base color: the entity's own vertex color, tinted by this mesh's
    // texture when it has one (textureSample reads (1,1,1,1) from the
    // shared default texture, a harmless no-op, when it doesn't).
    let has_texture = material.params.z;
    var base_color = in.color;
    if (has_texture > 0.5) {
        let sampled = textureSample(material_texture, material_sampler, in.uv);
        base_color = base_color * sampled.rgb;
    }
    let detail = material.extra.x;
    if (detail > 0.0) {
        base_color = base_color * ground_detail(in.world_position, detail);
    }
    let roughness = material.params.x;
    let metalness = material.params.y;

    // Ambient floor, the same value the old single hard-coded term always
    // used, plus every active light's diffuse contribution, summed. A scene
    // with no Light entities at all falls back to just this floor, flatter
    // than before, a real, one-time visible change on upgrade rather than a
    // hidden fallback light.
    // The ambient floor, scaled by the sky uniform's ambient scale: 1.0 unless
    // the day-night cycle has dimmed it for night (a zero means "not set").
    let ambient = 0.4 * select(sky.ground.w, 1.0, sky.ground.w <= 0.0);
    var accumulated: f32 = ambient;
    let normal = normalize(in.world_normal);
    for (var i: u32 = 0u; i < MAX_LIGHTS; i = i + 1u) {
        let light = lights.lights[i];
        let intensity = light.params.z;
        if (intensity <= 0.0) {
            continue;
        }
        let kind = light.params.x;
        var light_dir: vec3<f32>;
        var attenuation: f32 = 1.0;
        if (kind < 0.5) {
            // Directional: a fixed direction, no falloff with distance.
            light_dir = normalize(light.position_or_direction.xyz);
        } else {
            // Point: falloff linearly to zero at `range`.
            let to_light = light.position_or_direction.xyz - in.world_position;
            let dist = length(to_light);
            let range = max(light.params.y, 0.0001);
            light_dir = to_light / max(dist, 0.0001);
            attenuation = clamp(1.0 - dist / range, 0.0, 1.0);
        }
        var diffuse = max(dot(normal, light_dir), 0.0);
        // Roughness/metalness tweak, NOT a real specular highlight (that
        // needs the camera's world position, not available here yet — see
        // MeshMaterial's doc comment). A rough surface's lit side reads a
        // little flatter/brighter (less falloff contrast); a metal surface's
        // lit side reads punchier (more contrast, tinted toward its own
        // color rather than the light's), approximating "matte" versus
        // "shiny metal" without a real BRDF.
        diffuse = pow(diffuse, mix(1.4, 0.7, roughness));
        // The shadow-casting light only reaches fragments the shadow map says
        // it can see. The ambient floor is untouched, so shadows are never
        // pitch black.
        if (light.params.w > 0.5) {
            diffuse = diffuse * shadow_factor(in.world_position, normal);
        }
        accumulated = accumulated + diffuse * attenuation * intensity * mix(1.0, 1.3, metalness);
    }
    let shade = min(accumulated, 1.0);

    // Metals have essentially no diffuse ambient bounce, so their unlit side
    // reads darker than a non-metal's; scale just the ambient floor down by
    // metalness rather than the whole shaded result, so a lit metal doesn't
    // simply look dimmer overall.
    let shade_metal_adjusted = min(shade - ambient * metalness, 1.0);

    // Each entity draws in its own (possibly textured) colour. Emissive
    // blends between normal shading and full unlit brightness, so a high
    // emissive value makes an entity glow, ignoring every light. The
    // selected entity is then brightened toward white so it stands out.
    let shaded = base_color * shade_metal_adjusted;

    let lit = mix(shaded, base_color, in.emissive);
    let highlighted = mix(lit, vec3<f32>(1.0, 1.0, 1.0), 0.3 * in.selected);
    return highlighted;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    return vec4<f32>(apply_fog(shade_fragment(in), in.world_position), 1.0);
}

// Transparent module geometry (see ModuleGpu::draw_transparent), lit like
// everything else. uv.x is the opacity (0 clear to 1 solid). uv.y is the
// thickness, 0 to 1: thin geometry lightens and fades out, thick geometry
// keeps its colour and opacity. Water uses it for depth, so the shallows go
// pale and clear and the shoreline dissolves instead of ending in a hard line.
// A mesh that doesn't want this sets uv.y to 1.
@fragment
fn fs_transparent(in: VertexOutput) -> @location(0) vec4<f32> {
    let thin = 1.0 - clamp(in.uv.y, 0.0, 1.0);
    let alpha = clamp(in.uv.x, 0.0, 1.0) * (1.0 - 0.85 * thin);
    let ripples = material.params.w;
    if (ripples > 0.0) {
        let surface = liquid(in, thin, alpha, ripples);
        return vec4<f32>(apply_fog(surface.rgb, in.world_position), surface.a);
    }
    let lit = apply_fog(shade_fragment(in), in.world_position);
    let colour = min(lit + vec3<f32>(0.22, 0.38, 0.34) * thin, vec3<f32>(1.0, 1.0, 1.0));
    return vec4<f32>(colour, alpha);
}

// Liquid surfaces (a material whose params.w, the ripples, is above 0; see
// MaterialFactory::create_liquid). Waves repeat exactly every WAVE_PERIOD
// units along x and z: each one runs a whole number of cycles across it. That
// lets the shader take the render origin wrapped to the period, so the waves
// never jump when the origin moves. MUST match WAVE_PERIOD in lib.rs.
const WAVE_PERIOD: f32 = 4096.0;
// Running water's ripples: each layer is carried for this many seconds (it
// MUST divide WAVE_WRAP in lib.rs, so the cycle never jumps when the water's
// clock wraps), drawn this many times finer than the sea's, and break white
// from this speed (world units a second) up to twice it.
const FLOW_CYCLE: f32 = 2.0;
const RUNNING_SCALE: f32 = 3.0;
const RAPID_SPEED: f32 = 3.0;
const TAU: f32 = 6.2831853;

// The slope (x and z) of one wave at `p` after `t` seconds. `cycles` is how
// many times it repeats across WAVE_PERIOD along x and z, `slope` how steep
// it gets. It moves at the speed of a wave that long on deep water.
fn wave_slope(p: vec2<f32>, t: f32, cycles: vec2<f32>, slope: f32) -> vec2<f32> {
    let k = cycles * (TAU / WAVE_PERIOD);
    let omega = sqrt(9.81 * length(k));
    return normalize(k) * slope * cos(dot(k, p) - omega * t);
}

// The surface normal of moving water at `p` (true world x and z, wrapped),
// `amount` 0 (flat) to 1. Nine waves from about 90 m long down to 3 m, across
// each other at odd angles so no pattern shows.
fn liquid_normal(p: vec2<f32>, t: f32, amount: f32) -> vec3<f32> {
    var g = wave_slope(p, t, vec2<f32>(40.0, 18.0), 0.03);
    g = g + wave_slope(p, t, vec2<f32>(-25.0, 52.0), 0.03);
    g = g + wave_slope(p, t, vec2<f32>(75.0, -130.0), 0.03);
    g = g + wave_slope(p, t, vec2<f32>(110.0, 170.0), 0.03);
    g = g + wave_slope(p, t, vec2<f32>(-260.0, -150.0), 0.028);
    g = g + wave_slope(p, t, vec2<f32>(-180.0, 340.0), 0.026);
    g = g + wave_slope(p, t, vec2<f32>(430.0, 310.0), 0.024);
    g = g + wave_slope(p, t, vec2<f32>(-700.0, 560.0), 0.02);
    g = g + wave_slope(p, t, vec2<f32>(1210.0, -330.0), 0.018);
    g = g * amount;
    return normalize(vec3<f32>(-g.x, 1.0, -g.y));
}

// Foam on running water at `p`, 0 to 1: noise stretched `STREAK` times along
// the flow (`along`, a unit vector), in two sizes, so it breaks into streaks
// the way fast water does rather than round patches.
fn white_water(p: vec2<f32>, along: vec2<f32>) -> f32 {
    let across = vec2<f32>(-along.y, along.x);
    let q = vec2<f32>(dot(p, across), dot(p, along) / STREAK);
    return 0.65 * lattice_noise(q, 1.6, WAVE_PERIOD) + 0.35 * lattice_noise(q, 0.6, WAVE_PERIOD);
}
const STREAK: f32 = 4.0;

// The sky's colour looking in `direction`, without the sun's disc: the same
// gradient fs_sky draws, with the sun's broad glow.
fn sky_gradient(direction: vec3<f32>) -> vec3<f32> {
    let up = direction.y;
    let above = pow(clamp(up, 0.0, 1.0), 0.45);
    var colour = mix(horizon_colour(direction), sky.zenith.rgb, above);
    colour = mix(colour, sky.ground.rgb, smoothstep(0.0, 0.25, -up));
    let toward = max(dot(direction, sky.sun_direction.xyz), 0.0);
    let glow = (pow(toward, 48.0) * 0.45 + pow(toward, 6.0) * SUN_HAZE) * sky.sun_direction.w;
    return colour + sky.sun_colour.rgb * glow;
}

// A liquid surface's colour and opacity, before fog. Lit with the moving
// waves' normal, tinted pale where thin, then mirrors the sky more the lower
// the view (Fresnel), plus a glint of the sun.
fn liquid(in: VertexOutput, thin: f32, alpha: f32, ripples: f32) -> vec4<f32> {
    // The eye from the sky uniform: the camera uniform is the vertex stage's
    // only.
    let to_eye = sky.eye.xyz - in.world_position;
    let dist = max(length(to_eye), 1e-3);
    let v = to_eye / dist;
    // Waves too small to see far away would only shimmer, so they calm down
    // with distance.
    let calm = 1.0 - smoothstep(400.0, 6000.0, dist);
    let p_wave = in.world_position.xz + sky.motion.yz;
    var n = liquid_normal(p_wave, sky.motion.x, ripples * calm);
    // Running water: finer ripples carried along with the flow. Two layers,
    // half a cycle apart, each carried for one cycle and then started again,
    // weighted so each fades out before it restarts: the ripples move
    // downstream without ever stretching.
    let speed = length(in.flow);
    let running = smoothstep(0.05, 0.4, speed);
    var white = 0.0;
    if (running > 0.0) {
        let phase = fract(sky.motion.x / FLOW_CYCLE);
        let phase_b = fract(phase + 0.5);
        let carried = in.flow * FLOW_CYCLE;
        let still = sky.motion.x * 0.15;
        let na = liquid_normal((p_wave - carried * phase) * RUNNING_SCALE, still, ripples * calm);
        let nb = liquid_normal((p_wave - carried * phase_b) * RUNNING_SCALE, still, ripples * calm);
        let weight = 1.0 - abs(2.0 * phase - 1.0);
        let flowing = normalize(na * weight + nb * (1.0 - weight));
        n = normalize(mix(n, flowing, running));
        // White water where it runs fast: streaks drawn out along the flow,
        // carried with it.
        let rapid = smoothstep(RAPID_SPEED, 2.0 * RAPID_SPEED, speed)
            * (1.0 - smoothstep(300.0, 3000.0, dist));
        if (rapid > 0.0) {
            let along = in.flow / speed;
            let foam_a = white_water(p_wave - carried * phase, along);
            let foam_b = white_water(p_wave - carried * phase_b, along);
            white = rapid * smoothstep(0.42, 0.72, foam_a * weight + foam_b * (1.0 - weight));
        }
    }
    var surface = in;
    surface.world_normal = n;
    let lit = min(shade_fragment(surface) + vec3<f32>(0.22, 0.38, 0.34) * thin, vec3<f32>(1.0, 1.0, 1.0));
    // Schlick's Fresnel for water: 2% straight down, all of it at the horizon.
    let facing = clamp(dot(n, v), 0.0, 1.0);
    let fresnel = 0.02 + 0.98 * pow(1.0 - facing, 5.0);
    var r = reflect(-v, n);
    r = normalize(vec3<f32>(r.x, max(r.y, 0.01), r.z));
    let mirrored = with_clouds(sky_gradient(r), r);
    // The sun's glint: a tight spot up close, a broad path far out, where the
    // many small waves it would really sparkle on are too small to draw.
    let far = smoothstep(200.0, 20000.0, dist);
    let sharpness = mix(1200.0, 60.0, far);
    let glint = pow(max(dot(r, sky.sun_direction.xyz), 0.0), sharpness)
        * mix(4.0, 0.8, far) * sky.sun_direction.w;
    var colour = mix(lit, mirrored, fresnel) + sky.sun_colour.rgb * glint;
    // The mirror fades out with the surface itself: where a mesh fades its
    // edge out (uv.x near 0) and at the very waterline, so a low view does
    // not show the edge's triangles as a hard, bright line.
    let present = smoothstep(0.0, 0.25, in.uv.x) * (1.0 - smoothstep(0.75, 1.0, thin));
    var opacity = clamp(max(alpha, fresnel * present) + glint * present, 0.0, 1.0);
    if (white > 0.0) {
        let daylight = clamp(sky.sun_direction.y * 4.0 + 0.4, 0.0, 1.0);
        let foam = mix(vec3<f32>(0.93, 0.96, 0.98), sky.sun_colour.rgb, 0.25)
            * (0.35 + 0.65 * daylight);
        colour = mix(colour, foam, white);
        opacity = max(opacity, white * 0.9);
    }
    // Surf: bands of foam rolling in over the shallowest water, toward the
    // shore (where the water thins out), broken up by noise fixed to the
    // world. Gone far off, like the waves.
    let shallow = smoothstep(0.3, 0.85, thin) * (1.0 - smoothstep(300.0, 2500.0, dist));
    if (shallow > 0.0) {
        let p = in.world_position.xz + sky.motion.yz;
        let roll = 0.5 + 0.5 * sin(thin * 26.0 - sky.motion.x * 1.4);
        let breakup = 0.6 * lattice_noise(p, 8.0, WAVE_PERIOD)
            + 0.4 * lattice_noise(p + vec2<f32>(sky.motion.x * 0.6, 0.0), 2.0, WAVE_PERIOD);
        let foam = shallow * smoothstep(0.5, 0.8, 0.55 * roll + 0.6 * breakup);
        let daylight = clamp(sky.sun_direction.y * 4.0 + 0.4, 0.0, 1.0);
        let white = mix(vec3<f32>(0.93, 0.96, 0.98), sky.sun_colour.rgb, 0.25);
        let foam_colour = white * (0.35 + 0.65 * daylight);
        colour = mix(colour, foam_colour, foam);
        opacity = max(opacity, foam * 0.9);
    }
    return vec4<f32>(colour, opacity);
}

// The sky: one triangle that covers the whole screen, drawn first with no
// depth, so everything else draws over it.
struct SkyVarying {
    @builtin(position) position: vec4<f32>,
    @location(0) ndc: vec2<f32>,
};

@vertex
fn vs_sky(@builtin(vertex_index) index: u32) -> SkyVarying {
    // Vertices (-1,-1), (1,-1) and (-1,3): a triangle larger than the screen.
    let x = f32((index << 1u) & 2u);
    let y = f32(index & 2u);
    let ndc = vec2<f32>(x * 2.0 - 1.0, y * 2.0 - 1.0);
    var out: SkyVarying;
    out.position = vec4<f32>(ndc, 1.0, 1.0);
    out.ndc = ndc;
    return out;
}

@fragment
fn fs_sky(in: SkyVarying) -> @location(0) vec4<f32> {
    // The direction this pixel looks in: from the eye to a point down its
    // line of sight. Depth is reversed with no far plane, and depth 0.0001 is
    // ten thousand near-plane distances out (1 km), far enough for an exact
    // direction.
    let far = sky.inv_view_proj * vec4<f32>(in.ndc, 0.0001, 1.0);
    let direction = normalize(far.xyz / far.w - sky.eye.xyz);
    let up = direction.y;

    // Horizon to zenith, quick at first so the colour climbs off the horizon,
    // then below the horizon down to the darker ground haze.
    let above = pow(clamp(up, 0.0, 1.0), 0.45);
    var colour = mix(horizon_colour(direction), sky.zenith.rgb, above);
    colour = mix(colour, sky.ground.rgb, smoothstep(0.0, 0.25, -up));

    // The sun: a soft-edged disc with a glow around it. The disc is about
    // two degrees across, bigger than the real one on purpose.
    let cos_angle = dot(direction, sky.sun_direction.xyz);
    let strength = sky.sun_direction.w;
    let disc = smoothstep(0.9993, 0.9998, cos_angle) * strength;
    let toward = max(cos_angle, 0.0);
    let glow = (pow(toward, 48.0) * 0.45 + pow(toward, 6.0) * SUN_HAZE) * strength;
    colour = mix(colour, sky.sun_colour.rgb * 1.4, disc) + sky.sun_colour.rgb * glow;
    colour = with_clouds(colour, direction);
    return vec4<f32>(min(colour, vec3<f32>(1.0, 1.0, 1.0)), 1.0);
}

// The cloud layer: a flat sheet at CLOUD_HEIGHT (world height), its pattern
// from noise that repeats exactly every CLOUD_PERIOD units, so the render
// origin can be handed in wrapped to it. It drifts on the wind while the
// world moves (the same clock as the waves). MUST match CLOUD_PERIOD in
// lib.rs.
const CLOUD_HEIGHT: f32 = 2500.0;
const CLOUD_PERIOD: f32 = 262144.0;
const CLOUD_WIND: vec2<f32> = vec2<f32>(6.0, 2.0);

// A random number from 0 to 1 for a lattice point.
fn lattice_hash(x: i32, z: i32) -> f32 {
    var h = u32(x) * 374761393u + u32(z) * 668265263u;
    h = (h ^ (h >> 13u)) * 1274126177u;
    h = h ^ (h >> 16u);
    return f32(h & 0xffffffu) / 16777215.0;
}

// Smooth noise, 0 to 1, on a lattice `cell` units apart that wraps every
// `period` units (`cell` must divide it), so a position wrapped to the period
// gives the same noise as the real one.
fn lattice_noise(p: vec2<f32>, cell: f32, period: f32) -> f32 {
    let n = i32(period / cell);
    let q = p / cell;
    let i = floor(q);
    let f = q - i;
    let u = f * f * (3.0 - 2.0 * f);
    let x0 = ((i32(i.x) % n) + n) % n;
    let z0 = ((i32(i.y) % n) + n) % n;
    let x1 = (x0 + 1) % n;
    let z1 = (z0 + 1) % n;
    let top = mix(lattice_hash(x0, z0), lattice_hash(x1, z0), u.x);
    let bottom = mix(lattice_hash(x0, z1), lattice_hash(x1, z1), u.x);
    return mix(top, bottom, u.y);
}

// Cloud noise wraps with the clouds.
fn cloud_noise(p: vec2<f32>, cell: f32) -> f32 {
    return lattice_noise(p, cell, CLOUD_PERIOD);
}

// Ground detail (MaterialLook::detail): a brightness factor around 1 from
// noise at 256, 64, 16 and 4 units, fixed to the world (the position wrapped
// to WAVE_PERIOD, which every size divides). Each size fades out with
// distance before it gets smaller than a few pixels, so it never shimmers.
fn ground_detail(world: vec3<f32>, detail: f32) -> f32 {
    let p = world.xz + sky.motion.yz;
    let d = length(sky.eye.xyz - world);
    let a = (lattice_noise(p, 256.0, WAVE_PERIOD) - 0.5) * (1.0 - smoothstep(6000.0, 25000.0, d));
    let b = (lattice_noise(p, 64.0, WAVE_PERIOD) - 0.5) * (1.0 - smoothstep(1500.0, 6000.0, d));
    let c = (lattice_noise(p, 16.0, WAVE_PERIOD) - 0.5) * (1.0 - smoothstep(400.0, 1500.0, d));
    let e = (lattice_noise(p, 4.0, WAVE_PERIOD) - 0.5) * (1.0 - smoothstep(100.0, 400.0, d));
    return 1.0 + detail * (0.45 * a + 0.4 * b + 0.3 * c + 0.25 * e);
}

// How much cloud there is at `p` on the sheet, 0 to 1: big billows with
// smaller ones on their edges.
fn cloud_density(p: vec2<f32>) -> f32 {
    return 0.5 * cloud_noise(p, 8192.0)
        + 0.25 * cloud_noise(p, 4096.0)
        + 0.15 * cloud_noise(p, 2048.0)
        + 0.1 * cloud_noise(p, 512.0);
}

// `colour`, the sky looking in `direction`, with the cloud layer over it.
// Clouds are lit by the sun's colour on top and take the sky's colour in
// their thick middles, so they glow warm at dawn and dusk and go dark at
// night. Far off they fade into the horizon.
fn with_clouds(colour: vec3<f32>, direction: vec3<f32>) -> vec3<f32> {
    let cover = sky.clouds.x;
    let eye_height = sky.eye.y + sky.eye.w;
    if (cover <= 0.0 || direction.y <= 0.0 || eye_height >= CLOUD_HEIGHT) {
        return colour;
    }
    let t = (CLOUD_HEIGHT - eye_height) / direction.y;
    let p = sky.eye.xz + direction.xz * t + sky.clouds.yz + CLOUD_WIND * sky.motion.x;
    // Stretch the noise's spread so the cover setting really ranges from
    // a few puffs to overcast.
    let d = clamp((cloud_density(p) - 0.5) * 2.2 + 0.5, 0.0, 1.0);
    let edge = 1.0 - cover;
    let amount = smoothstep(edge - 0.05, edge + 0.12, d);
    if (amount <= 0.0) {
        return colour;
    }
    let core = smoothstep(edge + 0.1, edge + 0.5, d);
    let daylight = clamp(sky.sun_direction.y * 4.0 + 0.4, 0.0, 1.0);
    let toward = max(dot(direction, sky.sun_direction.xyz), 0.0);
    let sunlit = sky.sun_colour.rgb * (0.85 + 0.6 * pow(toward, 4.0)) * daylight;
    let shaded = mix(sky.zenith.rgb, horizon_colour(direction), 0.5) * 0.85;
    let cloud = mix(max(sunlit, shaded), shaded, core * 0.65);
    let fade = exp(-t / 90000.0);
    return mix(colour, cloud, amount * fade * 0.95);
}
