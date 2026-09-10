// Camera (shared view-projection matrix), set once per frame.
struct Camera {
    view_proj: mat4x4<f32>,
};
@group(0) @binding(0) var<uniform> camera: Camera;

// Per-vertex mesh data (matches MeshVertex in main.rs). Position is in the
// primitive's local space — roughly unit-sized, centred on the origin — and is
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
    @location(2) shade: f32,
    @location(3) emissive: f32,
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

    // Fixed-direction shading so shapes read as 3D as you orbit. We use the
    // mesh's own normals, rotated the same way the shape was, so shading
    // stays correct as an entity turns. A diagonal scale leaves an
    // axis-aligned cube normal untouched, and a uniformly-scaled sphere keeps
    // correct normals too; a *non-uniformly* scaled sphere shades
    // approximately, which is fine here.
    let rotated_normal = vec3<f32>(
        vertex.normal.x * cos_y - vertex.normal.z * sin_y,
        vertex.normal.y,
        vertex.normal.x * sin_y + vertex.normal.z * cos_y,
    );
    let light_dir = normalize(vec3<f32>(0.4, 0.8, 0.6));
    let diffuse = max(dot(normalize(rotated_normal), light_dir), 0.0);
    let shade = 0.4 + 0.6 * diffuse; // ambient floor + diffuse

    var out: VertexOutput;
    out.clip_position = camera.view_proj * vec4<f32>(world_pos, 1.0);
    out.color = instance.color;
    out.selected = instance.selected;
    out.shade = shade;
    out.emissive = instance.emissive;
    return out;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    // Each entity draws in its own colour. Emissive blends between normal
    // shading and full unlit brightness, so a high emissive value makes an
    // entity glow, ignoring the directional light. The selected entity is
    // then brightened toward white so it stands out.
    let shaded = in.color * in.shade;
    let lit = mix(shaded, in.color, in.emissive);
    let highlighted = mix(lit, vec3<f32>(1.0, 1.0, 1.0), 0.3 * in.selected);
    return vec4<f32>(highlighted, 1.0);
}
