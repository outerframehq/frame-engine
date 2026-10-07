// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

// Camera (shared view-projection matrix), set once per frame.
struct Camera {
    view_proj: mat4x4<f32>,
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
};
@group(0) @binding(2) var<uniform> shadow: Shadow;
@group(0) @binding(3) var shadow_map: texture_depth_2d;
@group(0) @binding(4) var shadow_sampler: sampler_comparison;

// The sky and the fog (see sky.rs, where every colour is decided). MUST match
// SkyUniform in sky.rs field-for-field.
struct Sky {
    // Clip space back to world space, to find the direction a pixel looks in.
    inv_view_proj: mat4x4<f32>,
    // xyz: the camera's world position.
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
    // rgb: the colour looking straight down.
    ground: vec4<f32>,
};
@group(0) @binding(5) var<uniform> sky: Sky;

// Blend a lit colour toward the horizon colour with distance from the camera,
// so far-off ground dissolves into the sky. Exponential squared: clear up
// close, thickening quickly. Mirrors fog_factor in sky.rs.
fn apply_fog(colour: vec3<f32>, world_position: vec3<f32>) -> vec3<f32> {
    let density = sky.horizon.w;
    if (density <= 0.0) {
        return colour;
    }
    let x = length(world_position - sky.eye.xyz) * density;
    let amount = 1.0 - exp(-(x * x));
    return mix(colour, sky.horizon.rgb, amount);
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
    // w unused.
    params: vec4<f32>,
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
};

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

@vertex
fn vs_main(vertex: VertexInput, instance: InstanceInput) -> VertexOutput {
    // Per-axis scale (component-wise), then the entity's rotation, then place
    // at the entity's position. Scale first so rotation spins the
    // already-sized shape rather than an elongated axis.
    let scaled = vertex.position * MESH_SIZE * instance.scale;
    let rotated = rotate_by(scaled, instance.rotation);
    let world_pos = instance.position + rotated;

    // The mesh's own normal, rotated the same way the shape was, so shading
    // stays correct as an entity turns. A diagonal scale leaves an
    // axis-aligned cube normal untouched, and a uniformly-scaled sphere keeps
    // correct normals too; a *non-uniformly* scaled sphere shades
    // approximately, which is fine here. Actual lighting now happens per
    // fragment, not here, so a point light's falloff varies smoothly across
    // a face instead of only being evaluated at each corner.
    let rotated_normal = rotate_by(vertex.normal, instance.rotation);

    var out: VertexOutput;
    out.clip_position = camera.view_proj * vec4<f32>(world_pos, 1.0);
    out.color = instance.color;
    out.selected = instance.selected;
    out.emissive = instance.emissive;
    out.world_position = world_pos;
    out.world_normal = rotated_normal;
    out.uv = vertex.uv;
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
    let roughness = material.params.x;
    let metalness = material.params.y;

    // Ambient floor, the same value the old single hard-coded term always
    // used, plus every active light's diffuse contribution, summed. A scene
    // with no Light entities at all falls back to just this floor, flatter
    // than before, a real, one-time visible change on upgrade rather than a
    // hidden fallback light.
    var accumulated: f32 = 0.4;
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
    let shade_metal_adjusted = min(shade - 0.4 * metalness, 1.0);

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
    let lit = apply_fog(shade_fragment(in), in.world_position);
    let thin = 1.0 - clamp(in.uv.y, 0.0, 1.0);
    let colour = min(lit + vec3<f32>(0.22, 0.38, 0.34) * thin, vec3<f32>(1.0, 1.0, 1.0));
    let alpha = clamp(in.uv.x, 0.0, 1.0) * (1.0 - 0.85 * thin);
    return vec4<f32>(colour, alpha);
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
    // The direction this pixel looks in: from the eye to its far-plane point.
    let far = sky.inv_view_proj * vec4<f32>(in.ndc, 1.0, 1.0);
    let direction = normalize(far.xyz / far.w - sky.eye.xyz);
    let up = direction.y;

    // Horizon to zenith, quick at first so the colour climbs off the horizon,
    // then below the horizon down to the darker ground haze.
    let above = pow(clamp(up, 0.0, 1.0), 0.45);
    var colour = mix(sky.horizon.rgb, sky.zenith.rgb, above);
    colour = mix(colour, sky.ground.rgb, smoothstep(0.0, 0.25, -up));

    // The sun: a soft-edged disc with a glow around it. The disc is about
    // two degrees across, bigger than the real one on purpose.
    let cos_angle = dot(direction, sky.sun_direction.xyz);
    let strength = sky.sun_direction.w;
    let disc = smoothstep(0.9993, 0.9998, cos_angle) * strength;
    let toward = max(cos_angle, 0.0);
    let glow = (pow(toward, 48.0) * 0.45 + pow(toward, 6.0) * 0.12) * strength;
    colour = mix(colour, sky.sun_colour.rgb * 1.4, disc) + sky.sun_colour.rgb * glow;
    return vec4<f32>(min(colour, vec3<f32>(1.0, 1.0, 1.0)), 1.0);
}
