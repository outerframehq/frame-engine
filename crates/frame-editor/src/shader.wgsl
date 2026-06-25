// Frame Engine wgpu renderer shader.
//
// Stages:
//   - vs_main  (vertex)   : runs once per corner, returns where that corner sits
//   - fs_main  (fragment) : runs once per covered pixel, returns its colour
//
// New in step 4a: a camera matrix arrives from Rust via a uniform buffer.
// The vertex shader multiplies each corner by it. For now that matrix is the
// identity (a do-nothing transform), so the quad looks unchanged — that's how
// we prove the matrix actually reached the shader. Step 4b swaps in a real
// perspective + view matrix.

// The uniform: must match the CameraUniform struct on the Rust side.
struct Camera {
    view_proj: mat4x4<f32>,
};

// @group(0) @binding(0) lines up with the bind group layout in main.rs.
@group(0) @binding(0)
var<uniform> camera: Camera;

@vertex
fn vs_main(@builtin(vertex_index) index: u32) -> @builtin(position) vec4<f32> {
    // A quad is two triangles. Six corners, wound as two tris.
    // Treat these as world-space positions on the z = 0 plane.
    var corners = array<vec2<f32>, 6>(
        vec2<f32>(-0.5, -0.5), // triangle 1
        vec2<f32>( 0.5, -0.5),
        vec2<f32>( 0.5,  0.5),
        vec2<f32>(-0.5, -0.5), // triangle 2
        vec2<f32>( 0.5,  0.5),
        vec2<f32>(-0.5,  0.5),
    );

    let p = corners[index];
    // Transform the corner by the camera matrix. With identity, this is a no-op.
    return camera.view_proj * vec4<f32>(p, 0.0, 1.0);
}

@fragment
fn fs_main() -> @location(0) vec4<f32> {
    // the familiar entity yellow (r, g, b, a), each 0..1
    return vec4<f32>(0.85, 0.85, 0.3, 1.0);
}
