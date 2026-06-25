// First shader for Frame Engine's wgpu renderer.
//
// Two stages run on the GPU:
//   - vs_main  (vertex)   : runs once per corner, returns where that corner sits
//   - fs_main  (fragment) : runs once per covered pixel, returns its colour
//
// For now the quad's six corners are hardcoded here, in clip space:
// clip space runs -1..1 across the window, with (0,0) at the centre. So a
// square from -0.5 to 0.5 sits centred, half the window wide. (It'll look
// stretched on a non-square window until the perspective camera lands in
// step 4 — that's expected.)

@vertex
fn vs_main(@builtin(vertex_index) index: u32) -> @builtin(position) vec4<f32> {
    // A quad is two triangles. Six corners, wound as two tris:
    var corners = array<vec2<f32>, 6>(
        vec2<f32>(-0.5, -0.5), // triangle 1
        vec2<f32>( 0.5, -0.5),
        vec2<f32>( 0.5,  0.5),
        vec2<f32>(-0.5, -0.5), // triangle 2
        vec2<f32>( 0.5,  0.5),
        vec2<f32>(-0.5,  0.5),
    );

    let p = corners[index];
    // x, y from the array; z = 0 (on the near plane); w = 1 (no perspective yet)
    return vec4<f32>(p, 0.0, 1.0);
}

@fragment
fn fs_main() -> @location(0) vec4<f32> {
    // the familiar entity yellow (r, g, b, a), each 0..1
    return vec4<f32>(0.85, 0.85, 0.3, 1.0);
}
