// Depth-only pass that draws every entity from the shadow-casting light's
// point of view into the shadow map. The main shader (shader.wgsl) samples
// the result to decide which fragments are in shadow.
//
// The vertex transform here MUST match vs_main in shader.wgsl (same scale,
// yaw and placement), or shadows would drift away from their casters.
struct ShadowCam {
    light_view_proj: mat4x4<f32>,
};
@group(0) @binding(0) var<uniform> shadow_cam: ShadowCam;

// World size of a primitive at scale 1. Must match MESH_SIZE in shader.wgsl.
const MESH_SIZE: f32 = 8.0;

struct VertexInput {
    @location(0) position: vec3<f32>,
};

// Only the instance fields that affect geometry are declared; the buffer
// layout still carries the rest, which is allowed.
struct InstanceInput {
    @location(3) position: vec3<f32>,
    @location(6) scale: vec3<f32>,
    @location(8) yaw: f32,
};

@vertex
fn vs_main(vertex: VertexInput, instance: InstanceInput) -> @builtin(position) vec4<f32> {
    let scaled = vertex.position * MESH_SIZE * instance.scale;
    let cos_y = cos(instance.yaw);
    let sin_y = sin(instance.yaw);
    let rotated = vec3<f32>(
        scaled.x * cos_y - scaled.z * sin_y,
        scaled.y,
        scaled.x * sin_y + scaled.z * cos_y,
    );
    return shadow_cam.light_view_proj * vec4<f32>(instance.position + rotated, 1.0);
}
