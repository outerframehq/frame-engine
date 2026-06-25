// Camera (shared view-projection matrix), set once per frame.
struct Camera {
    view_proj: mat4x4<f32>,
};
@group(0) @binding(0) var<uniform> camera: Camera;

// Per-entity instance data (matches InstanceRaw in main.rs).
struct InstanceInput {
    @location(0) position: vec3<f32>,
    @location(1) selected: f32,
};

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) selected: f32,
    @location(1) shade: f32,
};

// Side length of each entity cube, in world units.
// NOTE: must match QUAD_SIZE in main.rs (render and pick must agree).
const CUBE_SIZE: f32 = 8.0;

// 8 corners of a unit cube centred on the origin (half-extent 0.5).
var<private> CORNERS: array<vec3<f32>, 8> = array<vec3<f32>, 8>(
    vec3<f32>(-0.5, -0.5, -0.5),
    vec3<f32>( 0.5, -0.5, -0.5),
    vec3<f32>( 0.5,  0.5, -0.5),
    vec3<f32>(-0.5,  0.5, -0.5),
    vec3<f32>(-0.5, -0.5,  0.5),
    vec3<f32>( 0.5, -0.5,  0.5),
    vec3<f32>( 0.5,  0.5,  0.5),
    vec3<f32>(-0.5,  0.5,  0.5),
);

// 36 indices: 6 faces x 2 triangles x 3 verts, grouped by face.
var<private> INDICES: array<u32, 36> = array<u32, 36>(
    4u, 5u, 6u, 6u, 7u, 4u, // +Z front
    1u, 0u, 3u, 3u, 2u, 1u, // -Z back
    5u, 1u, 2u, 2u, 6u, 5u, // +X right
    0u, 4u, 7u, 7u, 3u, 0u, // -X left
    3u, 2u, 6u, 6u, 7u, 3u, // +Y top
    0u, 1u, 5u, 5u, 4u, 0u, // -Y bottom
);

// One outward normal per face (face = vertex_index / 6).
var<private> NORMALS: array<vec3<f32>, 6> = array<vec3<f32>, 6>(
    vec3<f32>( 0.0,  0.0,  1.0),
    vec3<f32>( 0.0,  0.0, -1.0),
    vec3<f32>( 1.0,  0.0,  0.0),
    vec3<f32>(-1.0,  0.0,  0.0),
    vec3<f32>( 0.0,  1.0,  0.0),
    vec3<f32>( 0.0, -1.0,  0.0),
);

@vertex
fn vs_main(@builtin(vertex_index) vi: u32, instance: InstanceInput) -> VertexOutput {
    let corner = CORNERS[INDICES[vi]];
    let world_pos = instance.position + corner * CUBE_SIZE;

    // Fixed-direction shading so the cube reads as 3D as you orbit.
    let normal = NORMALS[vi / 6u];
    let light_dir = normalize(vec3<f32>(0.4, 0.8, 0.6));
    let diffuse = max(dot(normal, light_dir), 0.0);
    let shade = 0.4 + 0.6 * diffuse; // ambient floor + diffuse

    var out: VertexOutput;
    out.clip_position = camera.view_proj * vec4<f32>(world_pos, 1.0);
    out.selected = instance.selected;
    out.shade = shade;
    return out;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let base = mix(
        vec3<f32>(0.95, 0.85, 0.35), // yellow
        vec3<f32>(0.95, 0.55, 0.15), // orange (selected)
        in.selected,
    );
    return vec4<f32>(base * in.shade, 1.0);
}
