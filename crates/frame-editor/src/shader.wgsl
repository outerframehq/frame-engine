struct Camera {
    view_proj: mat4x4<f32>,
};

@group(0) @binding(0)
var<uniform> camera: Camera;

struct InstanceInput {
    @location(0) position: vec3<f32>,
    @location(1) selected: f32,
};

// Passed from the vertex stage to the fragment stage.
struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) selected: f32,
};

// Size of each entity quad in world units.
// NOTE: must match QUAD_SIZE in main.rs (used by click-picking).
const QUAD_SIZE: f32 = 8.0;

@vertex
fn vs_main(
    @builtin(vertex_index) index: u32,
    instance: InstanceInput,
) -> VertexOutput {
    var corners = array<vec2<f32>, 6>(
        vec2<f32>(-0.5, -0.5),
        vec2<f32>( 0.5, -0.5),
        vec2<f32>( 0.5,  0.5),
        vec2<f32>(-0.5, -0.5),
        vec2<f32>( 0.5,  0.5),
        vec2<f32>(-0.5,  0.5),
    );

    let corner = corners[index] * QUAD_SIZE;
    let world = vec3<f32>(corner, 0.0) + instance.position;

    var out: VertexOutput;
    out.clip_position = camera.view_proj * vec4<f32>(world, 1.0);
    out.selected = instance.selected;
    return out;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let base = vec3<f32>(0.85, 0.85, 0.3);      // normal: yellow
    let highlight = vec3<f32>(1.0, 0.45, 0.1);  // selected: orange
    let color = mix(base, highlight, in.selected);
    return vec4<f32>(color, 1.0);
}
