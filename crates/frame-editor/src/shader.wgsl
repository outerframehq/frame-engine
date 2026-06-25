struct Camera {
    view_proj: mat4x4<f32>,
};

@group(0) @binding(0)
var<uniform> camera: Camera;

// Per-instance data: one entity's world position. @location(0) lines up with
// the instance buffer's vertex attribute in main.rs.
struct InstanceInput {
    @location(0) position: vec3<f32>,
};

// Size of each entity quad in world units (a knob).
const QUAD_SIZE: f32 = 8.0;

@vertex
fn vs_main(
    @builtin(vertex_index) index: u32,
    instance: InstanceInput,
) -> @builtin(position) vec4<f32> {
    var corners = array<vec2<f32>, 6>(
        vec2<f32>(-0.5, -0.5),
        vec2<f32>( 0.5, -0.5),
        vec2<f32>( 0.5,  0.5),
        vec2<f32>(-0.5, -0.5),
        vec2<f32>( 0.5,  0.5),
        vec2<f32>(-0.5,  0.5),
    );

    // unit corner -> sized quad, placed at the entity's world position
    let corner = corners[index] * QUAD_SIZE;
    let world = vec3<f32>(corner, 0.0) + instance.position;

    return camera.view_proj * vec4<f32>(world, 1.0);
}

@fragment
fn fs_main() -> @location(0) vec4<f32> {
    return vec4<f32>(0.85, 0.85, 0.3, 1.0);
}
