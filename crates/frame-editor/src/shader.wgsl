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
    // w unused.
    params: vec4<f32>,
};

const MAX_LIGHTS: u32 = 4u;

struct Lights {
    lights: array<Light, 4>,
};
@group(0) @binding(1) var<uniform> lights: Lights;

// Per-vertex mesh data (matches MeshVertex in main.rs). Position is in the
// primitive's local space, roughly unit-sized, centred on the origin, and is
// blown up to world size below. Bound at vertex-buffer slot 0.
struct VertexInput {
    @location(0) position: vec3<f32>,
    @location(1) normal: vec3<f32>,
};

// Per-entity instance data (matches InstanceRaw in main.rs). Bound at slot 1.
// Locations continue after the mesh attributes above.
struct InstanceInput {
    @location(2) position: vec3<f32>,
    @location(3) color: vec3<f32>,
    @location(4) selected: f32,
    @location(5) scale: vec3<f32>,
    @location(6) emissive: f32,
    @location(7) yaw: f32,
};

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) color: vec3<f32>,
    @location(1) selected: f32,
    @location(2) emissive: f32,
    @location(3) world_position: vec3<f32>,
    @location(4) world_normal: vec3<f32>,
};

// World size of a primitive at scale 1. Primitives are generated at ~unit size
// in main.rs and scaled up by this, so a default entity is exactly the size the
// cube always was. NOTE: must match MESH_SIZE in main.rs (render and pick agree).
const MESH_SIZE: f32 = 8.0;

@vertex
fn vs_main(vertex: VertexInput, instance: InstanceInput) -> VertexOutput {
    // Per-axis scale (component-wise), then yaw around the world's vertical
    // (Y) axis, then place at the entity's position. Scale first so rotation
    // spins the already-sized shape rather than an elongated axis.
    let scaled = vertex.position * MESH_SIZE * instance.scale;
    let cos_y = cos(instance.yaw);
    let sin_y = sin(instance.yaw);
    let rotated = vec3<f32>(
        scaled.x * cos_y - scaled.z * sin_y,
        scaled.y,
        scaled.x * sin_y + scaled.z * cos_y,
    );
    let world_pos = instance.position + rotated;

    // The mesh's own normal, rotated the same way the shape was, so shading
    // stays correct as an entity turns. A diagonal scale leaves an
    // axis-aligned cube normal untouched, and a uniformly-scaled sphere keeps
    // correct normals too; a *non-uniformly* scaled sphere shades
    // approximately, which is fine here. Actual lighting now happens per
    // fragment, not here, so a point light's falloff varies smoothly across
    // a face instead of only being evaluated at each corner.
    let rotated_normal = vec3<f32>(
        vertex.normal.x * cos_y - vertex.normal.z * sin_y,
        vertex.normal.y,
        vertex.normal.x * sin_y + vertex.normal.z * cos_y,
    );

    var out: VertexOutput;
    out.clip_position = camera.view_proj * vec4<f32>(world_pos, 1.0);
    out.color = instance.color;
    out.selected = instance.selected;
    out.emissive = instance.emissive;
    out.world_position = world_pos;
    out.world_normal = rotated_normal;
    return out;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
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
        let diffuse = max(dot(normal, light_dir), 0.0);
        accumulated = accumulated + diffuse * attenuation * intensity;
    }
    let shade = min(accumulated, 1.0);

    // Each entity draws in its own colour. Emissive blends between normal
    // shading and full unlit brightness, so a high emissive value makes an
    // entity glow, ignoring every light. The selected entity is then
    // brightened toward white so it stands out.
    let shaded = in.color * shade;

    let lit = mix(shaded, in.color, in.emissive);
    let highlighted = mix(lit, vec3<f32>(1.0, 1.0, 1.0), 0.3 * in.selected);
    return vec4<f32>(highlighted, 1.0);
}
