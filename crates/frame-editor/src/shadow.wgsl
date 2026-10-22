// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

// Depth-only pass that draws every entity from the shadow-casting light's
// point of view into the shadow map. The main shader (shader.wgsl) samples
// the result to decide which fragments are in shadow.
//
// The vertex transform here MUST match vs_main in shader.wgsl (same scale,
// yaw and placement), or shadows would drift away from their casters.
// The same buffer as `Shadow` in shader.wgsl.
struct ShadowCam {
    light_view_proj: mat4x4<f32>,
    params: vec4<f32>,
    // The viewing camera, so vertices morph exactly as they are drawn.
    eye: vec4<f32>,
};
@group(0) @binding(0) var<uniform> shadow_cam: ShadowCam;

// World size of a primitive at scale 1. Must match MESH_SIZE in shader.wgsl.
const MESH_SIZE: f32 = 1.0;

struct VertexInput {
    @location(0) position: vec3<f32>,
    @location(9) morph: vec3<f32>,
    @location(13) shape: vec2<u32>,
};

// Only the instance fields that affect geometry are declared; the buffer
// layout still carries the rest, which is allowed.
struct InstanceInput {
    @location(3) position: vec3<f32>,
    @location(6) scale: vec3<f32>,
    @location(8) rotation: vec3<f32>,
    @location(14) shape: u32,
};

// Shape keys, as `shaped` in shader.wgsl (positions only: a shadow needs no
// normals); keep them in step, or shadows would drift from their casters.
struct ShapeMove {
    position: vec3<f32>,
    key: u32,
    normal: vec3<f32>,
    pad: u32,
};
@group(1) @binding(0) var<storage, read> shape_moves: array<ShapeMove>;
@group(1) @binding(1) var<storage, read> shape_weights: array<f32>;

fn shaped(vertex: VertexInput, instance: InstanceInput) -> vec3<f32> {
    var position = vertex.position;
    if (vertex.shape.y == 0u || instance.shape == 0u) {
        return position;
    }
    let weights = instance.shape - 1u;
    for (var i = 0u; i < vertex.shape.y; i = i + 1u) {
        let m = shape_moves[vertex.shape.x + i];
        position = position + m.position * shape_weights[weights + m.key];
    }
    return position;
}

// Same rotation as `rotate_by` in shader.wgsl (yaw, pitch, roll); keep them in
// step, or shadows would drift away from their casters.
fn rotate_by(v: vec3<f32>, angles: vec3<f32>) -> vec3<f32> {
    let cr = cos(angles.z);
    let sr = sin(angles.z);
    let rolled = vec3<f32>(v.x * cr + v.y * sr, -v.x * sr + v.y * cr, v.z);
    let cp = cos(angles.y);
    let sp = sin(angles.y);
    let pitched = vec3<f32>(rolled.x, rolled.y * cp - rolled.z * sp, rolled.y * sp + rolled.z * cp);
    let cy = cos(angles.x);
    let sy = sin(angles.x);
    return vec3<f32>(pitched.x * cy - pitched.z * sy, pitched.y, pitched.x * sy + pitched.z * cy);
}

// Same as `morph_amount` in shader.wgsl; keep them in step.
fn morph_amount(world: vec3<f32>, eye: vec3<f32>, band: vec2<f32>) -> f32 {
    if (band.y <= band.x) {
        return 0.0;
    }
    let d = max(abs(world.x - eye.x), abs(world.z - eye.z));
    return clamp((d - band.x) / (band.y - band.x), 0.0, 1.0);
}

@vertex
fn vs_main(vertex: VertexInput, instance: InstanceInput) -> @builtin(position) vec4<f32> {
    let position = shaped(vertex, instance);
    let unmorphed = instance.position
        + rotate_by(position * MESH_SIZE * instance.scale, instance.rotation);
    let t = morph_amount(unmorphed, shadow_cam.eye.xyz, vertex.morph.yz);
    let local = position + vec3<f32>(0.0, vertex.morph.x * t, 0.0);
    let scaled = local * MESH_SIZE * instance.scale;
    let rotated = rotate_by(scaled, instance.rotation);
    return shadow_cam.light_view_proj * vec4<f32>(instance.position + rotated, 1.0);
}
