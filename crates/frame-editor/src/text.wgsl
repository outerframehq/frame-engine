// Screen-space overlay shader for debug text. Each instance is a small white
// rectangle already positioned in clip space (NDC) — so there is NO camera
// here; the overlay stays anchored to the screen regardless of pan/zoom.
struct OverlayInstance {
    @location(0) offset: vec2<f32>, // bottom-left corner, in NDC (-1..1)
    @location(1) size: vec2<f32>,   // width/height, in NDC units
};

@vertex
fn vs_main(
    @builtin(vertex_index) index: u32,
    inst: OverlayInstance,
) -> @builtin(position) vec4<f32> {
    // unit quad, corners 0..1
    var corners = array<vec2<f32>, 6>(
        vec2<f32>(0.0, 0.0),
        vec2<f32>(1.0, 0.0),
        vec2<f32>(1.0, 1.0),
        vec2<f32>(0.0, 0.0),
        vec2<f32>(1.0, 1.0),
        vec2<f32>(0.0, 1.0),
    );
    let p = inst.offset + corners[index] * inst.size;
    return vec4<f32>(p, 0.0, 1.0);
}

@fragment
fn fs_main() -> @location(0) vec4<f32> {
    return vec4<f32>(1.0, 1.0, 1.0, 1.0); // white
}
