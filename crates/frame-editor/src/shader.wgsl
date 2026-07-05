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
};

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) color: vec3<f32>,
    @location(1) selected: f32,
    @location(2) shade: f32,
};

// World size of a primitive at scale 1. Primitives are generated at ~unit size
// in main.rs and scaled up by this, so a default entity is exactly the size the
// cube always was. NOTE: must match MESH_SIZE in main.rs (render and pick agree).
const MESH_SIZE: f32 = 8.0;

@vertex
fn vs_main(vertex: VertexInput, instance: InstanceInput) -> VertexOutput {
    // Per-axis scale (component-wise), then place at the entity's position.
    let world_pos = instance.position + vertex.position * MESH_SIZE * instance.scale;

    // Fixed-direction shading so shapes read as 3D as you orbit. We use the
    // mesh's own normals directly. A diagonal scale leaves an axis-aligned cube
    // normal untouched, and a uniformly-scaled sphere keeps correct normals too;
    // a *non-uniformly* scaled sphere shades approximately, which is fine here.
    let light_dir = normalize(vec3<f32>(0.4, 0.8, 0.6));
    let diffuse = max(dot(normalize(vertex.normal), light_dir), 0.0);
    let shade = 0.4 + 0.6 * diffuse; // ambient floor + diffuse

    var out: VertexOutput;
    out.clip_position = camera.view_proj * vec4<f32>(world_pos, 1.0);
    out.color = instance.color;
    out.selected = instance.selected;
    out.shade = shade;
    return out;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    // Each entity draws in its own colour. The selected entity is brightened
    // toward white so it stands out without hiding the colour you're editing.
    let lit = in.color * in.shade;
    let highlighted = mix(lit, vec3<f32>(1.0, 1.0, 1.0), 0.3 * in.selected);
    return vec4<f32>(highlighted, 1.0);
}
