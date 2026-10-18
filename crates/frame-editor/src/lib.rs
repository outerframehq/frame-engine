// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

const LOGO_PNG: &[u8] = include_bytes!("../assets/frame-editor.png");
use bytemuck::Zeroable;
use frame_engine::core::Clock;
use frame_engine::input::{Button, InputState};
use frame_engine::pack::ProjectFiles;
use frame_engine::physics::{GRAVITY_Y, Physics};
use frame_engine::project;
use frame_engine::systems;
use frame_engine::world::{
    Camera, Controlled, Gravity, Light, LightKind, Mesh, Position, RigidBody, Script,
    ScriptRuntime, Sound, Static, Velocity, World,
};
use glam::{DVec3, Mat4, Vec3, Vec4};
use std::sync::Arc;
use wgpu::util::DeviceExt;
use winit::application::ApplicationHandler;
use winit::event::{
    DeviceEvent, DeviceId, ElementState, MouseButton, MouseScrollDelta, WindowEvent,
};
use winit::event_loop::{ActiveEventLoop, EventLoop};
use winit::keyboard::{KeyCode, PhysicalKey};
use winit::window::{Icon, Window, WindowId};
/// The editor's shader: the scene part every world pass shares, then the
/// editor's own entry points.
pub(crate) const ENTITY_WGSL: &str =
    concat!(include_str!("scene.wgsl"), include_str!("shader.wgsl"));
mod build;
mod font;
mod game_audio;
mod game_menu;
mod grass;
mod licences;
mod origin;
mod perf;
mod scenes;
pub use perf::CountingAlloc;
mod script;
mod sky;
mod sun;
use frame_engine::core::TICK_RATE;
const MAX_CATCHUP_TICKS: u32 = 5;
// Vertical field of view, shared by the projection and the pan maths.
const FOV_DEGREES: f32 = 45.0;
/// The camera's near plane, in world units. There is no far plane: depth is
/// "reversed", with the near plane at depth 1 and infinity at depth 0, which
/// spreads the 32-bit float depth buffer's precision evenly over distance. So
/// the view reaches as far as anything is drawn, with no z-fighting far out.
const NEAR: f32 = 0.1;
/// What the depth buffers clear to: as far as reversed depth goes.
const DEPTH_CLEAR: f32 = 0.0;
/// The camera's projection for a viewport `aspect` (width over height), with
/// reversed depth (see `NEAR`). Pipelines draw a fragment when its depth is
/// greater than what is there.
fn camera_projection(aspect: f32) -> Mat4 {
    Mat4::perspective_infinite_reverse_rh(FOV_DEGREES.to_radians(), aspect, NEAR)
}
// World-space size of an entity, used for picking. Defined once in the engine
// (it's a simulation fact — collision boxes use it too); we reference it here so
// pick and collision can't drift. MESH_SIZE in shader.wgsl must match it by hand.
const QUAD_SIZE: f32 = frame_engine::world::ENTITY_SIZE;
pub mod module_api;
// Re-exported so a crate that adds a module uses exactly the egui and wgpu
// this editor was built with.
pub use egui;
// For game programs built on this library: the cipher a game can seal its
// pack with, and the writer that makes a pack from a project.
pub use frame_engine::pack::{PackCipher, write_pack};
// For a game program's editor: say which program File > Build Game compiles.
pub use build::{GameBuilder, set_game_builder};
// For a game program: its own menu before play (see `run_player_with_menu`).
pub use game_menu::{GameMenu, GameMenuAction, GameSession, GameSettings, MenuView};
use module_api::{EditorModule, ExtEdit, ModuleGpu, ModuleScene};
use std::cell::RefCell;
use std::rc::Rc;
pub use wgpu;
// How fast middle-drag sweeps the orbit, in radians per pixel.
const ORBIT_SENS: f32 = 0.005;
// Mouselook sensitivity for the flythrough camera (radians per pixel of motion).
const LOOK_SENS: f32 = 0.005;

/// Hide and hold the mouse in `window` (for looking around), or give it back.
fn grab_cursor(window: &Window, grab: bool) {
    if grab {
        let _ = window
            .set_cursor_grab(winit::window::CursorGrabMode::Locked)
            .or_else(|_| window.set_cursor_grab(winit::window::CursorGrabMode::Confined));
    } else {
        let _ = window.set_cursor_grab(winit::window::CursorGrabMode::None);
    }
    window.set_cursor_visible(!grab);
}
// Format of the depth buffer. 32-bit float depth, no stencil.
const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;
// Size of the Camera preview shown in the Inspector when a Camera entity is
// selected. Fixed rather than following the panel width: the Inspector's own
// width can change (docking, popping out), and re-creating the preview
// texture and its egui registration on every resize is more churn than a
// small preview image needs.
const PREVIEW_WIDTH: u32 = 320;
const PREVIEW_HEIGHT: u32 = 180;
// How far a single nudge moves the selected entity, in world units.
const EDIT_STEP: f32 = 5.0;
// How far the WASD free camera pans the focus point per frame (while paused).
const CAM_PAN_SPEED: f32 = 3.0;
// Translate gizmo: how long each axis arm is, as a fraction of the distance from
// the camera to the entity — so it keeps a roughly constant size on screen.
const GIZMO_SCREEN_FRAC: f32 = 0.15;
// How close (in pixels) the cursor must be to an arm to grab it.
const GIZMO_PICK_PX: f32 = 10.0;
// The camera data handed to the shader. Must match the `Camera` struct in shader.wgsl.
#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct CameraUniform {
    view_proj: [[f32; 4]; 4],
    // xyz: the camera's position as drawn (relative to the render origin),
    // for vertex morphing. w: the tide, how far the sea stands over its still
    // level (see `EditorModule::tide`).
    eye: [f32; 4],
}
impl CameraUniform {
    fn new(view_proj: [[f32; 4]; 4]) -> Self {
        let eye = sky::eye_from_view_proj(&Mat4::from_cols_array_2d(&view_proj));
        CameraUniform {
            view_proj,
            eye: [eye.x, eye.y, eye.z, tide()],
        }
    }
}
// One light's data handed to the shader. Must match the `Light` struct in
// shader.wgsl field-for-field, including the deliberate use of [f32; 4]
// (rather than [f32; 3]) everywhere, purely to keep every light a clean
// 16-byte-aligned multiple in the uniform buffer, sidestepping WGSL's
// alignment rules for vec3.
#[repr(C)]
#[derive(Copy, Clone, Debug, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
struct LightRaw {
    // xyz: direction toward the light (directional) or world position
    // (point). w unused.
    position_or_direction: [f32; 4],
    // x: kind, 0.0 directional, 1.0 point. y: range (point only). z:
    // intensity, 0.0 marks an unused slot. w: 1.0 if this light casts the
    // scene's shadow (the first directional light), else 0.0.
    params: [f32; 4],
}
// A fixed number of simultaneously active lights sent to the shader every
// frame, unused slots zeroed (intensity 0.0), matching MAX_LIGHTS in
// shader.wgsl.
const MAX_LIGHTS: usize = 4;
#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct LightsUniform {
    lights: [LightRaw; MAX_LIGHTS],
}
// Shadow map resolution (square). 2048 gives soft, readable shadows for a
// typical scene at 16 MB of GPU memory.
const SHADOW_MAP_SIZE: u32 = 2048;
// Shadow data handed to the shader (group 0, binding 2). Must match `Shadow`
// in shader.wgsl field-for-field. The first 64 bytes double as the shadow
// pass's own camera (shadow.wgsl reads only the matrix).
#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct ShadowUniform {
    light_view_proj: [[f32; 4]; 4],
    // x: 1.0 when shadows are active this frame. y: depth bias. z: normal
    // offset in world units. w: one texel in UV space.
    params: [f32; 4],
    // xyz: the viewing camera's position as drawn, so the shadow pass morphs
    // vertices the same way the colour pass does. w unused.
    eye: [f32; 4],
}
/// Process-wide switch for shadows (an editor preference). A static rather
/// than an App field because the Play window has its own GpuState and both
/// read it.
static SHADOWS_ENABLED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);
/// How far from the camera shadows are drawn, in world units, as f32 bits.
/// Smaller is sharper; larger reaches further. An editor preference.
const SHADOW_DISTANCE_DEFAULT: f32 = 250.0;
static SHADOW_DISTANCE_BITS: std::sync::atomic::AtomicU32 =
    std::sync::atomic::AtomicU32::new(SHADOW_DISTANCE_DEFAULT.to_bits());
fn shadow_distance() -> f32 {
    f32::from_bits(SHADOW_DISTANCE_BITS.load(std::sync::atomic::Ordering::Relaxed))
}
fn set_shadow_distance(distance: f32) {
    SHADOW_DISTANCE_BITS.store(distance.to_bits(), std::sync::atomic::Ordering::Relaxed);
}
fn shadows_enabled() -> bool {
    SHADOWS_ENABLED.load(std::sync::atomic::Ordering::Relaxed)
}
/// Process-wide switch for vsync (an editor preference): on, a window never
/// draws faster than the screen shows. Off, it draws as fast as it can, which
/// is only useful to measure how fast that is.
static VSYNC: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);
fn vsync_enabled() -> bool {
    VSYNC.load(std::sync::atomic::Ordering::Relaxed)
}
fn set_vsync(on: bool) {
    VSYNC.store(on, std::sync::atomic::Ordering::Relaxed);
}
/// The present mode for the vsync setting. With vsync, plain `Fifo`: every
/// system supports it, and it is the steadiest (the Auto choice would take
/// `FifoRelaxed` where offered, which tears when a frame is late). Without,
/// `AutoNoVsync`, which falls back to a mode the system has.
fn present_mode_for(vsync: bool) -> wgpu::PresentMode {
    if vsync {
        wgpu::PresentMode::Fifo
    } else {
        wgpu::PresentMode::AutoNoVsync
    }
}
/// The mode a window really gets for `wanted`, given the modes the system
/// offers: the Auto modes resolve the way wgpu resolves them.
fn resolved_present_mode(
    wanted: wgpu::PresentMode,
    offered: &[wgpu::PresentMode],
) -> wgpu::PresentMode {
    use wgpu::PresentMode as P;
    let first_offered = |modes: &[P]| modes.iter().copied().find(|m| offered.contains(m));
    match wanted {
        P::AutoVsync => first_offered(&[P::FifoRelaxed, P::Fifo]).unwrap_or(P::Fifo),
        P::AutoNoVsync => first_offered(&[P::Immediate, P::Mailbox, P::Fifo]).unwrap_or(P::Fifo),
        other => other,
    }
}
/// How often an idle editor still draws a frame, so slow-changing things
/// (Source Control status, the overlay) stay fresh without drawing nonstop.
const IDLE_REFRESH: std::time::Duration = std::time::Duration::from_secs(1);
fn set_shadows_enabled(on: bool) {
    SHADOWS_ENABLED.store(on, std::sync::atomic::Ordering::Relaxed);
}
/// Process-wide switch for the sky and fog, and how thick the fog is, as f32
/// bits. Editor preferences, kept as statics for the same reason as the
/// shadow ones: the Play window has its own GpuState and both read them.
static SKY_ENABLED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);
static FOG_AMOUNT_BITS: std::sync::atomic::AtomicU32 =
    std::sync::atomic::AtomicU32::new(sky::FOG_AMOUNT_DEFAULT.to_bits());
/// How far the drawn world reaches from the viewer this frame, as f32 bits
/// (0 for no module that draws to a distance). Set with the module sun before
/// each window draws; the fog thins to suit it (see `sky::fog_density`).
static VIEW_REACH_BITS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
fn view_reach() -> f32 {
    f32::from_bits(VIEW_REACH_BITS.load(std::sync::atomic::Ordering::Relaxed))
}
/// The tide a module keeps (see `EditorModule::tide`), as f32 bits: how far
/// the sea stands over its still level this frame, 0 when no module keeps
/// one.
static TIDE_BITS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
fn set_tide(tide: Option<f32>) {
    let tide = tide.filter(|t| t.is_finite()).unwrap_or(0.0);
    TIDE_BITS.store(tide.to_bits(), std::sync::atomic::Ordering::Relaxed);
}
fn tide() -> f32 {
    f32::from_bits(TIDE_BITS.load(std::sync::atomic::Ordering::Relaxed))
}
/// The view under a liquid this frame (see `EditorModule::underwater`), with
/// the eye's height, or None above it. Set before each window draws, like
/// the tide.
static UNDERWATER: std::sync::Mutex<Option<(module_api::Underwater, f64)>> =
    std::sync::Mutex::new(None);
fn set_underwater(view: Option<module_api::Underwater>, eye_y: f64) {
    if let Ok(mut slot) = UNDERWATER.lock() {
        *slot = view.map(|v| (v, eye_y));
    }
}
fn underwater() -> Option<(module_api::Underwater, f64)> {
    UNDERWATER.lock().ok().and_then(|slot| *slot)
}
/// How far fog under a liquid has to reach to all but hide a thing
/// `Underwater::visibility` away: the fog lets through e to the minus this.
const UNDERWATER_FOG: f32 = 3.0;
/// How much of the sound gets through under a liquid: a low-pass cutoff in
/// hertz, and how quickly it closes or opens when the head dips or comes up.
const UNDERWATER_CUTOFF: f64 = 500.0;
const UNDERWATER_EASE: std::time::Duration = std::time::Duration::from_millis(150);

/// The sky's uniform turned to the inside of a liquid: every colour the
/// liquid's, dimmed with the light, fog thick enough to hide things at the
/// liquid's visibility, and no sun, clouds, moon or stars.
fn under_liquid(
    mut uniform: sky::SkyUniform,
    view: module_api::Underwater,
    eye_y: f64,
) -> sky::SkyUniform {
    let light = uniform.ground[3].clamp(0.06, 1.0);
    let c = view.color.map(|v| v.clamp(0.0, 1.0) * light);
    for field in [
        &mut uniform.zenith,
        &mut uniform.horizon,
        &mut uniform.ground,
        &mut uniform.sun_colour,
    ] {
        field[0] = c[0];
        field[1] = c[1];
        field[2] = c[2];
    }
    // The sky is drawn (it is the water all around); the sun's disc,
    // twilight thinning, near haze, clouds, moon and stars are not.
    uniform.zenith[3] = 1.0;
    uniform.sun_direction[3] = 0.0;
    uniform.sun_colour[3] = 0.0;
    uniform.motion[3] = 0.0;
    uniform.clouds[0] = 0.0;
    uniform.moon[3] = 0.0;
    uniform.night[2] = 0.0;
    // The fog thins with height (`sky::FOG_HEIGHT_SCALE`): thickened back by
    // as much at the eye's height, so a mountain lake is as murky as the
    // sea.
    let height = (eye_y.max(0.0) as f32 / sky::FOG_HEIGHT_SCALE).min(20.0);
    uniform.horizon[3] = UNDERWATER_FOG / view.visibility.max(0.1) * height.exp();
    uniform
}

fn set_view_reach(reach: Option<f64>) {
    let reach = reach.filter(|r| r.is_finite() && *r > 0.0).unwrap_or(0.0) as f32;
    VIEW_REACH_BITS.store(reach.to_bits(), std::sync::atomic::Ordering::Relaxed);
}
/// Seconds of moving water, for the waves of liquid surfaces, as f32 bits.
/// Runs with wall time only while the world moves (the editor playing or the
/// Play window open), so water is still while everything is paused, and the
/// idle editor need not keep drawing. Wraps every `WAVE_WRAP` seconds.
static WAVE_SECONDS_BITS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
const WAVE_WRAP: f32 = 3600.0;
/// Waves repeat exactly every this many world units along x and z, so the
/// render origin can be handed to the shader wrapped to it (see
/// `GpuState::wave_origin`). MUST match WAVE_PERIOD in shader.wgsl.
const WAVE_PERIOD: f64 = 4096.0;
/// The clouds' pattern repeats exactly every this many world units along x
/// and z (a few hundred kilometres, so it never shows). MUST match
/// CLOUD_PERIOD in shader.wgsl.
const CLOUD_PERIOD: f64 = 262_144.0;
/// How much of the sky clouds cover, 0 to 1, as f32 bits. An editor
/// preference, read by every window like the fog.
static CLOUD_AMOUNT_BITS: std::sync::atomic::AtomicU32 =
    std::sync::atomic::AtomicU32::new(sky::CLOUDS_DEFAULT.to_bits());
fn cloud_amount() -> f32 {
    f32::from_bits(CLOUD_AMOUNT_BITS.load(std::sync::atomic::Ordering::Relaxed))
}
fn set_cloud_amount(amount: f32) {
    CLOUD_AMOUNT_BITS.store(amount.to_bits(), std::sync::atomic::Ordering::Relaxed);
}
/// How much grass to draw (see `grass::GrassQuality`), as its index. An
/// editor preference, read by every window.
static GRASS_QUALITY: std::sync::atomic::AtomicU32 =
    std::sync::atomic::AtomicU32::new(grass::GrassQuality::High as u32);
fn grass_quality() -> grass::GrassQuality {
    grass::GrassQuality::from_index(GRASS_QUALITY.load(std::sync::atomic::Ordering::Relaxed))
}
fn set_grass_quality(quality: grass::GrassQuality) {
    GRASS_QUALITY.store(quality.to_index(), std::sync::atomic::Ordering::Relaxed);
}
fn wave_seconds() -> f32 {
    f32::from_bits(WAVE_SECONDS_BITS.load(std::sync::atomic::Ordering::Relaxed))
}
fn advance_waves(dt: f32) {
    let t = (wave_seconds() + dt.clamp(0.0, 0.1)).rem_euclid(WAVE_WRAP);
    WAVE_SECONDS_BITS.store(t.to_bits(), std::sync::atomic::Ordering::Relaxed);
}
fn sky_enabled() -> bool {
    SKY_ENABLED.load(std::sync::atomic::Ordering::Relaxed)
}
fn set_sky_enabled(on: bool) {
    SKY_ENABLED.store(on, std::sync::atomic::Ordering::Relaxed);
}
fn fog_amount() -> f32 {
    f32::from_bits(FOG_AMOUNT_BITS.load(std::sync::atomic::Ordering::Relaxed))
}
fn set_fog_amount(amount: f32) {
    FOG_AMOUNT_BITS.store(amount.to_bits(), std::sync::atomic::Ordering::Relaxed);
}
/// The direction toward the sun for this frame's lights: the shadow-casting
/// directional light if there is one, otherwise the first directional light.
/// None when the scene has no directional light.
fn sun_direction(lights: &[LightRaw; MAX_LIGHTS]) -> Option<[f32; 3]> {
    let directional = |l: &&LightRaw| l.params[0] < 0.5 && l.params[2] > 0.0;
    lights
        .iter()
        .filter(directional)
        .find(|l| l.params[3] > 0.5)
        .or_else(|| lights.iter().find(directional))
        .map(|l| {
            [
                l.position_or_direction[0],
                l.position_or_direction[1],
                l.position_or_direction[2],
            ]
        })
}
/// Where the sky should put the sun this frame: the time-of-day (or a
/// module's) sun when one is active, even if the scene has no light of its own,
/// otherwise the scene's sun light.
fn effective_sun(lights: &[LightRaw; MAX_LIGHTS]) -> Option<[f32; 3]> {
    sun::current()
        .map(|s| s.direction)
        .or_else(|| sun_direction(lights))
}
/// The sky's uniform for this frame. The ambient light scale rides along in
/// the unused `ground.w`: 1.0 normally, dimmed at night while a time-of-day
/// sun is active.
fn sky_uniform_for_frame(
    view_proj: [[f32; 4]; 4],
    lights: &[LightRaw; MAX_LIGHTS],
    enabled: bool,
    origin_y: f32,
    wave_origin: [f32; 2],
    cloud_origin: [f32; 2],
) -> sky::SkyUniform {
    let mut uniform = sky::sky_uniform(
        view_proj,
        effective_sun(lights),
        enabled,
        fog_amount(),
        view_reach(),
        origin_y,
    );
    // The near haze in motion.w is the sky's own; the rest is the frame's.
    uniform.motion[0] = wave_seconds();
    uniform.motion[1] = wave_origin[0];
    uniform.motion[2] = wave_origin[1];
    let cover = if enabled { cloud_amount() } else { 0.0 };
    // The clouds sit as far downwind as the wind has carried them.
    let drift = sun::cloud_drift(wave_seconds());
    let cloud_at =
        |origin: f32, drift: f64| (f64::from(origin) + drift).rem_euclid(CLOUD_PERIOD) as f32;
    uniform.clouds = [
        cover,
        cloud_at(cloud_origin[0], drift[0]),
        cloud_at(cloud_origin[1], drift[1]),
        0.0,
    ];
    let moon = sun::current_moon();
    match sun::current() {
        Some(sun) => {
            uniform.ground[3] = sky::ambient_scale(sun.direction[1], moon.map_or(0.0, |m| m.glow));
            let (m, night, tint) = sky::night_fields(
                sun.direction[1],
                moon.map(|m| (m.direction, m.phase, m.glow)),
            );
            if enabled {
                uniform.moon = m;
                uniform.night = night;
            }
            uniform.tint = tint;
        }
        None => uniform.ground[3] = 1.0,
    }
    match underwater() {
        Some((view, eye_y)) => under_liquid(uniform, view, eye_y),
        None => uniform,
    }
}
/// Work out the shadow map's view for this frame: an orthographic box,
/// looking along the first shadow-casting directional light, sized to just
/// contain every instance. Returns an "off" uniform (the shader then treats
/// everything as lit) when shadows are disabled, there is nothing to draw,
/// or no light is marked as the shadow caster.
fn compute_shadow(
    instances: &[InstanceRaw],
    lights: &[LightRaw; MAX_LIGHTS],
    view_proj: [[f32; 4]; 4],
    // Extra world-space box that also casts shadows, beyond `instances`.
    extra_bounds: Option<(Vec3, Vec3)>,
) -> ShadowUniform {
    let off = ShadowUniform {
        light_view_proj: Mat4::IDENTITY.to_cols_array_2d(),
        params: [0.0; 4],
        eye: [0.0; 4],
    };
    if !shadows_enabled() || (instances.is_empty() && extra_bounds.is_none()) {
        return off;
    }
    let Some(light) = lights
        .iter()
        .find(|l| l.params[2] > 0.0 && l.params[0] < 0.5 && l.params[3] > 0.5)
    else {
        return off;
    };
    let to_light = Vec3::new(
        light.position_or_direction[0],
        light.position_or_direction[1],
        light.position_or_direction[2],
    )
    .normalize_or_zero();
    if to_light == Vec3::ZERO {
        return off;
    }
    // World-space bounds of everything that casts. A shape's farthest point
    // from its centre is at most ~0.9 of the mesh size per unit of scale.
    let mut min = Vec3::splat(f32::MAX);
    let mut max = Vec3::splat(f32::MIN);
    for i in instances {
        let reach = QUAD_SIZE * 0.9 * i.scale[0].max(i.scale[1]).max(i.scale[2]);
        let p = Vec3::from(i.position);
        min = min.min(p - Vec3::splat(reach));
        max = max.max(p + Vec3::splat(reach));
    }
    if let Some((lo, hi)) = extra_bounds {
        min = min.min(lo);
        max = max.max(hi);
    }
    if !(min.is_finite() && max.is_finite()) {
        return off;
    }
    let scene_center = (min + max) * 0.5;
    let scene_radius = ((max - min).length() * 0.5).max(8.0);
    // Pick an "up" that isn't parallel to the light direction.
    let up = if to_light.y.abs() > 0.99 {
        Vec3::Z
    } else {
        Vec3::Y
    };
    // Fit the map to the part of the world the camera can actually see, up
    // to the shadow distance, rather than the whole scene: a huge ground
    // plane would otherwise spread the map thin and blur every shadow. When
    // the whole scene is already smaller than that slice, the whole scene is
    // used, which is the sharpest and most stable fit.
    let mut center = scene_center;
    let mut radius = scene_radius;
    let inverse = Mat4::from_cols_array_2d(&view_proj).inverse();
    let unproject = |x: f32, y: f32, z: f32| {
        let p = inverse * Vec4::new(x, y, z, 1.0);
        p.truncate() / p.w
    };
    let mut slice_points = Vec::with_capacity(8);
    for (x, y) in [(-1.0, -1.0), (1.0, -1.0), (1.0, 1.0), (-1.0, 1.0)] {
        // Depth is reversed with no far plane (see `NEAR`): the near plane
        // is at depth 1, and depth `NEAR / d` is `d` units in front.
        let near = unproject(x, y, 1.0);
        let far = unproject(x, y, NEAR / shadow_distance());
        let ray = far - near;
        if !(near.is_finite() && far.is_finite()) || ray.length() < 1e-4 {
            slice_points.clear();
            break;
        }
        let end = near + ray.normalize() * ray.length().min(shadow_distance());
        slice_points.push(near);
        slice_points.push(end);
    }
    if slice_points.len() == 8 {
        let mut lo = Vec3::splat(f32::MAX);
        let mut hi = Vec3::splat(f32::MIN);
        for p in &slice_points {
            lo = lo.min(*p);
            hi = hi.max(*p);
        }
        let slice_center = (lo + hi) * 0.5;
        let slice_radius = slice_points
            .iter()
            .map(|p| p.distance(slice_center))
            .fold(0.0f32, f32::max)
            .ceil()
            .max(8.0);
        if slice_radius < scene_radius {
            center = slice_center;
            radius = slice_radius;
            // Snap the centre to whole shadow-map texels in the light's own
            // frame, so shadows don't shimmer as the camera moves.
            let texel = (2.0 * radius) / SHADOW_MAP_SIZE as f32;
            let basis = Mat4::look_at_rh(Vec3::ZERO, -to_light, up);
            let c = basis.transform_point3(center);
            let snapped = Vec3::new(
                (c.x / texel).round() * texel,
                (c.y / texel).round() * texel,
                c.z,
            );
            center = basis.inverse().transform_point3(snapped);
        }
    }
    // The light's eye sits beyond everything that can cast, so casters
    // outside the visible slice (a tall wall behind the camera, say) still
    // land in the map.
    let back = scene_radius + (center - scene_center).length() + radius;
    let eye = center + to_light * back;
    let view = Mat4::look_at_rh(eye, center, up);
    let proj = Mat4::orthographic_rh(-radius, radius, -radius, radius, 0.1, back + radius);
    let texel_world = (2.0 * radius) / SHADOW_MAP_SIZE as f32;
    // Depth bias is in the 0..1 depth range, which now spans back + radius
    // units, so scale it to stay about the same size in the world (~0.05).
    let depth_bias = 0.05 / (back + radius);
    ShadowUniform {
        light_view_proj: (proj * view).to_cols_array_2d(),
        params: [
            1.0,
            depth_bias,
            texel_world * 1.5,
            1.0 / SHADOW_MAP_SIZE as f32,
        ],
        eye: [0.0; 4],
    }
}
// One mesh's material data handed to the shader (group 1, binding 2). Must
// match MaterialParams in shader.wgsl field-for-field, each field a clean
// 16 bytes, the same reasoning LightRaw uses.
#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct MaterialUniform {
    // x: roughness, y: metalness, z: 1.0 if a real texture is bound (0.0 for
    // the shared default white texture), w: how choppy a liquid surface's
    // waves are (0 for anything that is not a liquid).
    params: [f32; 4],
    // x: ground detail, 0 to 1 (see `module_api::MaterialLook`). yzw unused.
    extra: [f32; 4],
}
/// Decoded, GPU-ready form of one mesh's `world::MeshMaterial`. Decoding a
/// PNG (the `image` crate call) happens once per file, cached by name at the
/// App level (see `App::texture_cache`); this is what actually reaches
/// `GpuState::set_custom_meshes`, so rebuilding a material bind group (say,
/// after a roughness slider drag) never re-reads or re-decodes a texture
/// file that hasn't changed, only re-uploads bytes already in memory.
struct MaterialGpuData {
    /// (width, height, RGBA8 pixels), or `None` for an untextured mesh.
    texture_rgba: Option<(u32, u32, Vec<u8>)>,
    roughness: f32,
    metalness: f32,
}
// Per-vertex mesh geometry: a position in the primitive's local (roughly unit)
// space and its surface normal. One shared vertex buffer holds every primitive's
// vertices back to back; each entity instance picks which slice to draw.
#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct MeshVertex {
    position: [f32; 3],
    normal: [f32; 3],
    // Texture coordinates. Zeroed for the three built-in primitives (Cube,
    // Sphere, Plane), which stay untextured in this first pass; a real value
    // only ever comes from an imported model's own parsed UVs.
    uv: [f32; 2],
    // Vertex morph (see `module_api::VertexMorph`): x is the height offset,
    // y and z the start and end of the distance band. All zero for no morph.
    morph: [f32; 3],
    // The normal the vertex blends to as it morphs.
    morph_normal: [f32; 3],
    // The texture coordinates it blends to as it morphs. The same as `uv`
    // for a vertex whose colour does not slide.
    morph_uv: [f32; 2],
    // For liquid surfaces (see `module_api::WaterFlow`), each from -1 to 1:
    // x and z the water's velocity over FLOW_RANGE, then the tide rule (1
    // follows the tide, -1 floods when the tide is higher, 0 still), then
    // for flooding water the still sea level less the vertex's height, over
    // TIDE_REACH. All zero for anything else.
    water: [i16; 4],
}
impl MeshVertex {
    /// A vertex that never morphs.
    fn plain(position: [f32; 3], normal: [f32; 3], uv: [f32; 2]) -> Self {
        MeshVertex {
            position,
            normal,
            uv,
            morph: [0.0; 3],
            morph_normal: [0.0; 3],
            morph_uv: uv,
            water: [0; 4],
        }
    }
    const ATTRIBS: [wgpu::VertexAttribute; 7] = [
        wgpu::VertexAttribute {
            format: wgpu::VertexFormat::Float32x3,
            offset: 0,
            shader_location: 0,
        },
        wgpu::VertexAttribute {
            format: wgpu::VertexFormat::Float32x3,
            offset: std::mem::size_of::<[f32; 3]>() as wgpu::BufferAddress, // 12
            shader_location: 1,
        },
        wgpu::VertexAttribute {
            format: wgpu::VertexFormat::Float32x2,
            offset: std::mem::size_of::<[f32; 6]>() as wgpu::BufferAddress, // 24
            shader_location: 2,
        },
        // Locations 3 to 8 are the instance's.
        wgpu::VertexAttribute {
            format: wgpu::VertexFormat::Float32x3,
            offset: std::mem::size_of::<[f32; 8]>() as wgpu::BufferAddress, // 32
            shader_location: 9,
        },
        wgpu::VertexAttribute {
            format: wgpu::VertexFormat::Float32x3,
            offset: std::mem::size_of::<[f32; 11]>() as wgpu::BufferAddress, // 44
            shader_location: 10,
        },
        wgpu::VertexAttribute {
            format: wgpu::VertexFormat::Float32x2,
            offset: std::mem::size_of::<[f32; 14]>() as wgpu::BufferAddress, // 56
            shader_location: 11,
        },
        wgpu::VertexAttribute {
            format: wgpu::VertexFormat::Snorm16x4,
            offset: std::mem::size_of::<[f32; 16]>() as wgpu::BufferAddress, // 64
            shader_location: 12,
        },
    ];
    fn layout() -> wgpu::VertexBufferLayout<'static> {
        wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<MeshVertex>() as wgpu::BufferAddress,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &Self::ATTRIBS,
        }
    }
}

// --- Primitive geometry, generated once on the CPU ---
// Each primitive is built at roughly unit size (half-extent 0.5 / radius 0.5),
// centred on the origin, so the shader scales them all by the same MESH_SIZE and
// a default entity comes out exactly the size the cube always was. These are
// ordered to match the engine's `Mesh` enum: Cube, Sphere, Plane.

// A unit cube: 36 vertices (6 faces x 2 triangles), each face flat-shaded with
// one outward normal. This reproduces the cube the shader used to synthesise, so
// existing scenes look identical.
fn cube_vertices() -> Vec<MeshVertex> {
    // 8 corners of a cube with half-extent 0.5.
    const C: [[f32; 3]; 8] = [
        [-0.5, -0.5, -0.5],
        [0.5, -0.5, -0.5],
        [0.5, 0.5, -0.5],
        [-0.5, 0.5, -0.5],
        [-0.5, -0.5, 0.5],
        [0.5, -0.5, 0.5],
        [0.5, 0.5, 0.5],
        [-0.5, 0.5, 0.5],
    ];
    // Each face: six corner indices (two triangles) and one outward normal.
    const FACES: [([usize; 6], [f32; 3]); 6] = [
        ([4, 5, 6, 6, 7, 4], [0.0, 0.0, 1.0]),  // +Z front
        ([1, 0, 3, 3, 2, 1], [0.0, 0.0, -1.0]), // -Z back
        ([5, 1, 2, 2, 6, 5], [1.0, 0.0, 0.0]),  // +X right
        ([0, 4, 7, 7, 3, 0], [-1.0, 0.0, 0.0]), // -X left
        ([3, 2, 6, 6, 7, 3], [0.0, 1.0, 0.0]),  // +Y top
        ([0, 1, 5, 5, 4, 0], [0.0, -1.0, 0.0]), // -Y bottom
    ];
    let mut verts = Vec::with_capacity(36);
    for (indices, normal) in FACES {
        for i in indices {
            verts.push(MeshVertex::plain(C[i], normal, [0.0, 0.0]));
        }
    }
    verts
}

// A UV sphere of radius 0.5, built as rings of quads. The surface normal at any
// point on a sphere centred on the origin is just its (unit) direction, so the
// normal is the unit position and the position is that scaled by the radius. The
// poles produce a few degenerate (zero-area) triangles, which draw nothing.
fn sphere_vertices() -> Vec<MeshVertex> {
    use std::f32::consts::PI;
    const LAT: u32 = 12; // rings from pole to pole
    const LON: u32 = 18; // segments around
    const RADIUS: f32 = 0.5;
    let point = |theta: f32, phi: f32| -> [f32; 3] {
        [
            theta.sin() * phi.cos(),
            theta.cos(),
            theta.sin() * phi.sin(),
        ]
    };
    let vert = |unit: [f32; 3]| {
        MeshVertex::plain(
            [unit[0] * RADIUS, unit[1] * RADIUS, unit[2] * RADIUS],
            unit,
            [0.0, 0.0],
        )
    };
    let mut verts = Vec::new();
    for lat in 0..LAT {
        let t0 = PI * lat as f32 / LAT as f32;
        let t1 = PI * (lat + 1) as f32 / LAT as f32;
        for lon in 0..LON {
            let p0 = 2.0 * PI * lon as f32 / LON as f32;
            let p1 = 2.0 * PI * (lon + 1) as f32 / LON as f32;
            let a = point(t0, p0);
            let b = point(t1, p0);
            let c = point(t1, p1);
            let d = point(t0, p1);
            // two triangles per quad: (a, b, c) and (a, c, d)
            verts.push(vert(a));
            verts.push(vert(b));
            verts.push(vert(c));
            verts.push(vert(a));
            verts.push(vert(c));
            verts.push(vert(d));
        }
    }
    verts
}

// A flat, horizontal quad in the XZ plane (a floor tile), facing up (+Y),
// half-extent 0.5 — the same footprint as the cube's base. Back-face culling is
// off, so it's visible from below too.
fn plane_vertices() -> Vec<MeshVertex> {
    let normal = [0.0, 1.0, 0.0];
    let v = |x: f32, z: f32| MeshVertex::plain([x, 0.0, z], normal, [0.0, 0.0]);
    vec![
        v(-0.5, -0.5),
        v(0.5, -0.5),
        v(0.5, 0.5),
        v(0.5, 0.5),
        v(-0.5, 0.5),
        v(-0.5, -0.5),
    ]
}

// Per-entity instance data: world position plus a selected flag (0 or 1).
#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct InstanceRaw {
    position: [f32; 3],
    color: [f32; 3],
    selected: f32,
    scale: [f32; 3],
    emissive: f32,
    // Yaw, pitch and roll in radians (see `world::Rotation`).
    rotation: [f32; 3],
}
impl InstanceRaw {
    // Locations 0-2 belong to the mesh vertex buffer (MeshVertex, which grew
    // a uv attribute at location 2 alongside textures); the instance
    // attributes continue from 3.
    const ATTRIBS: [wgpu::VertexAttribute; 6] = [
        wgpu::VertexAttribute {
            format: wgpu::VertexFormat::Float32x3,
            offset: 0,
            shader_location: 3,
        },
        wgpu::VertexAttribute {
            format: wgpu::VertexFormat::Float32x3,
            offset: std::mem::size_of::<[f32; 3]>() as wgpu::BufferAddress, // 12
            shader_location: 4,
        },
        wgpu::VertexAttribute {
            format: wgpu::VertexFormat::Float32,
            offset: std::mem::size_of::<[f32; 6]>() as wgpu::BufferAddress, // 24
            shader_location: 5,
        },
        wgpu::VertexAttribute {
            format: wgpu::VertexFormat::Float32x3,
            offset: std::mem::size_of::<[f32; 7]>() as wgpu::BufferAddress, // 28
            shader_location: 6,
        },
        wgpu::VertexAttribute {
            format: wgpu::VertexFormat::Float32,
            offset: std::mem::size_of::<[f32; 10]>() as wgpu::BufferAddress, // 40
            shader_location: 7,
        },
        wgpu::VertexAttribute {
            format: wgpu::VertexFormat::Float32x3,
            offset: std::mem::size_of::<[f32; 11]>() as wgpu::BufferAddress, // 44
            shader_location: 8,
        },
    ];
    fn layout() -> wgpu::VertexBufferLayout<'static> {
        wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<InstanceRaw>() as wgpu::BufferAddress,
            step_mode: wgpu::VertexStepMode::Instance,
            attributes: &Self::ATTRIBS,
        }
    }
}
// A screen-space overlay rectangle (text pixel), positioned directly in NDC.
#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct TextInstance {
    offset: [f32; 2], // bottom-left corner in NDC
    size: [f32; 2],   // width/height in NDC
}
impl TextInstance {
    const ATTRIBS: [wgpu::VertexAttribute; 2] = [
        wgpu::VertexAttribute {
            format: wgpu::VertexFormat::Float32x2,
            offset: 0,
            shader_location: 0,
        },
        wgpu::VertexAttribute {
            format: wgpu::VertexFormat::Float32x2,
            offset: std::mem::size_of::<[f32; 2]>() as wgpu::BufferAddress, // 8
            shader_location: 1,
        },
    ];
    fn layout() -> wgpu::VertexBufferLayout<'static> {
        wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<TextInstance>() as wgpu::BufferAddress,
            step_mode: wgpu::VertexStepMode::Instance,
            attributes: &Self::ATTRIBS,
        }
    }
}
// Create (or recreate) the depth texture's view, sized to match the surface.
// Called once at startup and again on every resize.
fn create_depth_view(device: &wgpu::Device, width: u32, height: u32) -> wgpu::TextureView {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("depth texture"),
        size: wgpu::Extent3d {
            width: width.max(1),
            height: height.max(1),
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: DEPTH_FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    });
    texture.create_view(&wgpu::TextureViewDescriptor::default())
}
// Build the 1x1 opaque-white texture every mesh's material bind group falls
// back to when it has no real texture of its own: sampling it always
// returns (1,1,1,1), so multiplying it into an entity's own color in the
// shader is a harmless no-op. This is what lets the three built-in
// primitives (which never get a real texture) and an untextured custom mesh
// share the exact same shader path as a textured one.
fn create_default_white_texture(device: &wgpu::Device, queue: &wgpu::Queue) -> wgpu::TextureView {
    let texture = device.create_texture_with_data(
        queue,
        &wgpu::TextureDescriptor {
            label: Some("default white material texture"),
            size: wgpu::Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        },
        wgpu::util::TextureDataOrder::LayerMajor,
        &[255, 255, 255, 255],
    );
    texture.create_view(&wgpu::TextureViewDescriptor::default())
}
// Upload an already-decoded RGBA8 image (see MaterialGpuData) into a real
// wgpu texture and return its view. Decoding the PNG itself happens once, at
// the App level, cached by file name, so rebuilding a mesh's material bind
// group (a roughness slider drag, say) re-uploads bytes already in memory
// rather than re-reading and re-decoding the file each time.
fn create_material_texture(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    width: u32,
    height: u32,
    rgba: &[u8],
) -> wgpu::TextureView {
    let texture = device.create_texture_with_data(
        queue,
        &wgpu::TextureDescriptor {
            label: Some("material texture"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        },
        wgpu::util::TextureDataOrder::LayerMajor,
        rgba,
    );
    texture.create_view(&wgpu::TextureViewDescriptor::default())
}
// Build one mesh's material bind group (group 1: texture, sampler, params
// uniform). `texture_view` is either this mesh's own loaded texture or the
// shared default white one; `has_texture` tells the shader which, so it
// knows whether to actually sample or just treat the mesh as untextured.
fn create_material_bind_group(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    sampler: &wgpu::Sampler,
    texture_view: &wgpu::TextureView,
    roughness: f32,
    metalness: f32,
    has_texture: bool,
) -> wgpu::BindGroup {
    create_material_bind_group_with(
        device,
        layout,
        sampler,
        texture_view,
        [
            roughness,
            metalness,
            if has_texture { 1.0 } else { 0.0 },
            0.0,
        ],
        [0.0; 4],
    )
}
/// Like `create_material_bind_group`, with the whole `MaterialUniform`
/// params (w is the ripples of a liquid surface; see
/// `MaterialFactory::create_liquid`).
fn create_material_bind_group_with(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    sampler: &wgpu::Sampler,
    texture_view: &wgpu::TextureView,
    params: [f32; 4],
    extra: [f32; 4],
) -> wgpu::BindGroup {
    let uniform = MaterialUniform { params, extra };
    let buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("material params buffer"),
        contents: bytemuck::cast_slice(&[uniform]),
        usage: wgpu::BufferUsages::UNIFORM,
    });
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("material bind group"),
        layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(texture_view),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::Sampler(sampler),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: buffer.as_entire_binding(),
            },
        ],
    })
}
// Turn a string into a pile of screen-space quads, one per lit font pixel.
//
// Coordinates flow: a font pixel lives at some screen pixel (x right, y DOWN
// from the top-left), then we convert that pixel rectangle into NDC (x -1..1
// left..right, y -1..1 bottom..TOP). The Y axis flips, same as project().
//
// Knobs:
//   start_x / start_y : top-left of the text block, in screen pixels
//   pixel             : how many screen pixels each font dot occupies
fn build_text(
    text: &str,
    start_x: f32,
    start_y: f32,
    pixel: f32,
    screen_w: f32,
    screen_h: f32,
) -> Vec<TextInstance> {
    let mut out = Vec::new();
    let mut cursor_x = start_x;
    let mut cursor_y = start_y;
    for c in text.chars() {
        if c == '\n' {
            cursor_x = start_x;
            cursor_y += (font::GLYPH_HEIGHT as f32 + 1.0) * pixel;
            continue;
        }
        let rows = font::glyph(c);
        for (row_i, &bits) in rows.iter().enumerate() {
            for col in 0..font::GLYPH_WIDTH {
                // bit 4 is the leftmost column, bit 0 the rightmost.
                let lit = (bits >> (font::GLYPH_WIDTH - 1 - col)) & 1 == 1;
                if !lit {
                    continue;
                }
                // This font dot's top-left, in screen pixels.
                let sx = cursor_x + col as f32 * pixel;
                let sy = cursor_y + row_i as f32 * pixel;
                // Convert to an NDC rectangle. The quad's offset is its
                // bottom-left corner and it grows +x (right) and +y (up), so we
                // anchor at the dot's BOTTOM edge (sy + pixel) after the flip.
                let ndc_x = sx / screen_w * 2.0 - 1.0;
                let ndc_y_bottom = 1.0 - (sy + pixel) / screen_h * 2.0;
                out.push(TextInstance {
                    offset: [ndc_x, ndc_y_bottom],
                    size: [pixel / screen_w * 2.0, pixel / screen_h * 2.0],
                });
            }
        }
        cursor_x += (font::GLYPH_WIDTH as f32 + 1.0) * pixel;
    }
    out
}
// Build the camera's view-projection matrix from its current state.
//
// The eye orbits the focus point at `distance`, swung around by yaw (around the
// world Y axis) and pitch (elevation). yaw = pitch = 0 puts the eye straight out
// along +Z, i.e. looking down the -Z axis — the old fixed view.
fn camera_matrix(
    focus_x: f32,
    focus_y: f32,
    focus_z: f32,
    distance: f32,
    yaw: f32,
    pitch: f32,
    width: u32,
    height: u32,
) -> Mat4 {
    let aspect = width as f32 / height.max(1) as f32;
    let target = Vec3::new(focus_x, focus_y, focus_z);
    let offset = Vec3::new(
        pitch.cos() * yaw.sin(),
        pitch.sin(),
        pitch.cos() * yaw.cos(),
    ) * distance;
    let eye = target + offset;
    let up = Vec3::Y;
    let view = Mat4::look_at_rh(eye, target, up);
    let proj = camera_projection(aspect);
    proj * view
}
fn camera_view_proj(
    focus_x: f32,
    focus_y: f32,
    focus_z: f32,
    distance: f32,
    yaw: f32,
    pitch: f32,
    width: u32,
    height: u32,
) -> [[f32; 4]; 4] {
    camera_matrix(
        focus_x, focus_y, focus_z, distance, yaw, pitch, width, height,
    )
    .to_cols_array_2d()
}
// The view-projection matrix for one specific Camera-marked entity, looking
// straight along its own yaw (no pitch, since Rotation is yaw-only across
// this whole engine). Shared by Play mode's camera selection below and the
// Inspector's Camera preview, which needs the *selected* camera's view
// specifically, not necessarily whichever one Play would pick (see
// find_camera_entity below for the "which one would Play pick" question).
//
// Deliberately does not reuse camera_matrix/view_forward above: those use
// their own yaw convention (yaw turns the opposite way from an entity's
// Rotation.yaw), built for the orbit camera's own cam_yaw/cam_pitch fields.
// Mixing the two conventions in one function is exactly the kind of mistake
// that already had to be fixed once in physics.rs, so this works out the
// entity's forward vector directly from the documented convention instead:
// yaw 0 faces -Z, and yaw turns clockwise seen from above (towards +X). Now
// done through `Rotation::forward`/`apply`, the one place that convention lives.
fn camera_entity_view_proj(
    world: &frame_engine::world::World,
    id: usize,
    width: u32,
    height: u32,
    origin: DVec3,
) -> Option<[[f32; 4]; 4]> {
    let pos = world.positions.get(id)?;
    let mut rotation = world.rotations.get(id).copied().unwrap_or_default();
    let mut at = DVec3::new(pos.x, pos.y, pos.z);
    // A camera on a character sees from its eyes, where it looks.
    if let Some((eye, look)) = frame_engine::systems::character_eyes(world, id) {
        at = DVec3::from(eye);
        rotation = frame_engine::world::Rotation {
            yaw: look.yaw,
            pitch: look.pitch,
            roll: 0.0,
        };
    }
    // Relative to the render origin, like everything else that is drawn.
    let eye = origin::relative(at, origin);
    // The camera looks along its entity's full orientation: yaw turns it,
    // pitch tips it, roll banks it (the "up" vector tilts with it).
    let forward = Vec3::from(rotation.forward());
    let up = Vec3::from(rotation.apply([0.0, 1.0, 0.0]));
    let aspect = width as f32 / height.max(1) as f32;
    let view = Mat4::look_at_rh(eye, eye + forward, up);
    let proj = camera_projection(aspect);
    Some((proj * view).to_cols_array_2d())
}
// The scene's active Camera entity, if any: the lowest id. What Play mode
// renders from when one exists, and also the entity to leave out of the
// render itself (see build_instances' own `exclude` doc comment) -- both
// uses need the same answer to the same question, "which entity is the
// camera right now", so this is the one place that answers it.
fn find_camera_entity(world: &frame_engine::world::World) -> Option<usize> {
    world
        .cameras
        .iter()
        .enumerate()
        .find_map(|(id, slot)| slot.as_ref().map(|_| id))
}
// The camera's look direction (eye -> target) for a given yaw/pitch. Matches the
// orbit convention, so entering fly mode preserves the current heading.
fn view_forward(yaw: f32, pitch: f32) -> Vec3 {
    Vec3::new(
        -(pitch.cos() * yaw.sin()),
        -pitch.sin(),
        -(pitch.cos() * yaw.cos()),
    )
}
// Project a world point through the view-projection matrix to screen pixels.
fn project(vp: Mat4, x: f32, y: f32, z: f32, width: f32, height: f32) -> Option<(f32, f32)> {
    let clip = vp * Vec4::new(x, y, z, 1.0);
    if clip.w <= 0.0 {
        return None;
    }
    let ndc_x = clip.x / clip.w;
    let ndc_y = clip.y / clip.w;
    let screen_x = (ndc_x * 0.5 + 0.5) * width;
    let screen_y = (1.0 - (ndc_y * 0.5 + 0.5)) * height;
    Some((screen_x, screen_y))
}
// The translate gizmo, projected to screen. `origin` is the selected entity and
// `ends` are the tips of its X, Y, Z arms, all in PHYSICAL pixels (the same space
// as the cursor, so hit-testing is a straight comparison). `len` is how long one
// arm is in world units, which is what converts a screen drag back into a world
// move.
#[derive(Clone, Copy)]
struct GizmoScreen {
    origin: (f32, f32),
    ends: [(f32, f32); 3],
    len: f32,
}

// The same gizmo in egui points, ready to paint, plus which arm is lit up.
#[derive(Clone, Copy)]
struct GizmoDraw {
    origin: egui::Pos2,
    ends: [egui::Pos2; 3],
    active: Option<usize>,
}

// Shortest distance from a point to a line segment, in 2D. Used to decide whether
// the cursor is on a gizmo arm.
fn dist_to_segment(p: (f32, f32), a: (f32, f32), b: (f32, f32)) -> f32 {
    let (vx, vy) = (b.0 - a.0, b.1 - a.1);
    let (wx, wy) = (p.0 - a.0, p.1 - a.1);
    let len2 = vx * vx + vy * vy;
    if len2 <= 0.0001 {
        return (wx * wx + wy * wy).sqrt();
    }
    // Project the point onto the segment, clamped to its ends.
    let t = ((wx * vx + wy * vy) / len2).clamp(0.0, 1.0);
    let (cx, cy) = (a.0 + vx * t, a.1 + vy * t);
    ((p.0 - cx).powi(2) + (p.1 - cy).powi(2)).sqrt()
}
// Build the shared mesh vertex buffer: the three primitives first, then any
// imported models, each with its vertex range recorded. Bucket order here must
// match build_instances: Cube, Sphere, Plane, then customs in the given order
// (sorted by name at the call sites).
fn build_mesh_buffer(
    device: &wgpu::Device,
    customs: &[&frame_engine::assets::MeshData],
) -> (wgpu::Buffer, Vec<std::ops::Range<u32>>) {
    let mut mesh_verts: Vec<MeshVertex> = Vec::new();
    let mut ranges: Vec<std::ops::Range<u32>> = Vec::new();
    let push = |verts: Vec<MeshVertex>,
                mesh_verts: &mut Vec<MeshVertex>,
                ranges: &mut Vec<std::ops::Range<u32>>| {
        let start = mesh_verts.len() as u32;
        mesh_verts.extend(verts);
        ranges.push(start..mesh_verts.len() as u32);
    };
    push(cube_vertices(), &mut mesh_verts, &mut ranges);
    push(sphere_vertices(), &mut mesh_verts, &mut ranges);
    push(plane_vertices(), &mut mesh_verts, &mut ranges);
    for data in customs {
        let verts: Vec<MeshVertex> = data
            .vertices
            .iter()
            .map(|v| MeshVertex::plain(v.position, v.normal, v.uv))
            .collect();
        push(verts, &mut mesh_verts, &mut ranges);
    }
    let buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("mesh vertex buffer"),
        contents: bytemuck::cast_slice(&mesh_verts),
        usage: wgpu::BufferUsages::VERTEX,
    });
    (buffer, ranges)
}

/// The Vulkan instance/adapter/device/queue, shared across every window this
/// app opens (main, Play, and any popped-out dock tab). Each window gets its
/// own Surface, but only ever one Instance and one Device for the whole
/// process. This isn't a style choice: multiple independent Vulkan instances
/// created and destroyed in one process is unstable on at least some NVIDIA
/// driver versions and segfaulted deep inside the driver (not our code) the
/// moment a second one was torn down -- exactly what popping out a tab and
/// docking it back did before this. wgpu's handle types (Instance, Adapter,
/// Device, Queue) are cheap to Clone -- they're just references to the real
/// underlying object -- so sharing them this way doesn't duplicate GPU work.
#[derive(Clone)]
struct SharedGpu {
    instance: wgpu::Instance,
    adapter: wgpu::Adapter,
    device: wgpu::Device,
    queue: wgpu::Queue,
}

// All the long-lived GPU objects, bundled so they travel together.
struct GpuState {
    surface: wgpu::Surface<'static>,
    /// The window the surface draws to, told just before each present so
    /// that on Wayland the next redraw waits for the compositor's frame
    /// callback (see `render`).
    window: Arc<Window>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    // The present modes this window's surface offers (for the overlay).
    present_modes: Vec<wgpu::PresentMode>,
    render_pipeline: wgpu::RenderPipeline,
    /// The same shader with alpha blending and no depth writes, for
    /// transparent module geometry drawn after everything opaque.
    transparent_pipeline: wgpu::RenderPipeline,
    /// Draws modules' grass (see `grass.rs`): blades made in the vertex
    /// shader, drawn after the solid geometry.
    grass_pipeline: wgpu::RenderPipeline,
    /// The grass bind group layout, settings and blade pattern every module's
    /// fields in this window share.
    grass_factory: grass::GrassFactory,
    /// Blades the last main view drew, for the performance overlay.
    grass_drawn: std::cell::Cell<u64>,
    text_pipeline: wgpu::RenderPipeline,
    // One vertex buffer holding every primitive's geometry back to back, plus
    // each primitive's vertex range within it, ordered Cube, Sphere, Plane
    // (matching the engine's `Mesh` enum). Built once; static for the app's life.
    mesh_vertex_buffer: wgpu::Buffer,
    mesh_ranges: Vec<std::ops::Range<u32>>,
    camera_buffer: wgpu::Buffer,
    lights_buffer: wgpu::Buffer,
    /// The sky and fog data for the main viewport's camera (scene bind group
    /// binding 5), and the preview camera's own copy.
    sky_buffer: wgpu::Buffer,
    preview_sky_buffer: wgpu::Buffer,
    /// Draws the sky as one screen-covering triangle before everything else.
    sky_pipeline: wgpu::RenderPipeline,
    scene_bind_group: wgpu::BindGroup,
    depth_view: wgpu::TextureView,
    egui_renderer: egui_wgpu::Renderer,
    // Camera preview (see `render_preview`): a small, fixed-size offscreen
    // target with its own camera uniform and bind group (so writing its
    // view_proj never disturbs `camera_buffer`, the main viewport's own),
    // registered once with egui_renderer so re-rendering into the same
    // texture view each frame is all that's needed to keep the Inspector's
    // preview image current, no re-registration.
    preview_view: wgpu::TextureView,
    preview_depth_view: wgpu::TextureView,
    preview_camera_buffer: wgpu::Buffer,
    preview_bind_group: wgpu::BindGroup,
    preview_texture_id: egui::TextureId,
    // Material bind groups (group 1: texture, sampler, MaterialUniform), one
    // per mesh bucket, same order as mesh_ranges (Cube, Sphere, Plane, then
    // customs by name). The three primitives always point at the shared
    // default (see `default_material_bind_group`); each custom mesh gets its
    // own, rebuilt whenever its material changes or its texture is
    // (re)loaded. Rebuilt as a whole alongside mesh_ranges in
    // set_custom_meshes, so the two never drift out of index-correspondence.
    material_bind_group_layout: wgpu::BindGroupLayout,
    material_sampler: wgpu::Sampler,
    material_bind_groups: Vec<wgpu::BindGroup>,
    // Shadows: a depth-only pass renders every entity from the shadow-casting
    // light into `shadow_map_view`; the entity shader then samples it (scene
    // bind group bindings 2-4). `shadow_buffer` holds the light matrix and
    // bias values; `shadow_pass_bind_group` exposes the same buffer to the
    // shadow pass alone (the scene bind group can't be bound there, since it
    // contains the very texture the pass writes to).
    shadow_pipeline: wgpu::RenderPipeline,
    shadow_buffer: wgpu::Buffer,
    shadow_pass_bind_group: wgpu::BindGroup,
    shadow_map_view: wgpu::TextureView,
    // One GPU side per editor module (see `module_api`).
    module_gpu: Vec<Box<dyn ModuleGpu>>,
    // How high the render origin is above the world's zero, so the fog can
    // tell how high up a surface really is. Set by `sync_modules`, which runs
    // just before each draw with that frame's origin.
    origin_y: f32,
    // The render origin's x and z wrapped to `WAVE_PERIOD`, for the waves,
    // and to `CLOUD_PERIOD`, for the clouds.
    wave_origin: [f32; 2],
    cloud_origin: [f32; 2],
}
impl GpuState {
    /// Builds a GpuState for one window. `shared` is `None` exactly once per
    /// process -- for the very first (main) window -- which is when a new
    /// Instance/Adapter/Device/Queue actually get created; every later
    /// window (Play, a popped-out tab) passes the SharedGpu that call
    /// returned, so only a new Surface is created for it. Always returns the
    /// SharedGpu to use for the next window, whether it was just created or
    /// passed in unchanged, so callers don't need their own branching.
    fn new(
        window: Arc<Window>,
        shared: Option<&SharedGpu>,
        modules: &[Box<dyn EditorModule>],
    ) -> (GpuState, SharedGpu) {
        let size = window.inner_size();
        let (instance, surface, adapter, device, queue) = match shared {
            Some(shared) => {
                let surface = shared.instance.create_surface(window.clone()).unwrap();
                (
                    shared.instance.clone(),
                    surface,
                    shared.adapter.clone(),
                    shared.device.clone(),
                    shared.queue.clone(),
                )
            }
            None => {
                // Vulkan validation layers are on by default in debug
                // builds; turning them off costs us the layer's debug-time
                // GPU-usage warnings, not correctness at runtime.
                let mut instance_desc = wgpu::InstanceDescriptor::new_without_display_handle();
                instance_desc.flags = wgpu::InstanceFlags::empty();
                let instance = wgpu::Instance::new(instance_desc);
                let surface = instance.create_surface(window.clone()).unwrap();
                let adapter =
                    pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
                        power_preference: wgpu::PowerPreference::default(),
                        compatible_surface: Some(&surface),
                        force_fallback_adapter: false,
                    }))
                    .unwrap();
                let (device, queue) =
                    pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default()))
                        .unwrap();
                (instance, surface, adapter, device, queue)
            }
        };
        let shared_out = SharedGpu {
            instance,
            adapter: adapter.clone(),
            device: device.clone(),
            queue: queue.clone(),
        };
        let mut config = surface
            .get_default_config(&adapter, size.width.max(1), size.height.max(1))
            .unwrap();
        // The default is whatever mode the system lists first, which on some
        // drivers never waits for the screen and draws thousands of frames a
        // second for nothing.
        config.present_mode = present_mode_for(vsync_enabled());
        surface.configure(&device, &config);
        let present_modes = surface.get_capabilities(&adapter).present_modes;
        let depth_view = create_depth_view(&device, config.width, config.height);
        // egui's renderer. It draws in its own pass with no depth attachment,
        // so RendererOptions::default() (depth_stencil_format: None) is correct.
        let mut egui_renderer = egui_wgpu::Renderer::new(
            &device,
            config.format,
            egui_wgpu::RendererOptions::default(),
        );
        let camera_uniform = CameraUniform::new(Mat4::IDENTITY.to_cols_array_2d());
        let camera_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("camera buffer"),
            contents: bytemuck::cast_slice(&[camera_uniform]),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });
        // Zeroed lights (every slot's intensity 0.0, so the shader's loop
        // contributes nothing from any of them) until the first real frame
        // fills this in from the world's Light entities.
        let lights_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("lights buffer"),
            contents: bytemuck::cast_slice(&[LightsUniform {
                lights: [LightRaw::zeroed(); MAX_LIGHTS],
            }]),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });
        // The sky and fog data. Zeroed until the first frame fills it in, which
        // means no sky and no fog.
        let sky_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("sky buffer"),
            contents: bytemuck::cast_slice(&[sky::SkyUniform::zeroed()]),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });
        let preview_sky_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("preview sky buffer"),
            contents: bytemuck::cast_slice(&[sky::SkyUniform::zeroed()]),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });
        let scene_bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("scene bind group layout"),
                entries: &[
                    wgpu::BindGroupLayoutEntry {
                        binding: 0,
                        visibility: wgpu::ShaderStages::VERTEX,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Uniform,
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 1,
                        // Lighting is computed per fragment now, not per
                        // vertex, so only the fragment stage needs this.
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Uniform,
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 2,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Uniform,
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 3,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Texture {
                            sample_type: wgpu::TextureSampleType::Depth,
                            view_dimension: wgpu::TextureViewDimension::D2,
                            multisampled: false,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 4,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Comparison),
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 5,
                        // The sky and fog, read per fragment. Vertex shaders
                        // may read it too (a module placing clouds by the
                        // wind's drift, say).
                        visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Uniform,
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    },
                ],
            });
        // --- Shadow map resources (see GpuState::shadow_pipeline) ---
        let shadow_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("shadow buffer"),
            contents: bytemuck::cast_slice(&[ShadowUniform::zeroed()]),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });
        let shadow_map_view = device
            .create_texture(&wgpu::TextureDescriptor {
                label: Some("shadow map"),
                size: wgpu::Extent3d {
                    width: SHADOW_MAP_SIZE,
                    height: SHADOW_MAP_SIZE,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: DEPTH_FORMAT,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                    | wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            })
            .create_view(&wgpu::TextureViewDescriptor::default());
        let shadow_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("shadow sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            compare: Some(wgpu::CompareFunction::LessEqual),
            ..Default::default()
        });
        let scene_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("scene bind group"),
            layout: &scene_bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: camera_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: lights_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: shadow_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::TextureView(&shadow_map_view),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: wgpu::BindingResource::Sampler(&shadow_sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 5,
                    resource: sky_buffer.as_entire_binding(),
                },
            ],
        });
        // --- Material bind group layout (group 1: texture, sampler, params) ---
        // One of these is bound before each mesh's draw call in render()/
        // render_preview(), separate from the scene bind group above (group
        // 0, camera+lights), since different meshes in the same frame need
        // different materials while sharing the same camera and lights.
        let material_bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("material bind group layout"),
                entries: &[
                    wgpu::BindGroupLayoutEntry {
                        binding: 0,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Texture {
                            sample_type: wgpu::TextureSampleType::Float { filterable: true },
                            view_dimension: wgpu::TextureViewDimension::D2,
                            multisampled: false,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 1,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 2,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Uniform,
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    },
                ],
            });
        let material_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("material sampler"),
            address_mode_u: wgpu::AddressMode::Repeat,
            address_mode_v: wgpu::AddressMode::Repeat,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        // Every mesh gets a material bind group; the three primitives (and
        // any custom mesh with no texture of its own) point at this shared
        // 1x1 opaque-white texture, so sampling it and multiplying into the
        // entity's own color is a harmless no-op. Rebuilt fully, alongside
        // mesh_ranges, in set_custom_meshes.
        let default_material_view = create_default_white_texture(&device, &queue);
        let material_bind_groups: Vec<wgpu::BindGroup> = (0..3)
            .map(|_| {
                create_material_bind_group(
                    &device,
                    &material_bind_group_layout,
                    &material_sampler,
                    &default_material_view,
                    0.5,
                    0.0,
                    false,
                )
            })
            .collect();
        // --- Camera preview (see GpuState's own field doc comment) ---
        let preview_texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("camera preview texture"),
            size: wgpu::Extent3d {
                width: PREVIEW_WIDTH,
                height: PREVIEW_HEIGHT,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: config.format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let preview_view = preview_texture.create_view(&wgpu::TextureViewDescriptor::default());
        let preview_depth_view = create_depth_view(&device, PREVIEW_WIDTH, PREVIEW_HEIGHT);
        let preview_camera_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("preview camera buffer"),
            contents: bytemuck::cast_slice(&[CameraUniform::new(
                Mat4::IDENTITY.to_cols_array_2d(),
            )]),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });
        // Its own bind group, not a shared one: writing this camera's
        // view_proj each frame must never touch `camera_buffer`, the
        // viewport's own, since both get drawn in the same frame.
        let preview_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("preview scene bind group"),
            layout: &scene_bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: preview_camera_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    // Same lights as the main viewport: it's the same scene,
                    // just seen from a different camera.
                    resource: lights_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: shadow_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::TextureView(&shadow_map_view),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: wgpu::BindingResource::Sampler(&shadow_sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 5,
                    resource: preview_sky_buffer.as_entire_binding(),
                },
            ],
        });
        let preview_texture_id =
            egui_renderer.register_native_texture(&device, &preview_view, wgpu::FilterMode::Linear);
        // --- Primitive geometry: one shared vertex buffer, built once ---
        // Concatenate every primitive's vertices and remember each one's range,
        // in the engine's Mesh order (Cube, Sphere, Plane). At draw time we bind
        // this buffer and draw the range for whichever primitive an entity uses.
        let (mesh_vertex_buffer, mesh_ranges) = build_mesh_buffer(&device, &[]);
        // --- Entity pipeline (world-space, camera-driven) ---
        let entity_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("entity shader"),
            source: wgpu::ShaderSource::Wgsl(ENTITY_WGSL.into()),
        });
        let entity_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("entity pipeline layout"),
                // Group 0: camera + lights (scene-wide). Group 1: this mesh's
                // own material (texture, sampler, roughness/metalness),
                // rebound before each mesh's draw call.
                bind_group_layouts: &[
                    Some(&scene_bind_group_layout),
                    Some(&material_bind_group_layout),
                ],
                immediate_size: 0,
            });
        let render_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("entity pipeline"),
            layout: Some(&entity_pipeline_layout),
            vertex: wgpu::VertexState {
                module: &entity_shader,
                entry_point: Some("vs_main"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                buffers: &[MeshVertex::layout(), InstanceRaw::layout()],
            },
            fragment: Some(wgpu::FragmentState {
                module: &entity_shader,
                entry_point: Some("fs_main"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: config.format,
                    blend: Some(wgpu::BlendState::REPLACE),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState::default(),
            // Entities test AND write depth: nearer fragments win and record
            // their depth, so a later-drawn far fragment is correctly discarded.
            // This is what makes a solid cube (front faces hide back faces).
            // Depth is reversed (see `NEAR`), so nearer is greater.
            depth_stencil: Some(wgpu::DepthStencilState {
                format: DEPTH_FORMAT,
                depth_write_enabled: Some(true),
                depth_compare: Some(wgpu::CompareFunction::Greater),
                stencil: wgpu::StencilState::default(),
                bias: wgpu::DepthBiasState::default(),
            }),
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });
        // Transparent module geometry: same layout and vertex shader, but the
        // fragment alpha blends over what is already drawn, and it tests depth
        // without writing it so it never hides anything behind it.
        let transparent_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("transparent pipeline"),
            layout: Some(&entity_pipeline_layout),
            vertex: wgpu::VertexState {
                module: &entity_shader,
                entry_point: Some("vs_main"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                buffers: &[MeshVertex::layout(), InstanceRaw::layout()],
            },
            fragment: Some(wgpu::FragmentState {
                module: &entity_shader,
                entry_point: Some("fs_transparent"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: config.format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: Some(wgpu::DepthStencilState {
                format: DEPTH_FORMAT,
                depth_write_enabled: Some(false),
                depth_compare: Some(wgpu::CompareFunction::Greater),
                stencil: wgpu::StencilState::default(),
                bias: wgpu::DepthBiasState::default(),
            }),
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });
        // Grass: the same shader module's grass entry points, with the
        // scene's bind group and the grass data in group 1. No vertex
        // buffers: blades come from their vertex number. Both sides of a
        // blade are drawn.
        let grass_factory = grass::GrassFactory::new(&device);
        let grass_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("grass pipeline layout"),
                bind_group_layouts: &[Some(&scene_bind_group_layout), Some(&grass_factory.layout)],
                immediate_size: 0,
            });
        let grass_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("grass pipeline"),
            layout: Some(&grass_pipeline_layout),
            vertex: wgpu::VertexState {
                module: &entity_shader,
                entry_point: Some("vs_grass"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                buffers: &[],
            },
            fragment: Some(wgpu::FragmentState {
                module: &entity_shader,
                entry_point: Some("fs_grass"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: config.format,
                    blend: Some(wgpu::BlendState::REPLACE),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState {
                cull_mode: None,
                ..wgpu::PrimitiveState::default()
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: DEPTH_FORMAT,
                depth_write_enabled: Some(true),
                depth_compare: Some(wgpu::CompareFunction::Greater),
                stencil: wgpu::StencilState::default(),
                bias: wgpu::DepthBiasState::default(),
            }),
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });
        // --- Shadow pipeline (depth only, from the light's point of view) ---
        let shadow_pass_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("shadow pass bind group layout"),
                entries: &[wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                }],
            });
        let shadow_pass_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("shadow pass bind group"),
            layout: &shadow_pass_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: shadow_buffer.as_entire_binding(),
            }],
        });
        let shadow_shader = device.create_shader_module(wgpu::include_wgsl!("shadow.wgsl"));
        let shadow_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("shadow pipeline layout"),
                bind_group_layouts: &[Some(&shadow_pass_layout)],
                immediate_size: 0,
            });
        let shadow_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("shadow pipeline"),
            layout: Some(&shadow_pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shadow_shader,
                entry_point: Some("vs_main"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                buffers: &[MeshVertex::layout(), InstanceRaw::layout()],
            },
            // Depth only: no fragment stage, no colour target.
            fragment: None,
            // No face culling: a Plane is a single quad and must still cast.
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: Some(wgpu::DepthStencilState {
                format: DEPTH_FORMAT,
                depth_write_enabled: Some(true),
                depth_compare: Some(wgpu::CompareFunction::Less),
                stencil: wgpu::StencilState::default(),
                // Slope-scaled bias: surfaces nearly edge-on to the light get
                // more, which is what stops "shadow acne" stripes.
                bias: wgpu::DepthBiasState {
                    constant: 2,
                    slope_scale: 2.0,
                    clamp: 0.0,
                },
            }),
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });
        // The sky: one triangle covering the screen, drawn first. It reads only
        // the scene bind group, so its layout has no material group. No depth
        // test and no depth write: everything else simply draws over it.
        let sky_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("sky pipeline layout"),
            bind_group_layouts: &[Some(&scene_bind_group_layout)],
            immediate_size: 0,
        });
        let sky_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("sky pipeline"),
            layout: Some(&sky_pipeline_layout),
            vertex: wgpu::VertexState {
                module: &entity_shader,
                entry_point: Some("vs_sky"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                buffers: &[],
            },
            fragment: Some(wgpu::FragmentState {
                module: &entity_shader,
                entry_point: Some("fs_sky"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: config.format,
                    blend: Some(wgpu::BlendState::REPLACE),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: Some(wgpu::DepthStencilState {
                format: DEPTH_FORMAT,
                depth_write_enabled: Some(false),
                depth_compare: Some(wgpu::CompareFunction::Always),
                stencil: wgpu::StencilState::default(),
                bias: wgpu::DepthBiasState::default(),
            }),
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });
        // --- Text/overlay pipeline (screen-space, no camera) ---
        let text_shader = device.create_shader_module(wgpu::include_wgsl!("text.wgsl"));
        let text_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("text pipeline layout"),
            bind_group_layouts: &[], // no camera — positions are already in NDC
            immediate_size: 0,
        });
        let text_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("text pipeline"),
            layout: Some(&text_pipeline_layout),
            vertex: wgpu::VertexState {
                module: &text_shader,
                entry_point: Some("vs_main"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                buffers: &[TextInstance::layout()],
            },
            fragment: Some(wgpu::FragmentState {
                module: &text_shader,
                entry_point: Some("fs_main"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: config.format,
                    blend: Some(wgpu::BlendState::REPLACE),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState::default(),
            // The overlay is screen furniture: it must ALWAYS draw on top and
            // never write depth (Always = ignore the test, write disabled).
            depth_stencil: Some(wgpu::DepthStencilState {
                format: DEPTH_FORMAT,
                depth_write_enabled: Some(false),
                depth_compare: Some(wgpu::CompareFunction::Always),
                stencil: wgpu::StencilState::default(),
                bias: wgpu::DepthBiasState::default(),
            }),
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });
        let module_gpu: Vec<Box<dyn ModuleGpu>> = {
            let context = module_api::GpuContext {
                device: &device,
                queue: &queue,
                material_layout: &material_bind_group_layout,
                material_sampler: &material_sampler,
                grass: &grass_factory,
                scene_layout: &scene_bind_group_layout,
                color_format: config.format,
            };
            modules.iter().map(|m| m.new_gpu(&context)).collect()
        };
        (
            GpuState {
                window: window.clone(),
                surface,
                device,
                queue,
                config,
                present_modes,
                render_pipeline,
                transparent_pipeline,
                grass_pipeline,
                grass_factory,
                grass_drawn: std::cell::Cell::new(0),
                text_pipeline,
                mesh_vertex_buffer,
                mesh_ranges,
                camera_buffer,
                lights_buffer,
                sky_buffer,
                preview_sky_buffer,
                sky_pipeline,
                scene_bind_group,
                depth_view,
                egui_renderer,
                preview_view,
                preview_depth_view,
                preview_camera_buffer,
                preview_bind_group,
                preview_texture_id,
                material_bind_group_layout,
                material_sampler,
                material_bind_groups,
                shadow_pipeline,
                shadow_buffer,
                shadow_pass_bind_group,
                shadow_map_view,
                module_gpu,
                origin_y: 0.0,
                wave_origin: [0.0; 2],
                cloud_origin: [0.0; 2],
            },
            shared_out,
        )
    }
    // Rebuild the mesh vertex buffer with the current set of imported models,
    // and alongside it every mesh's material bind group (primitives first,
    // then customs in the same order), so mesh_ranges and material_bind_groups
    // never drift out of index-correspondence with each other. Called when a
    // project loads, a model is (re)imported, or a mesh's material changes.
    // `customs` pairs each model with its material, in `custom_names` order.
    fn set_custom_meshes(
        &mut self,
        customs: &[(&frame_engine::assets::MeshData, &MaterialGpuData)],
    ) {
        let mesh_data: Vec<&frame_engine::assets::MeshData> =
            customs.iter().map(|(data, _)| *data).collect();
        let (buffer, ranges) = build_mesh_buffer(&self.device, &mesh_data);
        self.mesh_vertex_buffer = buffer;
        self.mesh_ranges = ranges;
        let default_view = create_default_white_texture(&self.device, &self.queue);
        let mut bind_groups: Vec<wgpu::BindGroup> = (0..3)
            .map(|_| {
                create_material_bind_group(
                    &self.device,
                    &self.material_bind_group_layout,
                    &self.material_sampler,
                    &default_view,
                    0.5,
                    0.0,
                    false,
                )
            })
            .collect();
        for (_, material) in customs {
            let (view, has_texture) = match &material.texture_rgba {
                Some((width, height, rgba)) => (
                    create_material_texture(&self.device, &self.queue, *width, *height, rgba),
                    true,
                ),
                None => (
                    create_default_white_texture(&self.device, &self.queue),
                    false,
                ),
            };
            bind_groups.push(create_material_bind_group(
                &self.device,
                &self.material_bind_group_layout,
                &self.material_sampler,
                &view,
                material.roughness,
                material.metalness,
                has_texture,
            ));
        }
        self.material_bind_groups = bind_groups;
    }
    fn resize(&mut self, width: u32, height: u32) {
        if width > 0 && height > 0 {
            self.config.width = width;
            self.config.height = height;
            self.surface.configure(&self.device, &self.config);
            // Depth buffer must track the window size, or the test reads garbage.
            self.depth_view =
                create_depth_view(&self.device, self.config.width, self.config.height);
        }
    }
    /// Render the shadow map for these instances and lights (or just mark
    /// shadows off for the shader). Must be called before the pass that
    /// samples the map, on the same encoder.
    fn shadow_pass(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        instance_buffer: Option<&wgpu::Buffer>,
        instances: &[InstanceRaw],
        group_counts: &[u32],
        lights: &[LightRaw; MAX_LIGHTS],
        view_proj: [[f32; 4]; 4],
    ) {
        let mut extra_bounds: Option<(Vec3, Vec3)> = None;
        for module in &self.module_gpu {
            if let Some((lo, hi)) = module.shadow_bounds() {
                let (lo, hi) = (Vec3::from(lo), Vec3::from(hi));
                extra_bounds = Some(match extra_bounds {
                    Some((a, b)) => (a.min(lo), b.max(hi)),
                    None => (lo, hi),
                });
            }
        }
        let has_module_geometry = self.module_gpu.iter().any(|m| m.has_geometry());
        let mut uniform = compute_shadow(instances, lights, view_proj, extra_bounds);
        uniform.eye = CameraUniform::new(view_proj).eye;
        self.queue
            .write_buffer(&self.shadow_buffer, 0, bytemuck::cast_slice(&[uniform]));
        if instance_buffer.is_none() && !has_module_geometry {
            return;
        }
        if uniform.params[0] < 0.5 {
            return;
        }
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("shadow pass"),
            color_attachments: &[],
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: &self.shadow_map_view,
                depth_ops: Some(wgpu::Operations {
                    load: wgpu::LoadOp::Clear(1.0),
                    store: wgpu::StoreOp::Store,
                }),
                stencil_ops: None,
            }),
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_pipeline(&self.shadow_pipeline);
        pass.set_bind_group(0, &self.shadow_pass_bind_group, &[]);
        if let Some(buffer) = instance_buffer {
            pass.set_vertex_buffer(0, self.mesh_vertex_buffer.slice(..));
            let stride = std::mem::size_of::<InstanceRaw>() as wgpu::BufferAddress;
            let mut instance_start = 0u32;
            for (primitive, count) in group_counts.iter().enumerate() {
                if *count > 0 {
                    let begin = instance_start as wgpu::BufferAddress * stride;
                    pass.set_vertex_buffer(1, buffer.slice(begin..));
                    if let Some(range) = self.mesh_ranges.get(primitive) {
                        pass.draw(range.clone(), 0..*count);
                    }
                }
                instance_start += count;
            }
        }
        let view = module_api::DrawView::from_view_proj(uniform.light_view_proj, true);
        for module in &self.module_gpu {
            module.draw_in_view(&mut pass, &view);
        }
    }
    /// Make this window's module GPU state match `scenes` (one entry per
    /// module, in order; missing entries mean no scene). Cheap to call every
    /// frame.
    fn sync_modules(&mut self, scenes: &[Option<ModuleScene>], origin: DVec3) {
        self.origin_y = origin.y as f32;
        self.wave_origin = [
            origin.x.rem_euclid(WAVE_PERIOD) as f32,
            origin.z.rem_euclid(WAVE_PERIOD) as f32,
        ];
        self.cloud_origin = [
            origin.x.rem_euclid(CLOUD_PERIOD) as f32,
            origin.z.rem_euclid(CLOUD_PERIOD) as f32,
        ];
        for (i, module) in self.module_gpu.iter_mut().enumerate() {
            let scene = scenes.get(i).and_then(|s| s.as_ref());
            module.sync(&self.device, &self.queue, scene);
            // After the sync, so a module places what it just built.
            module.set_render_origin(&self.queue, [origin.x, origin.y, origin.z]);
        }
    }
    // Draw one frame: entities (world-space cubes) then text (screen overlay).
    /// The grass settings for this frame: the quality preference, the
    /// breeze and the wind's clock.
    fn write_grass_settings(&self) {
        let (density, range) = grass_quality().density_and_range();
        let settings = grass::SettingsRaw {
            density,
            range,
            near: grass::GRASS_FULL_NEAR,
            width: grass::BLADE_WIDTH,
            wind: grass::DEFAULT_WIND,
            time: wave_seconds(),
            origin: self.wave_origin,
            ..Default::default()
        };
        self.queue.write_buffer(
            &self.grass_factory.settings,
            0,
            bytemuck::bytes_of(&settings),
        );
    }

    /// `uniform` with `clouds.w` set to 1 when a module draws this window's
    /// clouds (see `ModuleGpu::draws_clouds`), so the sky leaves out its own.
    fn with_module_clouds(&self, mut uniform: sky::SkyUniform) -> sky::SkyUniform {
        uniform.clouds[3] = if self.module_gpu.iter().any(|m| m.draws_clouds()) {
            1.0
        } else {
            0.0
        };
        uniform
    }

    /// Let every module draw with its own pipeline (see
    /// `ModuleGpu::draw_custom_in_view`), with the scene bound as `scene`.
    fn draw_custom(
        &self,
        pass: &mut wgpu::RenderPass<'_>,
        scene: &wgpu::BindGroup,
        view: &module_api::DrawView,
    ) {
        for module in &self.module_gpu {
            if module.has_custom() {
                pass.set_bind_group(0, scene, &[]);
                module.draw_custom_in_view(pass, view);
            }
        }
    }

    /// Draw every module's grass, seen through `view_proj` with the scene
    /// bound as `scene`. Sets the grass pipeline; whoever draws next sets
    /// their own.
    fn draw_grass(
        &self,
        pass: &mut wgpu::RenderPass<'_>,
        scene: &wgpu::BindGroup,
        view_proj: [[f32; 4]; 4],
        view: &module_api::DrawView,
    ) {
        let quality = grass_quality();
        if std::ptr::eq(scene, &self.scene_bind_group) {
            self.grass_drawn.set(0);
        }
        if quality == grass::GrassQuality::Off
            || !self
                .module_gpu
                .iter()
                .any(|m| m.grass().is_some_and(|f| !f.is_empty()))
        {
            return;
        }
        let eye = sky::eye_from_view_proj(&Mat4::from_cols_array_2d(&view_proj));
        pass.set_pipeline(&self.grass_pipeline);
        pass.set_bind_group(0, scene, &[]);
        let mut drawn = 0;
        for module in &self.module_gpu {
            if let Some(field) = module.grass() {
                drawn += field.draw(pass, eye.to_array(), view, quality);
            }
        }
        if std::ptr::eq(scene, &self.scene_bind_group) {
            self.grass_drawn.set(drawn);
        }
    }

    fn render(
        &mut self,
        instances: &[InstanceRaw],
        // How many instances belong to each primitive, in Cube, Sphere, Plane
        // order. `instances` is laid out in that same order, so these counts also
        // give each primitive's contiguous slice of the instance buffer.
        group_counts: &[u32],
        text_instances: &[TextInstance],
        view_proj: [[f32; 4]; 4],
        lights: &[LightRaw; MAX_LIGHTS],
        egui_paint_jobs: &[egui::epaint::ClippedPrimitive],
        egui_textures_delta: &egui::TexturesDelta,
        egui_ppp: f32,
    ) {
        let camera_uniform = CameraUniform::new(view_proj);
        self.queue.write_buffer(
            &self.camera_buffer,
            0,
            bytemuck::cast_slice(&[camera_uniform]),
        );
        let sky_on = sky_enabled();
        self.queue.write_buffer(
            &self.sky_buffer,
            0,
            bytemuck::cast_slice(&[self.with_module_clouds(sky_uniform_for_frame(
                view_proj,
                lights,
                sky_on,
                self.origin_y,
                self.wave_origin,
                self.cloud_origin,
            ))]),
        );
        self.queue.write_buffer(
            &self.lights_buffer,
            0,
            bytemuck::cast_slice(&[LightsUniform { lights: *lights }]),
        );
        self.write_grass_settings();
        // egui: upload any new/changed textures before we start encoding.
        for (id, image_delta) in &egui_textures_delta.set {
            self.egui_renderer
                .update_texture(&self.device, &self.queue, *id, image_delta);
        }
        let egui_screen = egui_wgpu::ScreenDescriptor {
            size_in_pixels: [self.config.width, self.config.height],
            pixels_per_point: egui_ppp,
        };
        let present_mode = present_mode_for(vsync_enabled());
        if self.config.present_mode != present_mode {
            self.config.present_mode = present_mode;
            self.surface.configure(&self.device, &self.config);
        }
        let frame = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(frame)
            | wgpu::CurrentSurfaceTexture::Suboptimal(frame) => frame,
            // Hidden or slow to give a frame: skip this one and try on the
            // next redraw. Reconfiguring would not help and costs a stall.
            wgpu::CurrentSurfaceTexture::Timeout | wgpu::CurrentSurfaceTexture::Occluded => {
                return;
            }
            _ => {
                self.surface.configure(&self.device, &self.config);
                return;
            }
        };
        let instance_buffer = if instances.is_empty() {
            None
        } else {
            Some(
                self.device
                    .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                        label: Some("instance buffer"),
                        contents: bytemuck::cast_slice(instances),
                        usage: wgpu::BufferUsages::VERTEX,
                    }),
            )
        };
        let text_buffer = if text_instances.is_empty() {
            None
        } else {
            Some(
                self.device
                    .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                        label: Some("text buffer"),
                        contents: bytemuck::cast_slice(text_instances),
                        usage: wgpu::BufferUsages::VERTEX,
                    }),
            )
        };
        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("frame encoder"),
            });
        // egui: record its vertex/index uploads into the encoder. Must happen
        // before any render pass is active.
        let egui_user_buffers = self.egui_renderer.update_buffers(
            &self.device,
            &self.queue,
            &mut encoder,
            egui_paint_jobs,
            &egui_screen,
        );
        self.shadow_pass(
            &mut encoder,
            instance_buffer.as_ref(),
            instances,
            group_counts,
            lights,
            view_proj,
        );
        {
            let mut render_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("main pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: 0.12,
                            g: 0.12,
                            b: 0.16,
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                // Clear depth to the farthest (0, reversed) every frame.
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &self.depth_view,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(DEPTH_CLEAR),
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            // The sky first, behind everything (when it is switched on).
            if sky_on {
                render_pass.set_pipeline(&self.sky_pipeline);
                render_pass.set_bind_group(0, &self.scene_bind_group, &[]);
                render_pass.draw(0..3, 0..1);
            }
            // entities — draw each primitive's instances against its own
            // geometry. The shared mesh buffer sits at slot 0; the per-entity
            // instance buffer (grouped by primitive) at slot 1. For each
            // primitive with any instances, draw its vertex range for its
            // contiguous slice of instances.
            render_pass.set_pipeline(&self.render_pipeline);
            render_pass.set_bind_group(0, &self.scene_bind_group, &[]);
            if let Some(buffer) = &instance_buffer {
                render_pass.set_vertex_buffer(0, self.mesh_vertex_buffer.slice(..));
                let stride = std::mem::size_of::<InstanceRaw>() as wgpu::BufferAddress;
                let mut instance_start = 0u32;
                for (primitive, count) in group_counts.iter().enumerate() {
                    if *count > 0 {
                        // Bind just this group's slice of the instance buffer and
                        // draw from instance 0, rather than using a non-zero first
                        // instance (which some backends validate against).
                        let begin = instance_start as wgpu::BufferAddress * stride;
                        render_pass.set_vertex_buffer(1, buffer.slice(begin..));
                        // This mesh's own material (texture/roughness/
                        // metalness), rebound per mesh since different
                        // buckets in the same frame can differ. Falls back to
                        // bind group 0 (always a valid default) if the two
                        // lists briefly disagree in length, the same
                        // tolerance the mesh_ranges lookup just below has.
                        let material = self
                            .material_bind_groups
                            .get(primitive)
                            .unwrap_or(&self.material_bind_groups[0]);
                        render_pass.set_bind_group(1, material, &[]);
                        // Skip a group with no matching range. Happens if the
                        // instance list and mesh buffer briefly disagree.
                        if let Some(range) = self.mesh_ranges.get(primitive) {
                            render_pass.draw(range.clone(), 0..*count);
                        }
                    }
                    instance_start += count;
                }
            }
            // What the camera can see, so modules skip what is out of view.
            let view = module_api::DrawView::from_view_proj(view_proj, false);
            if self.module_gpu.iter().any(|m| m.has_geometry()) {
                render_pass.set_bind_group(1, &self.material_bind_groups[0], &[]);
                for module in &self.module_gpu {
                    // Every module starts from the default material, whatever
                    // the one before it bound.
                    render_pass.set_bind_group(1, &self.material_bind_groups[0], &[]);
                    module.draw_in_view(&mut render_pass, &view);
                }
            }
            self.draw_grass(&mut render_pass, &self.scene_bind_group, view_proj, &view);
            self.draw_custom(&mut render_pass, &self.scene_bind_group, &view);
            if self.module_gpu.iter().any(|m| m.has_transparent()) {
                render_pass.set_pipeline(&self.transparent_pipeline);
                render_pass.set_bind_group(1, &self.material_bind_groups[0], &[]);
                for module in &self.module_gpu {
                    render_pass.set_bind_group(1, &self.material_bind_groups[0], &[]);
                    module.draw_transparent_in_view(&mut render_pass, &view);
                }
            }
            // text overlay (screen-space, drawn on top, no camera)
            render_pass.set_pipeline(&self.text_pipeline);
            if let Some(buffer) = &text_buffer {
                render_pass.set_vertex_buffer(0, buffer.slice(..));
                render_pass.draw(0..6, 0..text_instances.len() as u32);
            }
        }
        // egui pass: layered over the scene (load, don't clear), no depth.
        // egui's renderer requires a RenderPass<'static>, hence forget_lifetime.
        {
            let mut egui_pass = encoder
                .begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("egui pass"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &view,
                        resolve_target: None,
                        depth_slice: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Load,
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                    multiview_mask: None,
                })
                .forget_lifetime();
            self.egui_renderer
                .render(&mut egui_pass, egui_paint_jobs, &egui_screen);
        }
        // egui: free textures it no longer needs, after this frame's draw.
        for id in &egui_textures_delta.free {
            self.egui_renderer.free_texture(id);
        }
        // egui's upload command buffers must be submitted before the main one.
        self.queue.submit(
            egui_user_buffers
                .into_iter()
                .chain(std::iter::once(encoder.finish())),
        );
        // On Wayland this asks the compositor for a frame callback, and
        // winit holds the window's next RedrawRequested until it comes. A
        // window the compositor is not showing (covered by a fullscreen
        // Play window, on another workspace) gets none, so it stops drawing
        // instead of blocking in `get_current_texture` for a second each
        // frame with vsync on, which stalled every window on this thread.
        // Only called here, right before a present that commits it: asked
        // for and never committed, the window would wait for ever. Does
        // nothing on other platforms.
        self.window.pre_present_notify();
        frame.present();
    }
    /// Render the scene from a Camera entity's own point of view into the
    /// small, fixed-size preview texture registered with egui as
    /// `preview_texture_id`. Separate from `render()` deliberately: this
    /// draws to an offscreen texture, not the window's swapchain, has no
    /// egui pass of its own, and uses its own camera bind group so writing
    /// its view_proj can never clobber the main viewport's `camera_buffer`
    /// mid-frame. The caller (the Inspector's Camera preview) is expected to
    /// call this before drawing the egui frame that displays the texture,
    /// same window and same encoder timing `render()` already relies on for
    /// its own camera/lights writes.
    fn render_preview(
        &mut self,
        instances: &[InstanceRaw],
        group_counts: &[u32],
        view_proj: [[f32; 4]; 4],
        lights: &[LightRaw; MAX_LIGHTS],
    ) {
        self.queue.write_buffer(
            &self.preview_camera_buffer,
            0,
            bytemuck::cast_slice(&[CameraUniform::new(view_proj)]),
        );
        let sky_on = sky_enabled();
        self.queue.write_buffer(
            &self.preview_sky_buffer,
            0,
            bytemuck::cast_slice(&[self.with_module_clouds(sky_uniform_for_frame(
                view_proj,
                lights,
                sky_on,
                self.origin_y,
                self.wave_origin,
                self.cloud_origin,
            ))]),
        );
        self.queue.write_buffer(
            &self.lights_buffer,
            0,
            bytemuck::cast_slice(&[LightsUniform { lights: *lights }]),
        );
        let instance_buffer = if instances.is_empty() {
            None
        } else {
            Some(
                self.device
                    .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                        label: Some("preview instance buffer"),
                        contents: bytemuck::cast_slice(instances),
                        usage: wgpu::BufferUsages::VERTEX,
                    }),
            )
        };
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("preview encoder"),
            });
        self.shadow_pass(
            &mut encoder,
            instance_buffer.as_ref(),
            instances,
            group_counts,
            lights,
            view_proj,
        );
        {
            let mut render_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("preview pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &self.preview_view,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: 0.12,
                            g: 0.12,
                            b: 0.16,
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &self.preview_depth_view,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(DEPTH_CLEAR),
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            if sky_on {
                render_pass.set_pipeline(&self.sky_pipeline);
                render_pass.set_bind_group(0, &self.preview_bind_group, &[]);
                render_pass.draw(0..3, 0..1);
            }
            render_pass.set_pipeline(&self.render_pipeline);
            render_pass.set_bind_group(0, &self.preview_bind_group, &[]);
            if let Some(buffer) = &instance_buffer {
                render_pass.set_vertex_buffer(0, self.mesh_vertex_buffer.slice(..));
                let stride = std::mem::size_of::<InstanceRaw>() as wgpu::BufferAddress;
                let mut instance_start = 0u32;
                for (primitive, count) in group_counts.iter().enumerate() {
                    if *count > 0 {
                        let begin = instance_start as wgpu::BufferAddress * stride;
                        render_pass.set_vertex_buffer(1, buffer.slice(begin..));
                        let material = self
                            .material_bind_groups
                            .get(primitive)
                            .unwrap_or(&self.material_bind_groups[0]);
                        render_pass.set_bind_group(1, material, &[]);
                        if let Some(range) = self.mesh_ranges.get(primitive) {
                            render_pass.draw(range.clone(), 0..*count);
                        }
                    }
                    instance_start += count;
                }
            }
            // What the camera can see, so modules skip what is out of view.
            let view = module_api::DrawView::from_view_proj(view_proj, false);
            if self.module_gpu.iter().any(|m| m.has_geometry()) {
                render_pass.set_bind_group(1, &self.material_bind_groups[0], &[]);
                for module in &self.module_gpu {
                    // Every module starts from the default material, whatever
                    // the one before it bound.
                    render_pass.set_bind_group(1, &self.material_bind_groups[0], &[]);
                    module.draw_in_view(&mut render_pass, &view);
                }
            }
            self.draw_grass(&mut render_pass, &self.preview_bind_group, view_proj, &view);
            self.draw_custom(&mut render_pass, &self.preview_bind_group, &view);
            if self.module_gpu.iter().any(|m| m.has_transparent()) {
                render_pass.set_pipeline(&self.transparent_pipeline);
                render_pass.set_bind_group(1, &self.material_bind_groups[0], &[]);
                for module in &self.module_gpu {
                    render_pass.set_bind_group(1, &self.material_bind_groups[0], &[]);
                    module.draw_transparent_in_view(&mut render_pass, &view);
                }
            }
        }
        self.queue.submit(std::iter::once(encoder.finish()));
    }
}
/// Which tab is showing in the right-hand inspector dock.
/// A dockable tool panel, shown as a tab in the right-hand egui_dock area. These
/// can be dragged, tabbed together, and split apart at runtime. The 3D viewport
/// is deliberately NOT one of these — it stays the fixed background the docks
/// leave uncovered, so its input routing and transparency are unchanged.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Tab {
    Viewport,
    Scene,
    Inspector,
    Scripts,
    Source,
}
/// Which tab is showing in the bottom console dock.
#[derive(Clone, Copy, PartialEq)]
enum ConsoleTab {
    Output,
    Terminal,
    Assets,
    Prefabs,
    Scenes,
}
/// A command chosen from the toolbar menus this frame, applied after the egui
/// pass. The menu closure can't borrow `self`, so it stages the choice here and
/// we dispatch it afterwards — the same lift-then-write-back pattern the
/// selection and inspector edits use.
/// Which screen the editor is showing. It starts at the launcher; creating or
/// opening a project loads its scene and switches to the editor proper.
#[derive(Clone, Copy, PartialEq, Eq)]
enum AppMode {
    Launcher,
    Editor,
    /// A game on its own: no launcher and no editor window, only the game
    /// window, and closing it quits. Started by `run_player`.
    Player,
}

/// A choice made on the launcher screen this frame, applied after the egui pass
/// (so the blocking file dialog doesn't run mid-draw) — the same lift-then-act
/// pattern the toolbar menus use.
enum LauncherAction {
    NewProject,
    OpenProject,
    OpenRecent(std::path::PathBuf),
    PlayRecent(std::path::PathBuf),
    OpenSettings(std::path::PathBuf),
    ConfirmDelete(std::path::PathBuf),
    CancelDelete,
    DeleteProject(std::path::PathBuf),
}

enum MenuAction {
    OpenScene,
    ImportModel,
    ImportAudio,
    ImportTexture,
    SaveScene,
    SaveSceneAs,
    ReloadScene,
    CloseProject,
    Undo,
    Redo,
    SpawnEntity,
    DespawnSelected,
    ClearSelection,
    TogglePause,
    StepOnce,
    ToggleHelp,
    ToggleStats,
    OpenEditorSettings,
    BuildGame,
    PlayScene,
    About,
    Quit,
}
/// An action that would throw away unsaved changes, so it asks first when the
/// scene has any: quitting (menu or the window's close button), closing the
/// project, reloading the scene from disk, or opening a different scene.
#[derive(Clone, Copy, PartialEq, Eq)]
enum GuardedAction {
    Quit,
    CloseProject,
    ReloadScene,
    OpenScene,
    /// Open the scene named in `pending_scene_switch` (the Scenes tab).
    SwitchScene,
}

/// What the user chose in the unsaved-changes prompt.
enum UnsavedChoice {
    Save,
    Discard,
    Cancel,
}

/// The selected entity's editable state, lifted out of the world for the
/// Inspector to edit and written back after the egui pass.
/// The selected entity's editable state, lifted out of the world for the
/// Inspector to edit and written back after the egui pass. A named struct
/// rather than a plain tuple: std's auto-derived PartialEq/Clone/etc. for
/// tuples only go up to 12 elements, and this grew past that once Light was
/// added as a 13th. A named struct has no such cap.
#[derive(Clone, PartialEq)]
struct EditedEntity {
    id: usize,
    pos: Position,
    vel: Velocity,
    color: frame_engine::world::Color,
    controlled: bool,
    scale: frame_engine::world::Scale,
    material: frame_engine::world::Material,
    rotation: frame_engine::world::Rotation,
    script_source: Option<String>,
    mesh: Mesh,
    is_static: bool,
    has_gravity: bool,
    // Whether rapier3d simulates this entity instead of the hand-rolled
    // movement/gravity/collision systems. Needs `is_static` or `has_gravity`
    // to actually do anything (see `physics::Physics`); ticking this alone
    // is a harmless no-op, not an error, the same "retried every tick"
    // tolerance `Physics::sync_new_and_removed` already has for it.
    is_rigid_body: bool,
    light: Option<Light>,
    sound: Option<Sound>,
    is_camera: bool,
    parent: Option<frame_engine::world::Parent>,
    ui_text: Option<frame_engine::world::UiText>,
    ui_image: Option<frame_engine::world::UiImage>,
    character: Option<frame_engine::world::CharacterBody>,
    // The entity's extension components, for editor modules' Inspector sections.
    ext: ExtEdit,
}

/// Source Control tab: a read-only view of the open project's git state —
/// branch, ahead/behind its upstream, and the changed files. No push, pull, or
/// commit happens here (that's what keeps the editor out of credential
/// handling); this answers "what state am I in?" and the terminal does the rest.
fn source_tab_ui(ui: &mut egui::Ui, summary: &Option<GitSummary>) {
    let Some(s) = summary else {
        ui.label("This project isn't inside a git repository.");
        ui.add_space(4.0);
        ui.weak("Run `git init` in the project folder to start one.");
        return;
    };
    ui.horizontal(|ui| {
        ui.label("Branch:");
        ui.strong(&s.branch);
    });
    match (&s.upstream, s.ahead_behind) {
        (Some(up), Some((ahead, behind))) => {
            ui.horizontal(|ui| {
                ui.label("Upstream:");
                ui.strong(up);
            });
            let state = match (ahead, behind) {
                (0, 0) => "Up to date with upstream".to_string(),
                (a, 0) => format!("{a} commit(s) ahead, ready to push"),
                (0, b) => format!("{b} commit(s) behind — pull to catch up"),
                (a, b) => format!("{a} ahead, {b} behind — diverged"),
            };
            ui.weak(state);
            ui.add_space(2.0);
            ui.weak("(as of the last fetch made outside the editor)");
        }
        _ => {
            ui.weak("No upstream branch configured.");
        }
    }
    ui.separator();
    if s.files.is_empty() {
        ui.colored_label(
            egui::Color32::from_rgb(0x7c, 0xc5, 0x7c),
            "Working tree clean — nothing to commit",
        );
        return;
    }
    ui.label(format!("{} change(s):", s.files.len()));
    ui.add_space(4.0);
    for f in &s.files {
        ui.horizontal(|ui| {
            let (label, color) = if f.staged {
                (
                    format!("{} (staged)", f.label),
                    egui::Color32::from_rgb(0x7c, 0xc5, 0x7c),
                )
            } else {
                (
                    f.label.to_string(),
                    egui::Color32::from_rgb(0xd6, 0xa8, 0x4c),
                )
            };
            ui.colored_label(color, label);
            ui.monospace(&f.path);
        });
    }
}

/// Render a small shaded preview of a model on the CPU. The mesh is already
/// unit normalised, so a fixed camera angle frames everything: rotate, project
/// orthographically, z buffer, one light. 64x64 is plenty for a browser tile.
fn render_thumbnail(data: &frame_engine::assets::MeshData) -> egui::ColorImage {
    const SIZE: usize = 64;
    // Flat rgba bytes, filled with the background colour.
    let mut pixels = vec![0u8; SIZE * SIZE * 4];
    for px in pixels.chunks_exact_mut(4) {
        px.copy_from_slice(&[30, 30, 34, 255]);
    }
    let mut depth = vec![f32::NEG_INFINITY; SIZE * SIZE];

    // Fixed view: yaw then pitch, looking down -Z after rotation.
    let (yaw, pitch) = (0.7f32, -0.45f32);
    let (sy, cy) = yaw.sin_cos();
    let (sp, cp) = pitch.sin_cos();
    let rotate = |p: [f32; 3]| {
        let (x, y, z) = (p[0], p[1], p[2]);
        let (x1, z1) = (x * cy + z * sy, -x * sy + z * cy);
        let (y2, z2) = (y * cp - z1 * sp, y * sp + z1 * cp);
        [x1, y2, z2]
    };
    let light = {
        let l = [0.4f32, 0.7, 0.6];
        let len = (l[0] * l[0] + l[1] * l[1] + l[2] * l[2]).sqrt();
        [l[0] / len, l[1] / len, l[2] / len]
    };
    // Unit coords sit in -0.5..0.5. Scale by 0.9 to leave a margin.
    let to_px = |v: f32| (v * 0.9 + 0.5) * SIZE as f32;

    for tri in data.vertices.chunks_exact(3) {
        let p: Vec<[f32; 3]> = tri.iter().map(|v| rotate(v.position)).collect();
        let n = {
            let m = [
                (tri[0].normal[0] + tri[1].normal[0] + tri[2].normal[0]) / 3.0,
                (tri[0].normal[1] + tri[1].normal[1] + tri[2].normal[1]) / 3.0,
                (tri[0].normal[2] + tri[1].normal[2] + tri[2].normal[2]) / 3.0,
            ];
            rotate(m)
        };
        let shade = (n[0] * light[0] + n[1] * light[1] + n[2] * light[2])
            .abs()
            .clamp(0.15, 1.0);
        let color = [
            (216.0 * shade) as u8,
            (206.0 * shade) as u8,
            (170.0 * shade) as u8,
            255,
        ];
        // Screen space corners. Y flips because pixels count down.
        let ax = to_px(p[0][0]);
        let ay = to_px(-p[0][1]);
        let bx = to_px(p[1][0]);
        let by = to_px(-p[1][1]);
        let cx = to_px(p[2][0]);
        let cyp = to_px(-p[2][1]);
        let min_x = ax.min(bx).min(cx).floor().max(0.0) as usize;
        let max_x = (ax.max(bx).max(cx).ceil() as usize).min(SIZE - 1);
        let min_y = ay.min(by).min(cyp).floor().max(0.0) as usize;
        let max_y = (ay.max(by).max(cyp).ceil() as usize).min(SIZE - 1);
        let area = (bx - ax) * (cyp - ay) - (by - ay) * (cx - ax);
        if area.abs() <= f32::EPSILON {
            continue;
        }
        for py in min_y..=max_y {
            for px in min_x..=max_x {
                let (fx, fy) = (px as f32 + 0.5, py as f32 + 0.5);
                // Barycentric weights. Same sign as the area means inside.
                let w0 = (bx - fx) * (cyp - fy) - (by - fy) * (cx - fx);
                let w1 = (cx - fx) * (ay - fy) - (cyp - fy) * (ax - fx);
                let w2 = (ax - fx) * (by - fy) - (ay - fy) * (bx - fx);
                let inside =
                    (w0 >= 0.0 && w1 >= 0.0 && w2 >= 0.0) || (w0 <= 0.0 && w1 <= 0.0 && w2 <= 0.0);
                if !inside {
                    continue;
                }
                let z = (w0 * p[0][2] + w1 * p[1][2] + w2 * p[2][2]) / area;
                let i = py * SIZE + px;
                if z > depth[i] {
                    depth[i] = z;
                    pixels[i * 4..i * 4 + 4].copy_from_slice(&color);
                }
            }
        }
    }
    egui::ColorImage::from_rgba_unmultiplied([SIZE, SIZE], &pixels)
}

/// Prefabs tab: one row per saved prefab, each with a Spawn button. Prefabs
/// are created from the Inspector's "Save as Prefab" field, not here — this
/// tab is read-only browsing plus the one action, the same shape the Assets
/// tab has for models it doesn't itself import. Returns the name of the
/// prefab whose Spawn button was clicked this frame, if any; the caller (who
/// has world/camera access, this function doesn't) does the actual spawn.
fn prefabs_tab_ui(
    ui: &mut egui::Ui,
    prefabs: &std::collections::BTreeMap<String, frame_engine::world::Prefab>,
) -> Option<String> {
    let mut spawn_request = None;
    if prefabs.is_empty() {
        ui.weak("No prefabs yet. Select an entity in the Inspector and use \"Save as Prefab\".");
        return spawn_request;
    }
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            for name in prefabs.keys() {
                ui.horizontal(|ui| {
                    ui.label(name);
                    if ui.button("Spawn").clicked() {
                        spawn_request = Some(name.clone());
                    }
                });
            }
        });
    spawn_request
}

/// Assets tab: a small browser for the project's assets folder. Folders can be
/// opened and created; files are listed with a tag for models. Import
/// happens through File > Import model, File > Import audio (audio lands
/// in whichever folder is open here) or File > Import texture, or by
/// dropping files on the window. A model tile also gets a Material button,
/// opening a small panel below the grid to assign its texture and set its
/// roughness/metalness (see world::MeshMaterial's doc comment for why these
/// live on the model rather than per entity). Returns the edited material
/// for the caller to write into World::mesh_meta and re-upload to the GPU,
/// the same lift-then-write-back shape every other tab here already uses.
fn assets_tab_ui(
    ui: &mut egui::Ui,
    assets_root: Option<&std::path::Path>,
    subdir: &mut std::path::PathBuf,
    new_folder: &mut String,
    thumbnails: &std::collections::HashMap<String, egui::TextureHandle>,
    move_pending: &mut Option<std::path::PathBuf>,
    mesh_meta: &std::collections::BTreeMap<String, frame_engine::world::MeshMeta>,
    image_names: &[String],
    material_editing: &mut Option<String>,
) -> Option<(String, frame_engine::world::MeshMaterial)> {
    let mut material_result = None;
    let Some(root) = assets_root else {
        ui.weak("Open a project to browse its assets.");
        return None;
    };
    let current = root.join(&*subdir);
    // Header: where we are, plus Up when inside a subfolder.
    ui.horizontal(|ui| {
        if !subdir.as_os_str().is_empty() {
            if ui.button("Up").clicked() {
                subdir.pop();
            }
        }
        let shown = if subdir.as_os_str().is_empty() {
            "assets/".to_string()
        } else {
            format!("assets/{}/", subdir.display())
        };
        ui.monospace(shown);
        // A marked file pastes into whichever folder is open.
        if let Some(src) = move_pending.clone() {
            let file = src
                .file_name()
                .and_then(|f| f.to_str())
                .unwrap_or("file")
                .to_string();
            if ui.button(format!("Paste '{file}' here")).clicked() {
                let dest = current.join(&file);
                if src != dest {
                    let _ = std::fs::rename(&src, &dest);
                }
                *move_pending = None;
            }
            if ui.button("Cancel move").clicked() {
                *move_pending = None;
            }
        }
    });
    // New folder row.
    ui.horizontal(|ui| {
        ui.add(
            egui::TextEdit::singleline(new_folder)
                .hint_text("new folder name")
                .desired_width(160.0),
        );
        let name = new_folder.trim().to_string();
        let ok = !name.is_empty() && !name.contains(['/', '\\']);
        if ui
            .add_enabled(ok, egui::Button::new("New folder"))
            .clicked()
        {
            let _ = std::fs::create_dir_all(current.join(&name));
            new_folder.clear();
        }
    });
    ui.separator();
    // Listing: folders first, then files, both sorted by name.
    let mut dirs: Vec<String> = Vec::new();
    let mut files: Vec<String> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&current) {
        for entry in entries.flatten() {
            let Some(name) = entry.file_name().to_str().map(String::from) else {
                continue;
            };
            match entry.file_type() {
                Ok(t) if t.is_dir() => dirs.push(name),
                Ok(_) => files.push(name),
                Err(_) => {}
            }
        }
    } else {
        ui.weak("No assets folder yet. Importing a model or audio creates it.");
        return None;
    }
    dirs.sort();
    files.sort();
    if dirs.is_empty() && files.is_empty() {
        ui.weak("(empty)");
    }
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                for dir in &dirs {
                    if ui.button(format!("[{dir}]")).clicked() {
                        subdir.push(dir);
                    }
                }
            });
            if !dirs.is_empty() && !files.is_empty() {
                ui.add_space(4.0);
            }
            // Files as tiles: preview square, name underneath, Move below.
            ui.horizontal_wrapped(|ui| {
                for file in &files {
                    let stem = file.rsplit_once('.').map(|(st, _)| st).unwrap_or(file);
                    ui.allocate_ui(egui::vec2(84.0, 112.0), |ui| {
                        ui.vertical(|ui| {
                            match thumbnails.get(stem) {
                                Some(handle) => {
                                    ui.add(
                                        egui::Image::new(handle)
                                            .fit_to_exact_size(egui::vec2(64.0, 64.0)),
                                    );
                                }
                                None => {
                                    // Not a model, or not loaded. A flat square
                                    // with the extension keeps the grid even.
                                    let (rect, _) = ui.allocate_exact_size(
                                        egui::vec2(64.0, 64.0),
                                        egui::Sense::hover(),
                                    );
                                    ui.painter().rect_filled(
                                        rect,
                                        2.0,
                                        egui::Color32::from_rgb(45, 45, 50),
                                    );
                                    let ext = file
                                        .rsplit_once('.')
                                        .map(|(_, e)| e.to_uppercase())
                                        .unwrap_or_default();
                                    ui.painter().text(
                                        rect.center(),
                                        egui::Align2::CENTER_CENTER,
                                        ext,
                                        egui::FontId::monospace(12.0),
                                        egui::Color32::GRAY,
                                    );
                                }
                            }
                            ui.add(egui::Label::new(egui::RichText::new(file).small()).truncate());
                            ui.horizontal(|ui| {
                                if ui.small_button("Move").clicked() {
                                    *move_pending = Some(current.join(file));
                                }
                                // A model tile (one with a thumbnail) also
                                // gets a Material button, toggling the panel
                                // below the grid for this mesh specifically.
                                if mesh_meta.contains_key(stem) {
                                    if ui.small_button("Material").clicked() {
                                        *material_editing =
                                            if material_editing.as_deref() == Some(stem) {
                                                None
                                            } else {
                                                Some(stem.to_string())
                                            };
                                    }
                                }
                            });
                        });
                    });
                }
            });
        });
    // Material panel: only while a model's Material button is toggled on,
    // and only while that model still exists (it can vanish out from under
    // this if the assets folder changes on disk between frames).
    if let Some(name) = material_editing.clone() {
        if let Some(meta) = mesh_meta.get(&name) {
            let mut mat = meta.material.clone();
            let mut changed = false;
            ui.separator();
            ui.horizontal(|ui| {
                ui.label(format!("Material: {name}"));
                if ui.small_button("Close").clicked() {
                    *material_editing = None;
                }
            });
            ui.horizontal(|ui| {
                ui.label("Texture");
                egui::ComboBox::from_id_salt("material_texture_picker")
                    .selected_text(mat.texture.clone().unwrap_or_else(|| "(none)".to_string()))
                    .show_ui(ui, |ui| {
                        if ui
                            .selectable_label(mat.texture.is_none(), "(none)")
                            .clicked()
                            && mat.texture.is_some()
                        {
                            mat.texture = None;
                            changed = true;
                        }
                        for img in image_names {
                            let selected = mat.texture.as_deref() == Some(img.as_str());
                            if ui.selectable_label(selected, img.as_str()).clicked() && !selected {
                                mat.texture = Some(img.clone());
                                changed = true;
                            }
                        }
                    });
                if image_names.is_empty() {
                    ui.weak("(no images yet — File > Import texture…)");
                }
            });
            changed |= ui
                .add(egui::Slider::new(&mut mat.roughness, 0.0..=1.0).text("Roughness"))
                .changed();
            changed |= ui
                .add(egui::Slider::new(&mut mat.metalness, 0.0..=1.0).text("Metalness"))
                .changed();
            if changed {
                material_result = Some((name, mat));
            }
        } else {
            *material_editing = None;
        }
    }
    material_result
}

/// Scene tab: entities shown as a parent/child tree rather than a flat list,
/// using each entity's `Parent.entity` link (see `World::apply_parenting`
/// and the Parent section of the Inspector). An entity's parent has to be
/// present in this same `entity_ids` snapshot to nest under it; a dangling
/// or missing parent (despawned, or pointing at itself) falls back to being
/// shown at the root level, the same tolerance `apply_parenting` itself
/// gives a stale link rather than erroring.
///
/// Dragging one row onto another requests reparenting: drop it, and the
/// dragged entity becomes a child of whichever row it was released over.
/// Returns that request (dragged entity, new parent) for the caller to
/// apply to the world — this function only ever reads `entity_parents`, it
/// doesn't own the world and can't write to it directly. `None` covers "no
/// drag happened", "released over empty space", and "rejected" (dropping
/// onto itself or one of its own descendants, which would create a cycle)
/// alike; the caller doesn't need to tell those apart.
fn scene_tab_ui(
    ui: &mut egui::Ui,
    entity_ids: &[usize],
    entity_parents: &std::collections::HashMap<usize, usize>,
    selection: &mut Option<usize>,
) -> Option<(usize, usize)> {
    ui.label(format!("{} entities", entity_ids.len()));
    ui.weak("Drag an entity onto another to make it a child.");
    ui.separator();

    let valid: std::collections::HashSet<usize> = entity_ids.iter().copied().collect();
    let mut children: std::collections::BTreeMap<usize, Vec<usize>> =
        std::collections::BTreeMap::new();
    let mut roots: Vec<usize> = Vec::new();
    for &id in entity_ids {
        match entity_parents.get(&id) {
            Some(&parent) if parent != id && valid.contains(&parent) => {
                children.entry(parent).or_default().push(id);
            }
            _ => roots.push(id),
        }
    }

    // Built on egui's own purpose-built drag-and-drop API —
    // `dnd_drag_source`/`dnd_hover_payload`/`dnd_release_payload`, copied
    // from egui's own official drag-and-drop demo
    // (egui_demo_lib::demo::drag_and_drop) — after two hand-rolled attempts
    // at `Response`/`Sense`/`ui.interact` fell short in different ways (one
    // broke plain clicking, the other never actually recognised a drag at
    // all: `response.dragged()` from a bare `ui.allocate_exact_size(...,
    // Sense::click_and_drag())` never became true here, twice confirmed,
    // even though the identical `Sense` works fine through
    // `dnd_drag_source`). The payload is just the dragged entity's own id
    // (a bare `usize`, satisfies the `Any + Send + Sync` bound trivially).
    //
    // A real drag reliably works through `dnd_drag_source`, but its own
    // click-vs-drag split is a small movement distance, not time, and an
    // ordinary click's natural hand-wobble easily crosses it — so a plain
    // click routinely got misread as a (very short) drag, and once egui's
    // own bookkeeping decides that, `Response::clicked()` never fires for
    // it. Rather than fight `dnd_drag_source`'s own internal sensing (which
    // is what actually makes real dragging work, twice now confirmed
    // fragile to touch), the hold-time requirement is layered entirely on
    // top of it, resolved once here after the whole tree has rendered for
    // the frame — never inside a single row's own closure, where the *other*
    // row involved (the actual drag origin, if this row is just a hovered
    // target) might read or clear the shared timer before or after this one
    // in an order this code doesn't control.
    let mut dragging_row: Option<usize> = None;
    let mut raw_reparent: Option<(usize, usize)> = None;
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            // Guards against a cycle in the Parent links (A parents B parents
            // A) — apply_parenting only ever walks one entity's single parent
            // at a time each tick, so it never has to notice a cycle, but a
            // tree view actually descends the graph and would recurse forever
            // without this.
            let mut visiting = std::collections::HashSet::new();
            for &id in &roots {
                scene_tree_node(
                    ui,
                    id,
                    0,
                    &children,
                    &mut visiting,
                    selection,
                    &mut dragging_row,
                    &mut raw_reparent,
                );
            }
        });

    // The one place that reads or clears the shared drag-hold timer, run
    // strictly after every row's own turn this frame, so there's no
    // question of which row's closure ran first.
    let now = ui.input(|i| i.time);
    if dragging_row.is_none() {
        // No row is reporting an active drag this frame. Either nothing's
        // being pressed at all (nothing stored, nothing to do), or a drag
        // that was active as of last frame just ended — released, however
        // briefly held.
        let key = scene_tree_drag_hold_key();
        if let Some((origin, start_time)) = ui.ctx().data(|d| d.get_temp::<(usize, f64)>(key)) {
            ui.ctx().data_mut(|d| d.remove::<(usize, f64)>(key));
            let held_long_enough = now - start_time >= scene_tree_drag_hold_seconds();
            match raw_reparent {
                // Landed on another row, and was actually held long enough
                // to count as a deliberate drag rather than a click's
                // wobble: apply it, once the usual cycle/self-drop check
                // clears it.
                Some((child, target)) if child == origin && held_long_enough => {
                    if !entity_is_or_descends(&children, child, target) {
                        return Some((child, target));
                    }
                }
                // Released without ever reaching another valid row, and
                // never held long enough to be a real drag either: this was
                // just a click that happened to wobble a pixel or two.
                // egui's own `Response::clicked()` won't fire for it (as
                // far as `dnd_drag_source`'s own bookkeeping is concerned
                // this was a drag, however short), so this is the only
                // place that ever will.
                None if !held_long_enough => {
                    *selection = Some(origin);
                }
                // Held long enough but never landed on anything (dropped in
                // empty space), or landed on something but never held long
                // enough — neither counts as a click nor a reparent.
                _ => {}
            }
        }
    }
    None
}

/// True if `needle` is `root` itself or nested anywhere under it in the
/// tree. Used to reject a drag-drop that would make an entity its own
/// descendant's child — a cycle `apply_parenting` would then walk forever.
fn entity_is_or_descends(
    children: &std::collections::BTreeMap<usize, Vec<usize>>,
    root: usize,
    needle: usize,
) -> bool {
    if root == needle {
        return true;
    }
    children.get(&root).is_some_and(|kids| {
        kids.iter()
            .any(|&k| entity_is_or_descends(children, k, needle))
    })
}

/// How long a row has to be held, by default, before it counts as a
/// deliberate drag rather than a click. An ordinary click is over in roughly a
/// tenth of a second, so 0.3 s keeps a click's hand-wobble from becoming a
/// drag while still feeling snappy. It was 1.5 s, which felt far too slow; it
/// is now a preference (Edit > Editor settings…). See the long comments in
/// `scene_tab_ui` and `scene_tree_node` for why this is layered on top of
/// `dnd_drag_source` rather than built into the widget itself.
const SCENE_TREE_DRAG_HOLD_DEFAULT: f32 = 0.3;

/// The live hold time, as `f32` bits. A process-wide atomic rather than a
/// field on `App`, because the Scene tab is drawn by free functions and can
/// also be popped out into a window with its own egui context.
static SCENE_TREE_DRAG_HOLD_BITS: std::sync::atomic::AtomicU32 =
    std::sync::atomic::AtomicU32::new(SCENE_TREE_DRAG_HOLD_DEFAULT.to_bits());

fn scene_tree_drag_hold_seconds() -> f64 {
    f32::from_bits(SCENE_TREE_DRAG_HOLD_BITS.load(std::sync::atomic::Ordering::Relaxed)) as f64
}

fn set_scene_tree_drag_hold_seconds(seconds: f32) {
    SCENE_TREE_DRAG_HOLD_BITS.store(seconds.to_bits(), std::sync::atomic::Ordering::Relaxed);
}

/// Single fixed key for the one drag-hold timer `scene_tab_ui` tracks
/// (origin entity id, start time). Only one drag can be in flight across
/// the whole tree at a time, so this doesn't need to be per-row.
fn scene_tree_drag_hold_key() -> egui::Id {
    egui::Id::new("scene_tree_drag_hold")
}

/// One row (and its children, recursively) of the Scene tree. See
/// `scene_tab_ui` for the cycle guard `visiting` provides and how
/// `dragging_row`/`raw_reparent` get resolved, with the hold-time check,
/// after the whole tree has rendered for the frame.
///
/// Deliberately a simple first slice of drag-and-drop: the row being
/// dragged doesn't visually detach into its own floating layer the way
/// egui's own demo does it for a plain flat list (this tree recurses, and
/// following that exact pattern for a nested structure wasn't worth the
/// risk this pass) — only the row currently under the cursor gets an
/// outline, and only once the drag has actually been held long enough to
/// count.
fn scene_tree_node(
    ui: &mut egui::Ui,
    id: usize,
    depth: usize,
    children: &std::collections::BTreeMap<usize, Vec<usize>>,
    visiting: &mut std::collections::HashSet<usize>,
    selection: &mut Option<usize>,
    dragging_row: &mut Option<usize>,
    raw_reparent: &mut Option<(usize, usize)>,
) {
    if !visiting.insert(id) {
        ui.colored_label(
            egui::Color32::from_rgb(0xe0, 0x5c, 0x5c),
            format!("Entity {id} (parent cycle, stopped here)"),
        );
        return;
    }
    ui.horizontal(|ui| {
        ui.add_space(depth as f32 * 16.0);
        let label = if depth == 0 {
            format!("Entity {id}")
        } else {
            format!("↳ Entity {id}")
        };
        let is_selected = *selection == Some(id);

        // Fourth construction of this row's widget, and a different root
        // cause than the last three fixes assumed. Found via egui's own
        // issue tracker (emilk/egui#2730): a maintainer states plainly that
        // `dnd_drag_source` "only detects drags, not clicks" — it isn't a
        // real `Sense::click_and_drag()` widget at all, just a drag sensor
        // that happens to also expose a `Response`. That's the actual
        // reason clicking still felt broken even in the version confirmed
        // to drag correctly: `dnd_drag_source` was never really trying to
        // sense a click in the first place.
        //
        // The two earlier bare-`Sense::click_and_drag()` attempts
        // (`ui.allocate_exact_size(..., Sense::click_and_drag())`) never
        // got `response.dragged()` to become true either, but both relied
        // on that call's own auto-generated widget id. The one thing
        // `dnd_drag_source`'s (reliably working) drag-sensing has that
        // those attempts didn't is an *explicit*, stable id — plausible
        // enough as the actual difference that it's worth testing directly
        // rather than a fifth guess at the `Sense` itself: `ui
        // .allocate_space` reserves the row's rect with no interaction of
        // its own (nothing to compete with), then one single
        // `ui.interact(rect, item_id, Sense::click_and_drag())` call, using
        // the same explicit id scheme `dnd_drag_source` used, senses both a
        // real click and a real drag on the one widget.
        let item_id = egui::Id::new(("scene_tree_row", id));
        let row_height = ui.text_style_height(&egui::TextStyle::Body) + 4.0;
        let desired_size = egui::vec2(ui.available_width().max(80.0), row_height);
        let (_auto_id, rect) = ui.allocate_space(desired_size);
        let response = ui.interact(rect, item_id, egui::Sense::click_and_drag());

        if ui.is_rect_visible(rect) {
            if is_selected {
                ui.painter().rect_filled(
                    rect,
                    2.0,
                    egui::Color32::from_rgba_unmultiplied(0xff, 0xd5, 0x4c, 50),
                );
            }
            ui.painter().text(
                rect.left_center() + egui::vec2(4.0, 0.0),
                egui::Align2::LEFT_CENTER,
                &label,
                egui::FontId::default(),
                ui.visuals().text_color(),
            );
        }

        if response.clicked() {
            *selection = Some(id);
        }

        // TEMPORARY diagnostic. Three fixes in a row have each looked right
        // and then not actually worked on Luke's machine — this prints what
        // egui itself thinks is happening on the row that's actually being
        // pressed, to the terminal the editor was launched from, so if this
        // fourth attempt is *also* wrong, the next round is a real
        // diagnosis instead of a fifth guess. Safe to delete once clicking
        // is confirmed working again.
        if response.clicked() || response.dragged() {
            eprintln!(
                "[scene_tree] entity {id}: clicked={} dragged={} hovered={}",
                response.clicked(),
                response.dragged(),
                response.hovered(),
            );
        }

        // This row is the drag's own origin this frame. Start (or keep
        // advancing) the one shared timer `scene_tab_ui` resolves after
        // every row has had its turn — never resolved here, since a
        // different row (the one this drag might land on) could run before
        // or after this one within the same frame, and reading or clearing
        // shared state from either side of that race is exactly the bug
        // an earlier attempt at this hold-gate had.
        if response.dragged() {
            *dragging_row = Some(id);
            let key = scene_tree_drag_hold_key();
            let now = ui.input(|i| i.time);
            let start_time = ui
                .ctx()
                .data(|d| d.get_temp::<(usize, f64)>(key))
                .filter(|(origin, _)| *origin == id)
                .map_or(now, |(_, start)| start);
            ui.ctx().data_mut(|d| d.insert_temp(key, (id, start_time)));
            if now - start_time >= scene_tree_drag_hold_seconds() {
                // Only past the hold does this row actually become a real
                // drag-and-drop source; a shorter hold never reaches here,
                // so `DragAndDrop`'s payload is never set for it and every
                // other row's `dnd_hover_payload` stays quiet.
                egui::DragAndDrop::set_payload(ui.ctx(), id);
            }
        }

        // `dnd_hover_payload` fires on every row currently under the
        // pointer while *any* row's drag is in progress. Only actually show
        // the "drop here" outline once that drag's own timer has cleared
        // the hold threshold — otherwise every plain click would flash it
        // again, exactly the bug this whole hold-gate exists to fix.
        if response.dnd_hover_payload::<usize>().is_some() {
            let held_long_enough = ui
                .ctx()
                .data(|d| d.get_temp::<(usize, f64)>(scene_tree_drag_hold_key()))
                .is_some_and(|(_, start_time)| {
                    ui.input(|i| i.time) - start_time >= scene_tree_drag_hold_seconds()
                });
            if held_long_enough {
                // Four line segments rather than `Painter::rect_stroke`:
                // this codebase already draws the translate gizmo with
                // `line_segment`, proven to compile against the pinned
                // egui version, whereas `rect_stroke`'s exact parameter
                // list has changed across egui versions (a trailing
                // `StrokeKind` in some).
                let r = response.rect;
                let color = egui::Color32::from_rgb(0xff, 0xd5, 0x4c);
                let painter = ui.painter();
                painter.line_segment([r.left_top(), r.right_top()], egui::Stroke::new(1.5, color));
                painter.line_segment(
                    [r.right_top(), r.right_bottom()],
                    egui::Stroke::new(1.5, color),
                );
                painter.line_segment(
                    [r.right_bottom(), r.left_bottom()],
                    egui::Stroke::new(1.5, color),
                );
                painter.line_segment(
                    [r.left_bottom(), r.left_top()],
                    egui::Stroke::new(1.5, color),
                );
            }
        }

        // Fires exactly once, on the frame a drag is released over this
        // exact row — the dragged entity's id is the payload it was
        // carrying. Recorded here unconditionally; `scene_tab_ui` is the
        // one place that checks it against the hold timer and decides
        // whether it actually counts.
        if let Some(dragged_id) = response.dnd_release_payload::<usize>() {
            *raw_reparent = Some((*dragged_id, id));
        }
    });
    if let Some(kids) = children.get(&id) {
        for &child in kids {
            scene_tree_node(
                ui,
                child,
                depth + 1,
                children,
                visiting,
                selection,
                dragging_row,
                raw_reparent,
            );
        }
    }
    visiting.remove(&id);
}

/// One custom Inspector field for the currently selected entity, lifted out
/// for `inspector_tab_ui` to edit and written back to `World::insert_dynamic`
/// after the pass, the same lift-then-write pattern every other field here
/// already uses. A plain struct rather than a tuple, since `min`/`max` make
/// a bare `(String, String, f64)` awkward to read at either end.
#[derive(Clone, PartialEq)]
struct EditableCustomField {
    /// Which entity this value belongs to, captured at the same time as the
    /// value itself. Needed because the write-back happens after the pass,
    /// by which point self.selected may already have moved to a different
    /// entity (the user clicked a new one in Scene this same frame); using
    /// that instead of a captured id could silently write a value onto the
    /// wrong entity.
    entity: usize,
    /// The dynamic component name; what gets passed to `insert_dynamic`.
    name: String,
    label: String,
    value: f64,
    min: Option<f64>,
    max: Option<f64>,
}

/// Inspector tab: the selected entity's properties.
fn inspector_tab_ui(
    ui: &mut egui::Ui,
    edited: &mut Option<EditedEntity>,
    script_library: &std::collections::BTreeMap<String, String>,
    script_filter: &mut String,
    custom_mesh_names: &[String],
    // Plugin-declared custom fields the selected entity currently has a
    // value under. Empty for an entity with none.
    custom_fields: &mut [EditableCustomField],
    // The open project's assets folder, for the Sound picker. None when no
    // project is open.
    assets_root: Option<&std::path::Path>,
    // The main window's Camera preview texture, registered with egui once
    // and re-rendered into every frame a Camera entity is selected (see
    // GpuState::render_preview). None on the launcher screen, before the
    // window/GPU exist.
    preview_texture_id: Option<egui::TextureId>,
    // The "Save as Prefab" name field, persisted across frames.
    new_prefab_name: &mut String,
    // Set to (entity id, name) when "Save as Prefab" is clicked this frame;
    // the caller does the actual file write afterward, since this function
    // only draws UI and has no world/filesystem access.
    save_prefab_request: &mut Option<(usize, String)>,
    modules: &RefCell<Vec<Box<dyn EditorModule>>>,
) {
    match edited {
        Some(EditedEntity {
            id,
            pos,
            vel,
            color,
            controlled,
            scale,
            material,
            rotation,
            script_source,
            mesh,
            is_static,
            has_gravity,
            is_rigid_body,
            light,
            sound,
            is_camera,
            parent,
            ui_text,
            ui_image,
            character,
            ext,
        }) => {
            ui.heading(format!("Entity {id}"));
            ui.add_space(4.0);
            section_label(ui, "Position");
            ui.horizontal(|ui| {
                ui.add(egui::DragValue::new(&mut pos.x).speed(1.0).prefix("x "));
                ui.add(egui::DragValue::new(&mut pos.y).speed(1.0).prefix("y "));
                ui.add(egui::DragValue::new(&mut pos.z).speed(1.0).prefix("z "));
            });
            ui.add_space(4.0);
            section_label(ui, "Velocity");
            ui.horizontal(|ui| {
                ui.add(egui::DragValue::new(&mut vel.dx).speed(0.1).prefix("dx "));
                ui.add(egui::DragValue::new(&mut vel.dy).speed(0.1).prefix("dy "));
                ui.add(egui::DragValue::new(&mut vel.dz).speed(0.1).prefix("dz "));
            });
            ui.add_space(4.0);
            section_label(ui, "Color");
            let mut rgb = [color.r, color.g, color.b];
            if ui.color_edit_button_rgb(&mut rgb).changed() {
                color.r = rgb[0];
                color.g = rgb[1];
                color.b = rgb[2];
            }
            ui.add_space(4.0);
            section_label(ui, "Scale");
            ui.horizontal(|ui| {
                ui.add(
                    egui::DragValue::new(&mut scale.x)
                        .speed(0.05)
                        .range(0.1..=100.0)
                        .prefix("x "),
                );
                ui.add(
                    egui::DragValue::new(&mut scale.y)
                        .speed(0.05)
                        .range(0.1..=100.0)
                        .prefix("y "),
                );
                ui.add(
                    egui::DragValue::new(&mut scale.z)
                        .speed(0.05)
                        .range(0.1..=100.0)
                        .prefix("z "),
                );
                ui.add_space(4.0);
                ui.label("Material");
                ui.add(egui::Slider::new(&mut material.emissive, 0.0..=1.0).text("emissive"));
            });
            ui.add_space(4.0);
            section_label(ui, "Rotation");
            ui.horizontal(|ui| {
                for (label, angle, hover) in [
                    (
                        "yaw",
                        &mut rotation.yaw,
                        "Turn left or right, around the vertical axis.",
                    ),
                    (
                        "pitch",
                        &mut rotation.pitch,
                        "Tip the nose up (positive) or down.",
                    ),
                    (
                        "roll",
                        &mut rotation.roll,
                        "Bank around the forward axis; positive leans the right side down.",
                    ),
                ] {
                    let mut degrees = angle.to_degrees();
                    if ui
                        .add(
                            egui::DragValue::new(&mut degrees)
                                .speed(1.0)
                                .prefix(format!("{label} "))
                                .suffix("°"),
                        )
                        .on_hover_text(hover)
                        .changed()
                    {
                        *angle = degrees.to_radians();
                    }
                }
            });
            ui.add_space(4.0);
            section_label(ui, "Mesh");
            let mesh_label: String = match mesh {
                Mesh::Cube => "Cube".to_string(),
                Mesh::Sphere => "Sphere".to_string(),
                Mesh::Plane => "Plane".to_string(),
                Mesh::Custom(name) => name.clone(),
                Mesh::Empty => "None".to_string(),
            };
            egui::ComboBox::from_id_salt("mesh_picker")
                .selected_text(mesh_label)
                .show_ui(ui, |ui| {
                    ui.selectable_value(mesh, Mesh::Cube, "Cube");
                    ui.selectable_value(mesh, Mesh::Sphere, "Sphere");
                    ui.selectable_value(mesh, Mesh::Plane, "Plane");
                    ui.selectable_value(mesh, Mesh::Empty, "None")
                        .on_hover_text(
                            "No mesh: not drawn, no collision, no physics body. \
                         Select it from the Scene list or by clicking where it \
                         is, and move it with the gizmo.",
                        );
                    // Imported models come after the primitives.
                    for name in custom_mesh_names {
                        ui.selectable_value(mesh, Mesh::Custom(name.clone()), name);
                    }
                });
            ui.add_space(4.0);
            outerface_checkbox(ui, controlled, "Controlled (WASD)");
            outerface_checkbox(ui, is_static, "Static (immovable)");
            outerface_checkbox(ui, has_gravity, "Gravity (falls)");
            outerface_checkbox(ui, is_rigid_body, "Physics (rapier3d)").on_hover_text(
                "Simulated by rapier3d instead of the built-in movement/\
                     gravity/collision. Needs Static, Gravity or Controlled too. \
                     With Controlled it's a character: input moves it and it \
                     stops at walls. Add Gravity to make it fall.",
            );
            outerface_checkbox(ui, is_camera, "Camera").on_hover_text(
                "Play mode renders from the first Camera entity in the \
                     scene (lowest id) instead of the editor's own orbit \
                     camera. It looks along its own Rotation: yaw turns it, \
                     pitch tips it up or down, roll banks it. \
                     Attach it to a Parent below to have it ride along on \
                     another entity.",
            );
            if *is_camera {
                match preview_texture_id {
                    Some(texture_id) => {
                        ui.add(
                            egui::Image::new((
                                texture_id,
                                egui::vec2(PREVIEW_WIDTH as f32, PREVIEW_HEIGHT as f32) * 0.75,
                            ))
                            .corner_radius(4.0),
                        )
                        .on_hover_text(
                            "What this camera sees, updated live while it's \
                             selected. Position/Rotation edits above, and any \
                             Parent it rides on, show up here immediately.",
                        );
                    }
                    None => {
                        ui.weak("(camera preview unavailable)");
                    }
                }
            }
            ui.add_space(8.0);
            section_label(ui, "Character");
            let mut has_body = character.is_some();
            if outerface_checkbox(ui, &mut has_body, "Character body")
                .on_hover_text(
                    "A person-shaped capsule (turning it on also turns on \
                     Controlled, Gravity and Physics): \
                     it walks where it looks at its own speeds, runs (Shift), \
                     crouches (C), steps up small ledges and jumps. In the Play \
                     window the mouse turns it. A Camera on it, or attached \
                     to it, sees from its eyes. Its drawn size follows the capsule.",
                )
                .changed()
            {
                *character = if has_body {
                    // A body only moves as a Controlled physics character
                    // that falls, so those come with it.
                    *controlled = true;
                    *is_rigid_body = true;
                    *has_gravity = true;
                    *is_static = false;
                    Some(frame_engine::world::CharacterBody::default())
                } else {
                    None
                };
            }
            if let Some(body) = character {
                let row =
                    |ui: &mut egui::Ui, label: &str, value: &mut f32, speed: f64, unit: &str| {
                        ui.horizontal(|ui| {
                            ui.label(label);
                            ui.add(egui::DragValue::new(value).speed(speed).suffix(unit));
                        });
                    };
                row(ui, "Height", &mut body.height, 0.01, " m");
                row(ui, "Radius", &mut body.radius, 0.01, " m");
                row(ui, "Eye height", &mut body.eye_height, 0.01, " m");
                row(ui, "Crouch height", &mut body.crouch_height, 0.01, " m");
                row(ui, "Walk speed", &mut body.walk_speed, 0.05, " m/s");
                row(ui, "Run speed", &mut body.run_speed, 0.05, " m/s");
                row(ui, "Crouch speed", &mut body.crouch_speed, 0.05, " m/s");
                row(ui, "Jump height", &mut body.jump_height, 0.01, " m");
                row(ui, "Step height", &mut body.step_height, 0.01, " m");
                row(ui, "Steepest slope", &mut body.max_slope, 0.5, "°");
                row(ui, "Mass", &mut body.mass, 0.5, " kg");
                row(ui, "Air control", &mut body.air_control, 0.01, "");
                *body = body.sanitized();
            }
            ui.add_space(8.0);
            section_label(ui, "Light");
            let mut has_light = light.is_some();
            if outerface_checkbox(ui, &mut has_light, "Light source").changed() {
                *light = if has_light {
                    Some(Light::default())
                } else {
                    None
                };
            }
            if let Some(l) = light {
                let mut is_point = matches!(l.kind, LightKind::Point { .. });
                egui::ComboBox::from_id_salt("light_kind_picker")
                    .selected_text(if is_point { "Point" } else { "Directional" })
                    .show_ui(ui, |ui| {
                        if ui
                            .selectable_value(&mut is_point, false, "Directional")
                            .clicked()
                            || ui.selectable_value(&mut is_point, true, "Point").clicked()
                        {
                            // Switching kind starts that kind fresh rather than
                            // trying to carry a direction into a range or back;
                            // the two don't correspond to each other.
                            l.kind = if is_point {
                                LightKind::Point { range: 200.0 }
                            } else {
                                LightKind::Directional {
                                    direction: [0.4, 0.8, 0.6],
                                }
                            };
                        }
                    });
                match &mut l.kind {
                    LightKind::Directional { direction } => {
                        ui.horizontal(|ui| {
                            ui.label("Direction");
                            ui.add(
                                egui::DragValue::new(&mut direction[0])
                                    .speed(0.01)
                                    .prefix("x: "),
                            );
                            ui.add(
                                egui::DragValue::new(&mut direction[1])
                                    .speed(0.01)
                                    .prefix("y: "),
                            );
                            ui.add(
                                egui::DragValue::new(&mut direction[2])
                                    .speed(0.01)
                                    .prefix("z: "),
                            );
                        });
                    }
                    LightKind::Point { range } => {
                        ui.horizontal(|ui| {
                            ui.label("Range");
                            ui.add(egui::DragValue::new(range).speed(1.0).range(0.0..=f32::MAX));
                        });
                    }
                }
                ui.horizontal(|ui| {
                    ui.label("Intensity");
                    ui.add(
                        egui::DragValue::new(&mut l.intensity)
                            .speed(0.05)
                            .range(0.0..=f32::MAX),
                    );
                });
            }
            ui.add_space(8.0);
            section_label(ui, "Sound");
            let mut has_sound = sound.is_some();
            if outerface_checkbox(ui, &mut has_sound, "Sound source").changed() {
                *sound = if has_sound {
                    Some(Sound::default())
                } else {
                    None
                };
            }
            if let Some(s) = sound {
                ui.horizontal(|ui| {
                    ui.label("File");
                    let shown = if s.name.is_empty() {
                        "(none)".to_string()
                    } else {
                        s.name.clone()
                    };
                    egui::ComboBox::from_id_salt("sound_picker")
                        .selected_text(shown)
                        .show_ui(ui, |ui| {
                            // Listed only while the dropdown is open, so a
                            // file added outside the editor shows up without
                            // walking the assets folder every frame.
                            let names = assets_root.map(list_audio).unwrap_or_default();
                            if names.is_empty() {
                                ui.weak("No audio yet. File > Import audio…");
                            }
                            for name in names {
                                let label = name.clone();
                                ui.selectable_value(&mut s.name, name, label);
                            }
                        });
                });
                let missing = !s.name.is_empty()
                    && assets_root.is_some_and(|root| find_audio(root, &s.name).is_none());
                if missing {
                    ui.colored_label(
                        egui::Color32::from_rgb(230, 170, 60),
                        "File not found under assets/",
                    );
                }
                if ui.button("Play now").clicked() {
                    s.play = true;
                }
            }
            ui.add_space(8.0);
            section_label(ui, "UI Text");
            let mut has_ui_text = ui_text.is_some();
            if outerface_checkbox(ui, &mut has_ui_text, "Screen-space text overlay").changed() {
                *ui_text = if has_ui_text {
                    Some(frame_engine::world::UiText::default())
                } else {
                    None
                };
            }
            if let Some(t) = ui_text {
                ui.horizontal(|ui| {
                    ui.label("Text");
                    ui.text_edit_singleline(&mut t.text);
                });
                ui.horizontal(|ui| {
                    section_label(ui, "Position");
                    ui.add(
                        egui::DragValue::new(&mut t.x)
                            .speed(0.01)
                            .range(0.0..=1.0)
                            .prefix("x "),
                    );
                    ui.add(
                        egui::DragValue::new(&mut t.y)
                            .speed(0.01)
                            .range(0.0..=1.0)
                            .prefix("y "),
                    );
                });
                ui.horizontal(|ui| {
                    ui.label("Font size");
                    ui.add(
                        egui::DragValue::new(&mut t.font_size)
                            .speed(0.5)
                            .range(4.0..=200.0),
                    );
                });
                section_label(ui, "Color");
                let mut rgb = [t.color.r, t.color.g, t.color.b];
                if ui.color_edit_button_rgb(&mut rgb).changed() {
                    t.color.r = rgb[0];
                    t.color.g = rgb[1];
                    t.color.b = rgb[2];
                }
            }
            ui.add_space(8.0);
            section_label(ui, "UI Image");
            let mut has_ui_image = ui_image.is_some();
            if outerface_checkbox(ui, &mut has_ui_image, "Screen-space image overlay").changed() {
                *ui_image = if has_ui_image {
                    Some(frame_engine::world::UiImage::default())
                } else {
                    None
                };
            }
            if let Some(img) = ui_image {
                ui.horizontal(|ui| {
                    ui.label("File");
                    let shown = if img.name.is_empty() {
                        "(none)".to_string()
                    } else {
                        img.name.clone()
                    };
                    egui::ComboBox::from_id_salt("ui_image_picker")
                        .selected_text(shown)
                        .show_ui(ui, |ui| {
                            // Listed only while the dropdown is open, same
                            // reasoning as the Sound file picker above: a
                            // texture added outside the editor shows up
                            // without walking the assets folder every frame.
                            let names = assets_root.map(list_images).unwrap_or_default();
                            if names.is_empty() {
                                ui.weak("No images yet. File > Import texture…");
                            }
                            for name in names {
                                let label = name.clone();
                                ui.selectable_value(&mut img.name, name, label);
                            }
                        });
                });
                ui.horizontal(|ui| {
                    section_label(ui, "Position");
                    ui.add(
                        egui::DragValue::new(&mut img.x)
                            .speed(0.01)
                            .range(0.0..=1.0)
                            .prefix("x "),
                    );
                    ui.add(
                        egui::DragValue::new(&mut img.y)
                            .speed(0.01)
                            .range(0.0..=1.0)
                            .prefix("y "),
                    );
                });
                ui.horizontal(|ui| {
                    ui.label("Size");
                    ui.add(
                        egui::DragValue::new(&mut img.width)
                            .speed(0.01)
                            .range(0.0..=1.0)
                            .prefix("w "),
                    );
                    ui.add(
                        egui::DragValue::new(&mut img.height)
                            .speed(0.01)
                            .range(0.0..=1.0)
                            .prefix("h "),
                    );
                });
            }
            for module in modules.borrow_mut().iter_mut() {
                module.inspect(ui, ext);
            }
            ui.add_space(8.0);
            section_label(ui, "Parent");
            let mut has_parent = parent.is_some();
            if outerface_checkbox(ui, &mut has_parent, "Attached to another entity").changed() {
                *parent = if has_parent {
                    Some(frame_engine::world::Parent::default())
                } else {
                    None
                };
            }
            if let Some(p) = parent {
                ui.horizontal(|ui| {
                    ui.label("Parent entity id");
                    ui.add(egui::DragValue::new(&mut p.entity).speed(1.0));
                });
                ui.label("Offset (in the parent's own space)")
                    .on_hover_text(
                        "x is to the parent's right, z is behind it — these \
                         turn with the parent's yaw, so the offset stays in \
                         the same relative spot as it turns.",
                    );
                ui.horizontal(|ui| {
                    ui.add(
                        egui::DragValue::new(&mut p.offset_x)
                            .speed(0.1)
                            .prefix("x "),
                    );
                    ui.add(
                        egui::DragValue::new(&mut p.offset_y)
                            .speed(0.1)
                            .prefix("y "),
                    );
                    ui.add(
                        egui::DragValue::new(&mut p.offset_z)
                            .speed(0.1)
                            .prefix("z "),
                    );
                });
                ui.horizontal(|ui| {
                    ui.label("Offset yaw");
                    let mut offset_yaw_degrees = p.offset_yaw.to_degrees();
                    if ui
                        .add(
                            egui::DragValue::new(&mut offset_yaw_degrees)
                                .speed(1.0)
                                .suffix("°"),
                        )
                        .changed()
                    {
                        p.offset_yaw = offset_yaw_degrees.to_radians();
                    }
                });
            }
            ui.add_space(8.0);
            section_label(ui, "Script");
            if script_library.is_empty() {
                ui.weak("No scripts yet — add some in the Script Editor tab.");
            } else {
                let selected_text = script_source
                    .clone()
                    .unwrap_or_else(|| "(none)".to_string());
                egui::ComboBox::from_id_salt("script_picker")
                    .selected_text(selected_text)
                    .show_ui(ui, |ui| {
                        ui.add(egui::TextEdit::singleline(script_filter).hint_text("filter…"));
                        ui.separator();
                        ui.selectable_value(script_source, None, "(none)");
                        let needle = script_filter.to_lowercase();
                        for name in script_library.keys() {
                            if needle.is_empty() || name.to_lowercase().contains(&needle) {
                                ui.selectable_value(
                                    script_source,
                                    Some(name.clone()),
                                    name.as_str(),
                                );
                            }
                        }
                    });
                if let Some(name) = script_source.as_ref() {
                    if !script_library.contains_key(name) {
                        ui.weak(format!("(uses missing script '{name}')"));
                    }
                }
            }
            if !custom_fields.is_empty() {
                ui.add_space(8.0);
                ui.label("Plugin Fields");
                for field in custom_fields.iter_mut() {
                    ui.horizontal(|ui| {
                        ui.label(field.label.as_str());
                        match (field.min, field.max) {
                            (Some(lo), Some(hi)) => {
                                ui.add(egui::Slider::new(&mut field.value, lo..=hi));
                            }
                            _ => {
                                let mut drag = egui::DragValue::new(&mut field.value).speed(0.1);
                                if let Some(lo) = field.min {
                                    drag = drag.range(lo..=f64::MAX);
                                }
                                if let Some(hi) = field.max {
                                    drag = drag.range(f64::MIN..=hi);
                                }
                                ui.add(drag);
                            }
                        }
                    });
                }
            }
            ui.add_space(8.0);
            section_label(ui, "Prefab");
            ui.horizontal(|ui| {
                ui.add(
                    egui::TextEdit::singleline(new_prefab_name)
                        .hint_text("prefab name")
                        .desired_width(140.0),
                );
                let can_save = !new_prefab_name.trim().is_empty();
                if ui
                    .add_enabled(can_save, egui::Button::new("Save as Prefab"))
                    .on_hover_text(
                        "Saves this entity's mesh, scale, color, material, \
                         rotation, script, and physics/light/sound/camera/\
                         parent settings as a reusable prefab file. Not \
                         linked afterward — editing the prefab later never \
                         changes entities already spawned from it, only new \
                         spawns see the change. Position isn't saved; a \
                         spawned copy lands wherever you spawn it, at full \
                         health.",
                    )
                    .clicked()
                {
                    *save_prefab_request = Some((*id, new_prefab_name.trim().to_string()));
                    new_prefab_name.clear();
                }
            });
        }
        None => {
            ui.weak("No entity selected");
            ui.weak("Click a cube, or pick one in Scene.");
        }
    }
}

/// Script Editor tab: a script name list above one code editor. Laid out
/// vertically (rather than a left sidebar) so it reads well in a docked column.
fn scripts_tab_ui(
    ui: &mut egui::Ui,
    script_library: &mut std::collections::BTreeMap<String, String>,
    new_script_name: &mut String,
    open_script: &mut Option<String>,
    script_status: &Option<Result<(), script::ScriptError>>,
    script_warnings: &[script::ScriptError],
    // In-progress rename text buffer for the open script. `None` means not
    // currently renaming; `Some((original_name, buffer))` tracks which script
    // it belongs to, so navigating to a different script clears stale state
    // instead of showing an old buffer for the wrong name.
    renaming: &mut Option<(String, String)>,
    // Set once, the frame a rename actually completes, so the caller (which has
    // access to the world, this function doesn't) can rewrite every entity's
    // Script.uses that pointed at the old name. Read and cleared by the caller.
    renamed: &mut Option<(String, String)>,
) {
    let mut delete: Option<String> = None;
    // LEFT: the script list and the "new script" box, in a resizable sidebar.
    egui::Panel::left("script_list")
        .resizable(true)
        .default_size(200.0)
        .show(ui, |ui| {
            ui.label("Scripts");
            ui.separator();
            ui.add(
                egui::TextEdit::singleline(new_script_name)
                    .hint_text("new script name")
                    .desired_width(f32::INFINITY),
            );
            let key = new_script_name.trim().to_string();
            // Scripts are saved as files named after them, so the name has
            // to work as one (see `project::script_name_problem`).
            let problem = (!key.is_empty())
                .then(|| frame_engine::project::script_name_problem(&key))
                .flatten();
            if let Some(problem) = problem {
                ui.colored_label(egui::Color32::from_rgb(0xe0, 0x6c, 0x6c), problem);
            }
            if ui
                .add_enabled(
                    !key.is_empty() && problem.is_none(),
                    egui::Button::new("Add"),
                )
                .clicked()
            {
                {
                    script_library.entry(key.clone()).or_default();
                    *open_script = Some(key);
                    new_script_name.clear();
                }
            }
            ui.separator();
            egui::ScrollArea::vertical()
                .id_salt("script_name_list")
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    if script_library.is_empty() {
                        ui.weak("No scripts yet.");
                    }
                    for name in script_library.keys() {
                        ui.selectable_value(open_script, Some(name.clone()), name.as_str());
                    }
                });
        });
    // RIGHT: the editor for the open script fills the space the sidebar leaves.
    let open = open_script
        .as_ref()
        .filter(|n| script_library.contains_key(*n))
        .cloned();
    match open {
        Some(name) => {
            // Stale rename state (started on a different script, or the user
            // navigated away mid-rename) is cleared rather than shown for the
            // wrong entry.
            if renaming.as_ref().is_some_and(|(orig, _)| orig != &name) {
                *renaming = None;
            }
            ui.horizontal(|ui| {
                match renaming {
                    Some((_, buffer)) => {
                        ui.add(egui::TextEdit::singleline(buffer).desired_width(160.0));
                        let new_name = buffer.trim().to_string();
                        // Same name (or empty) just cancels quietly; a name
                        // that collides with a DIFFERENT existing script is
                        // refused rather than silently overwriting it.
                        let collides = !new_name.is_empty()
                            && new_name != name
                            && script_library.contains_key(&new_name);
                        let problem = (!new_name.is_empty())
                            .then(|| frame_engine::project::script_name_problem(&new_name))
                            .flatten();
                        if ui
                            .add_enabled(
                                !new_name.is_empty() && !collides && problem.is_none(),
                                egui::Button::new("Confirm"),
                            )
                            .clicked()
                        {
                            if new_name != name {
                                if let Some(source) = script_library.remove(&name) {
                                    script_library.insert(new_name.clone(), source);
                                    *open_script = Some(new_name.clone());
                                    *renamed = Some((name.clone(), new_name));
                                }
                            }
                            *renaming = None;
                        }
                        if ui.button("Cancel").clicked() {
                            *renaming = None;
                        }
                        if collides {
                            ui.colored_label(
                                egui::Color32::from_rgb(0xe0, 0x6c, 0x6c),
                                "already used",
                            );
                        } else if let Some(problem) = problem {
                            ui.colored_label(egui::Color32::from_rgb(0xe0, 0x6c, 0x6c), problem);
                        }
                    }
                    None => {
                        ui.strong(&name);
                        if ui.small_button("Rename").clicked() {
                            *renaming = Some((name.clone(), name.clone()));
                        }
                        if ui.small_button("Delete").clicked() {
                            delete = Some(name.clone());
                        }
                    }
                }
            });
            match script_status {
                Some(Ok(())) if script_warnings.is_empty() => {
                    ui.colored_label(egui::Color32::from_rgb(0x7c, 0xc5, 0x7c), "No problems");
                }
                Some(Ok(())) => {
                    // Parses, but uses names the script API doesn't define — the
                    // script will run and silently do nothing at those lines.
                    for w in script_warnings {
                        let loc = match (w.line, w.column) {
                            (Some(l), Some(c)) => format!("line {l}, col {c}: "),
                            (Some(l), None) => format!("line {l}: "),
                            _ => String::new(),
                        };
                        ui.colored_label(
                            egui::Color32::from_rgb(0xd6, 0xa8, 0x4c),
                            format!("{loc}{}", w.message),
                        );
                    }
                }
                Some(Err(e)) => {
                    let loc = match (e.line, e.column) {
                        (Some(l), Some(c)) => format!("line {l}, col {c}: "),
                        (Some(l), None) => format!("line {l}: "),
                        _ => String::new(),
                    };
                    ui.colored_label(
                        egui::Color32::from_rgb(0xe0, 0x6c, 0x6c),
                        format!("{loc}{}", e.message),
                    );
                }
                None => {}
            }
            if let Some(source) = script_library.get_mut(&name) {
                let row_h = ui.text_style_height(&egui::TextStyle::Monospace);
                let rows = ((ui.available_height() / row_h).floor() - 1.0).max(3.0) as usize;
                let line_count = source.matches('\n').count() + 1;
                let digits = line_count.to_string().len();
                let gutter: String = (1..=line_count)
                    .map(|n| format!("{n:>digits$}"))
                    .collect::<Vec<_>>()
                    .join("\n");
                egui::ScrollArea::vertical()
                    .id_salt("script_editor")
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.horizontal_top(|ui| {
                            ui.vertical(|ui| {
                                ui.add_space(2.0);
                                ui.add(egui::Label::new(
                                    egui::RichText::new(&gutter).monospace().weak(),
                                ));
                            });
                            ui.add(
                                egui::TextEdit::multiline(source)
                                    .code_editor()
                                    .desired_rows(rows)
                                    .desired_width(f32::INFINITY),
                            );
                        });
                    });
            }
        }
        None => {
            ui.weak("Select a script on the left, or add one to begin.");
        }
    }
    if let Some(name) = delete {
        script_library.remove(&name);
        if open_script.as_ref() == Some(&name) {
            *open_script = None;
        }
        if renaming.as_ref().is_some_and(|(orig, _)| orig == &name) {
            *renaming = None;
        }
    }
}

/// Owns the transient per-frame state the dockable tabs read and write, so
/// egui_dock's `TabViewer::ui` can reach it. Built fresh from the world each
/// frame and drained back into the world afterwards.
struct EditorTabViewer {
    entity_ids: Vec<usize>,
    // Each entity's Parent.entity, for the Scene tab's tree view. Built
    // alongside entity_ids from the same world snapshot; see scene_tab_ui.
    entity_parents: std::collections::HashMap<usize, usize>,
    // Set by the Scene tab when a drag-and-drop reparent is dropped this
    // frame: (dragged entity, new parent). None on every other frame,
    // including any frame the Scene tab isn't visible at all — same
    // reset-each-frame shape as viewport_rect below. Applied by the caller
    // after the pass, via App::reparent_entity.
    scene_reparent_request: Option<(usize, usize)>,
    selection: Option<usize>,
    edited: Option<EditedEntity>,
    // Plugin-declared custom fields for the selected entity. See
    // EditableCustomField.
    custom_fields: Vec<EditableCustomField>,
    script_library: std::collections::BTreeMap<String, String>,
    new_script_name: String,
    script_filter: String,
    open_script: Option<String>,
    // In-progress script rename, and the completed-this-frame signal for the
    // caller to rewrite entity references. See scripts_tab_ui.
    renaming: Option<(String, String)>,
    renamed: Option<(String, String)>,
    script_status: Option<Result<(), script::ScriptError>>,
    script_warnings: Vec<script::ScriptError>,
    // Names of the project's imported models, for the mesh picker.
    custom_mesh_names: Vec<String>,
    // The open project's assets folder, for the Sound picker.
    assets_root: Option<std::path::PathBuf>,
    // Read-only git snapshot for the Source Control tab (owned for the frame,
    // handed back to the App afterwards).
    git_summary: Option<GitSummary>,
    // Set by the Viewport tab each frame to its transparent body rect (egui
    // points). `None` when the Viewport tab isn't visible. Used to route 3D
    // input: clicks/drags land on the viewport only when the cursor is here.
    viewport_rect: Option<egui::Rect>,
    // The translate gizmo to paint over the viewport, if anything is selected.
    gizmo: Option<GizmoDraw>,
    // The performance overlay's lines, when it is shown.
    stats: Option<Vec<String>>,
    // Set by context_menu when the user picks "Open in new window" on a tab;
    // the caller pops it out (see App::pop_out_tab) after the egui pass, the
    // same lift-then-write-back pattern menu_action uses.
    pop_out_request: Option<Tab>,
    // The main window's GpuState.preview_texture_id, for the Inspector's
    // Camera preview to draw. `None` before the window/GPU exist yet (the
    // launcher screen), in which case the Inspector just doesn't show one.
    preview_texture_id: Option<egui::TextureId>,
    // The Inspector's "Save as Prefab" name field, persisted across frames
    // like new_script_name is.
    new_prefab_name: String,
    // Set when "Save as Prefab" is clicked: the entity id captured this same
    // frame (not re-read from selection afterward — see EditableCustomField's
    // own doc comment for why that distinction matters) and the name typed
    // in. Applied by the caller after the pass.
    save_prefab_request: Option<(usize, String)>,
    // Editor modules, for their Inspector sections.
    modules: Rc<RefCell<Vec<Box<dyn EditorModule>>>>,
}

impl egui_dock::TabViewer for EditorTabViewer {
    type Tab = Tab;

    fn title(&mut self, tab: &mut Self::Tab) -> egui::WidgetText {
        match tab {
            Tab::Viewport => "Viewport",
            Tab::Scene => "Scene",
            Tab::Inspector => "Inspector",
            Tab::Scripts => "Script Editor",
            Tab::Source => "Source Control",
        }
        .into()
    }

    fn ui(&mut self, ui: &mut egui::Ui, tab: &mut Self::Tab) {
        match tab {
            // The viewport draws nothing itself — the 3D scene is rendered behind
            // egui and shows through this tab's transparent body. We only record
            // the body rect so window_event can route 3D input to it.
            Tab::Viewport => {
                self.viewport_rect = Some(ui.max_rect());
                // The translate gizmo is painted here, on top of the 3D showing
                // through, rather than as scene geometry — it's a tool, not part of
                // the world, and this keeps it out of the render pipeline entirely.
                if let Some(g) = self.gizmo {
                    let painter = ui.painter();
                    // X red, Y green, Z blue — the usual convention.
                    let colors = [
                        egui::Color32::from_rgb(0xe0, 0x5c, 0x5c),
                        egui::Color32::from_rgb(0x6c, 0xc9, 0x6c),
                        egui::Color32::from_rgb(0x5c, 0x8c, 0xe0),
                    ];
                    for axis in 0..3 {
                        let lit = g.active == Some(axis);
                        let color = if lit {
                            egui::Color32::from_rgb(0xff, 0xd5, 0x4c) // grabbed/hovered
                        } else {
                            colors[axis]
                        };
                        let width = if lit { 3.5 } else { 2.0 };
                        painter.line_segment(
                            [g.origin, g.ends[axis]],
                            egui::Stroke::new(width, color),
                        );
                        painter.circle_filled(g.ends[axis], if lit { 6.0 } else { 4.5 }, color);
                    }
                }
                if let Some(lines) = &self.stats {
                    paint_stats(ui, lines);
                }
            }
            Tab::Scene => {
                self.scene_reparent_request = scene_tab_ui(
                    ui,
                    &self.entity_ids,
                    &self.entity_parents,
                    &mut self.selection,
                );
            }
            Tab::Inspector => inspector_tab_ui(
                ui,
                &mut self.edited,
                &self.script_library,
                &mut self.script_filter,
                &self.custom_mesh_names,
                &mut self.custom_fields,
                self.assets_root.as_deref(),
                self.preview_texture_id,
                &mut self.new_prefab_name,
                &mut self.save_prefab_request,
                &self.modules,
            ),
            Tab::Scripts => scripts_tab_ui(
                ui,
                &mut self.script_library,
                &mut self.new_script_name,
                &mut self.open_script,
                &self.script_status,
                &self.script_warnings,
                &mut self.renaming,
                &mut self.renamed,
            ),
            Tab::Source => source_tab_ui(ui, &self.git_summary),
        }
    }

    /// These tool panels are always present; don't offer a close button.
    fn is_closeable(&self, _tab: &Self::Tab) -> bool {
        false
    }

    /// Right-click a tab for the option to pop it into its own real, separate
    /// OS window (draggable to another monitor). Not offered on the Viewport
    /// tab -- it's the live 3D scene rendered behind egui, not a panel, and
    /// popping it out would need its own duplicated render target.
    fn context_menu(&mut self, ui: &mut egui::Ui, tab: &mut Self::Tab, _path: egui_dock::NodePath) {
        if matches!(tab, Tab::Viewport) {
            return;
        }
        if ui.button("Open in new window").clicked() {
            self.pop_out_request = Some(*tab);
            ui.close();
        }
    }

    /// Leave the Viewport tab's body unpainted so the 3D scene behind egui shows
    /// through it. Every other tab clears its background normally.
    fn clear_background(&self, tab: &Self::Tab) -> bool {
        !matches!(tab, Tab::Viewport)
    }

    /// No scroll bars over the viewport — it's a fixed window onto the 3D scene.
    fn scroll_bars(&self, tab: &Self::Tab) -> [bool; 2] {
        match tab {
            Tab::Viewport => [false, false],
            _ => [true, true],
        }
    }
}

mod theme {
    use egui::Color32;

    pub const BG: Color32 = Color32::from_rgb(0x15, 0x16, 0x15);
    pub const PANEL: Color32 = Color32::from_rgb(0x1B, 0x1C, 0x1B);
    pub const INPUT: Color32 = Color32::from_rgb(0x25, 0x27, 0x25);
    pub const BORDER: Color32 = Color32::from_rgb(0x3A, 0x3D, 0x3A);
    pub const TEXT: Color32 = Color32::from_rgb(0xE4, 0xE6, 0xE3);
    pub const MUTED: Color32 = Color32::from_rgb(0x8E, 0x93, 0x8D);
    pub const ACCENT: Color32 = Color32::from_rgb(0x58, 0x84, 0x4C);
    pub const ACCENT_HOVER: Color32 = Color32::from_rgb(0x6B, 0x9A, 0x5E);
    pub const ON_ACCENT: Color32 = Color32::from_rgb(0x0E, 0x17, 0x10);
    // The wood-brown secondary accent from the mockup. Not used by the
    // theme below yet -- it was the mockup's tick-mark/marker color, which
    // needs actual icon/marker work to carry over, not just a Visuals field.
    pub const WOOD: Color32 = Color32::from_rgb(0xB9, 0x8A, 0x4E);
}

/// The editor's visual theme (Outerface V1: green accent, small rounding,
/// carved-adjacent widget states). Applied to every egui::Context the
/// editor's own chrome uses -- the main window and each popped-out tab
/// window -- but deliberately not the Play window, which is the player's
/// own UI, not editor chrome.
fn apply_editor_theme(ctx: &egui::Context) {
    use egui::Color32;

    let mut visuals = egui::Visuals::dark();

    visuals.override_text_color = Some(theme::TEXT);
    visuals.panel_fill = theme::PANEL;
    visuals.window_fill = theme::PANEL;
    visuals.extreme_bg_color = theme::INPUT;
    visuals.faint_bg_color = theme::INPUT;
    visuals.code_bg_color = theme::INPUT;
    visuals.hyperlink_color = theme::ACCENT;
    visuals.window_stroke = egui::Stroke::new(1.0, theme::BORDER);
    visuals.window_corner_radius = egui::CornerRadius::same(4);
    visuals.menu_corner_radius = egui::CornerRadius::same(4);

    visuals.selection.bg_fill = Color32::from_rgba_unmultiplied(0x58, 0x84, 0x4C, 90);
    visuals.selection.stroke = egui::Stroke::new(1.0, theme::ACCENT);

    // Sliders don't paint a filled rail at all unless this is on -- without
    // it, a Slider looks identical to stock egui no matter what accent color
    // is set, since the fill (not just the handle) is what reads as "green"
    // in the mockup. It's drawn with `selection.bg_fill` above.
    visuals.slider_trailing_fill = true;

    visuals.widgets.noninteractive.bg_fill = theme::PANEL;
    visuals.widgets.noninteractive.bg_stroke = egui::Stroke::new(1.0, theme::BORDER);
    visuals.widgets.noninteractive.fg_stroke = egui::Stroke::new(1.0, theme::MUTED);

    visuals.widgets.inactive.bg_fill = theme::INPUT;
    visuals.widgets.inactive.weak_bg_fill = theme::INPUT;
    visuals.widgets.inactive.bg_stroke = egui::Stroke::new(1.0, theme::BORDER);
    visuals.widgets.inactive.fg_stroke = egui::Stroke::new(1.0, theme::TEXT);
    visuals.widgets.inactive.corner_radius = egui::CornerRadius::same(4);

    visuals.widgets.hovered.bg_fill = theme::INPUT;
    visuals.widgets.hovered.weak_bg_fill = theme::INPUT;
    visuals.widgets.hovered.bg_stroke = egui::Stroke::new(1.0, theme::ACCENT_HOVER);
    visuals.widgets.hovered.fg_stroke = egui::Stroke::new(1.0, theme::ACCENT_HOVER);
    visuals.widgets.hovered.corner_radius = egui::CornerRadius::same(4);

    visuals.widgets.active.bg_fill = theme::ACCENT;
    visuals.widgets.active.weak_bg_fill = theme::ACCENT;
    visuals.widgets.active.bg_stroke = egui::Stroke::new(1.0, theme::ACCENT_HOVER);
    // Must stay light: egui's strong_text_color() (headings) reads this, so a
    // dark value made headings vanish on dark panels.
    visuals.widgets.active.fg_stroke = egui::Stroke::new(1.0, Color32::WHITE);
    visuals.widgets.active.corner_radius = egui::CornerRadius::same(4);

    visuals.widgets.open.bg_fill = theme::INPUT;
    visuals.widgets.open.weak_bg_fill = theme::INPUT;
    visuals.widgets.open.bg_stroke = egui::Stroke::new(1.0, theme::ACCENT);
    visuals.widgets.open.fg_stroke = egui::Stroke::new(1.0, theme::TEXT);
    visuals.widgets.open.corner_radius = egui::CornerRadius::same(4);

    ctx.set_visuals(visuals);

    // Outerface V1 typography: Manrope for body text, JetBrains Mono for monospace,
    // Fraunces (serif) as its own named family used for headings. Each is put
    // first in its family so egui's built-in fonts stay behind as fallbacks
    // for any glyph these don't cover.
    let mut fonts = egui::FontDefinitions::default();
    fonts.font_data.insert(
        "Manrope".to_owned(),
        std::sync::Arc::new(egui::FontData::from_static(include_bytes!(
            "../assets/fonts/Manrope-Regular.ttf"
        ))),
    );
    fonts.font_data.insert(
        "JetBrainsMono".to_owned(),
        std::sync::Arc::new(egui::FontData::from_static(include_bytes!(
            "../assets/fonts/JetBrainsMono-Medium.ttf"
        ))),
    );
    fonts.font_data.insert(
        "Fraunces".to_owned(),
        std::sync::Arc::new(egui::FontData::from_static(include_bytes!(
            "../assets/fonts/Fraunces.ttf"
        ))),
    );
    if let Some(family) = fonts.families.get_mut(&egui::FontFamily::Proportional) {
        family.insert(0, "Manrope".to_owned());
    }
    if let Some(family) = fonts.families.get_mut(&egui::FontFamily::Monospace) {
        family.insert(0, "JetBrainsMono".to_owned());
    }
    let serif = egui::FontFamily::Name("fraunces".into());
    fonts.families.insert(
        serif.clone(),
        vec!["Fraunces".to_owned(), "Manrope".to_owned()],
    );
    ctx.set_fonts(fonts);

    // Context::style()/set_style() don't exist in this egui version -- style
    // is now split per Theme (dark/light). all_styles_mut sets spacing on
    // both, which is fine here since the editor always runs in dark mode.
    ctx.all_styles_mut(|style| {
        style.text_styles.insert(
            egui::TextStyle::Heading,
            egui::FontId::new(18.0, serif.clone()),
        );
        style.spacing.item_spacing = egui::vec2(10.0, 8.0);
        style.spacing.button_padding = egui::vec2(14.0, 7.0);
    });
}

/// The Credits section of the Editor Settings window: who made Frame Engine,
/// the embedded fonts and their licence, the community links, and the
/// open-source libraries the editor is built on. Plain data and labels, so
/// adding a credit later is one more line here.
fn credits_ui(ui: &mut egui::Ui) {
    ui.heading("Credits");
    ui.add_space(4.0);

    ui.label("Frame Engine and Frame Editor");
    ui.weak(format!(
        "Version {} (pre-release), MPL-2.0 licence",
        frame_engine::version::ENGINE
    ));
    ui.add_space(6.0);

    section_label(ui, "Made by");
    ui.label("Outer Frame Interactive");
    ui.weak("Designed and built by Luke Fawcett, in the open, as a learning project.");
    ui.add_space(6.0);

    section_label(ui, "Theme");
    ui.label("Outerface V1");
    ui.weak("The editor's own look: neutral dark panels, a green accent and a wood tone.");
    ui.add_space(6.0);

    section_label(ui, "Fonts (SIL Open Font License 1.1)");
    ui.label("Manrope, by Mikhail Sharanda");
    ui.label("Fraunces, by Undercase Type");
    ui.label("JetBrains Mono, by JetBrains");
    ui.add_space(6.0);

    section_label(ui, "Community");
    ui.hyperlink_to(
        "github.com/outerframehq/frame-engine",
        "https://github.com/outerframehq/frame-engine",
    );
    ui.hyperlink_to("@OuterFrameInter on X", "https://x.com/OuterFrameInter");
    ui.hyperlink_to("Discord server", "https://discord.gg/ufXDNJnzgf");
    ui.add_space(6.0);

    egui::CollapsingHeader::new("Open-source libraries").show(ui, |ui| {
        ui.weak("Hand-rolled at the heart, bought for the rest:");
        for (name, what) in [
            ("winit", "windowing"),
            ("wgpu", "GPU rendering"),
            ("glam", "linear algebra"),
            ("egui, egui_dock", "the editor's panel UI"),
            ("rapier3d", "rigid-body physics"),
            ("rhai", "entity scripting"),
            ("kira", "audio playback"),
            ("serde, ron", "scene and project files"),
            ("git2", "the Source Control tab"),
            ("rfd", "native file dialogs"),
            ("image", "texture and logo decoding"),
            ("dirs, chrono", "config paths and dates"),
            ("bytemuck, pollster", "GPU plumbing"),
        ] {
            ui.horizontal(|ui| {
                ui.monospace(name);
                ui.weak(what);
            });
        }
    });
}

/// An Inspector section heading: small serif (Fraunces) in the muted color, so
/// "Position", "Velocity" and the rest read as headings above their controls
/// instead of looking like every other line of text.
pub fn section_label(ui: &mut egui::Ui, text: &str) {
    ui.label(
        egui::RichText::new(text)
            .family(egui::FontFamily::Name("fraunces".into()))
            .size(13.0)
            .color(theme::MUTED),
    );
}

/// The Outerface V1 checkbox: a small square box with an accent border and a diamond
/// tick when checked, the mockup's look. Drop-in for `ui.checkbox`: takes the
/// same `&mut bool` and label, and the returned response reports `changed()`
/// and takes `on_hover_text` the same way. Clicking the label toggles it too.
pub fn outerface_checkbox(
    ui: &mut egui::Ui,
    checked: &mut bool,
    text: impl Into<egui::WidgetText>,
) -> egui::Response {
    let text = text.into();
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 7.0;
        let (box_rect, box_response) =
            ui.allocate_exact_size(egui::vec2(15.0, 15.0), egui::Sense::click());
        let label_response = ui.add(
            egui::Label::new(text)
                .selectable(false)
                .sense(egui::Sense::click()),
        );
        let mut response = box_response.union(label_response);
        if response.clicked() {
            *checked = !*checked;
            response.mark_changed();
        }

        let border = if *checked {
            theme::ACCENT
        } else if response.hovered() {
            theme::ACCENT_HOVER
        } else {
            theme::BORDER
        };
        ui.painter().rect(
            box_rect,
            egui::CornerRadius::same(3),
            theme::INPUT,
            egui::Stroke::new(1.0, border),
            egui::StrokeKind::Inside,
        );
        if *checked {
            let c = box_rect.center();
            let r = 4.0;
            ui.painter().add(egui::Shape::convex_polygon(
                vec![
                    c + egui::vec2(0.0, -r),
                    c + egui::vec2(r, 0.0),
                    c + egui::vec2(0.0, r),
                    c + egui::vec2(-r, 0.0),
                ],
                theme::ACCENT,
                egui::Stroke::NONE,
            ));
        }
        response
    })
    .inner
}

/// The dock's tab chrome in the Outerface V1 theme: the active tab sits on the panel
/// color with a wood-brown outline (the mockup's selected-tab look), inactive
/// tabs are muted with no outline, and hovering warms them to the green accent.
fn dock_style(base: &egui::Style) -> egui_dock::Style {
    let mut style = egui_dock::Style::from_egui(base);

    style.main_surface_border_stroke = egui::Stroke::new(1.0, theme::BORDER);

    style.tab_bar.bg_fill = theme::BG;
    style.tab_bar.hline_color = theme::BORDER;

    for tab in [
        &mut style.tab.active,
        &mut style.tab.focused,
        &mut style.tab.active_with_kb_focus,
        &mut style.tab.focused_with_kb_focus,
    ] {
        tab.bg_fill = theme::PANEL;
        tab.outline_color = theme::WOOD;
        tab.text_color = theme::TEXT;
        tab.corner_radius = egui::CornerRadius::same(4);
    }
    for tab in [
        &mut style.tab.inactive,
        &mut style.tab.inactive_with_kb_focus,
    ] {
        tab.bg_fill = theme::BG;
        tab.outline_color = egui::Color32::TRANSPARENT;
        tab.text_color = theme::MUTED;
        tab.corner_radius = egui::CornerRadius::same(4);
    }
    style.tab.hovered.bg_fill = theme::INPUT;
    style.tab.hovered.outline_color = theme::BORDER;
    style.tab.hovered.text_color = theme::ACCENT_HOVER;
    style.tab.hovered.corner_radius = egui::CornerRadius::same(4);

    style
}

/// A dock tab the user has popped out into its own real, separate OS window
/// (see App::pop_out_tab). Mirrors the same real-second-window shape
/// game_window/game_gpu already use for Play mode: its own window, its own
/// GPU surface, and -- unlike the game window, which draws no egui -- its
/// own small egui Context and input state, since this window's whole job is
/// drawing one egui panel.
struct PoppedOutWindow {
    // Field order matters here: Rust drops struct fields top-to-bottom, and
    // `gpu`'s wgpu surface holds a handle into `window`, so `gpu` must be
    // declared (and therefore dropped) first -- the same ordering
    // close_game's own doc comment spells out for the game window. Getting
    // this backwards is what caused the dock-back segfault.
    gpu: GpuState,
    window: Arc<Window>,
    egui_ctx: egui::Context,
    egui_state: egui_winit::State,
}

struct App {
    script_runtime: script::RhaiRuntime,
    window: Option<Arc<Window>>,
    gpu: Option<GpuState>,
    // The one Instance/Adapter/Device/Queue for the whole process, set the
    // first time a GpuState is created (the main window, in `resumed`) and
    // reused by every later window's GpuState -- see SharedGpu's own doc
    // comment for why this matters.
    gpu_shared: Option<SharedGpu>,
    // Launcher screen vs the editor proper.
    mode: AppMode,
    // Name of the open project (its folder name), shown in the window title.
    project_name: Option<String>,
    // Remembered projects, most-recently-edited first, shown on the launcher.
    // Refreshed when the launcher is (re)entered and on open/create.
    recent_projects: Vec<RecentProject>,
    // The name typed on the launcher for a new project (becomes its scene file).
    new_project_name: String,
    // Project-settings window state (opened from a card's Settings button).
    settings_open: bool,
    // A project the user clicked Delete on, awaiting Confirm/Cancel on its card.
    pending_delete: Option<std::path::PathBuf>,
    settings_root: Option<std::path::PathBuf>,
    settings_orig_name: String,
    settings_name: String,
    settings_description: String,
    settings_version: String,
    world: World,
    // rapier3d state for `world`'s `RigidBody`-marked entities, kept
    // alongside it the same way `script_runtime` is kept alongside rather
    // than folded into `World` itself (see `physics::Physics`'s own doc
    // comment for why).
    physics: Physics,
    // Undo/redo: full-world snapshots. Pushed before a mutating gesture; undo
    // pops to the redo stack and back. `inspector_editing` coalesces a drag into
    // one snapshot. `ctrl_held`/`shift_held` track modifiers for the shortcuts.
    undo_stack: Vec<World>,
    redo_stack: Vec<World>,
    inspector_editing: bool,
    ctrl_held: bool,
    shift_held: bool,
    // Where "Save scene" writes and "Reload scene" reads. Set by Open/Save-As
    // (and defaulted to the startup scene). None means Save prompts for a path.
    current_scene_path: Option<std::path::PathBuf>,
    // The open project's folder. Scenes live in its `scenes/` folder, so the
    // project is no longer simply the folder the scene file is in.
    project_dir: Option<std::path::PathBuf>,
    // Imported models for the open project, parsed once and kept CPU side so
    // they can be uploaded to any GPU device (editor window and game window).
    // BTreeMap so the name order is stable and sorted.
    custom_meshes: std::collections::BTreeMap<String, frame_engine::assets::MeshData>,
    // Decoded material textures, keyed by file name, shared with build_material_gpu_data
    // the same way sound_cache caches decoded audio: so editing a mesh's
    // roughness/metalness in the Assets tab (which rebuilds every material
    // bind group) never re-reads or re-decodes a texture file that hasn't
    // itself changed.
    texture_cache: std::collections::HashMap<String, (u32, u32, Vec<u8>)>,
    // Which mesh's material panel is open in the Assets tab, if any. None
    // when nothing's expanded, or when the named mesh no longer exists.
    material_editing: Option<String>,
    // Saved prefabs for the open project, scanned from its prefabs/ folder
    // the same way custom_meshes is scanned from assets/. Unlinked stamps
    // (see Prefab's own doc comment): this map is only ever read to spawn a
    // fresh copy, never mutated to propagate a change to existing entities.
    prefabs: std::collections::BTreeMap<String, frame_engine::world::Prefab>,
    // The Inspector's "Save as Prefab" name field, persisted across frames
    // like new_script_name is.
    new_prefab_name: String,
    // Names of the game window's uploaded models, same sorted order.
    game_custom_names: Vec<String>,
    // Assets tab browser state: the open subfolder inside assets/ and the
    // pending new folder name.
    assets_subdir: std::path::PathBuf,
    new_asset_folder: String,
    // Rendered previews for the Assets tab, keyed by model name.
    thumbnails: std::collections::HashMap<String, egui::TextureHandle>,
    // A file marked for moving, pasted into whichever folder is open.
    move_pending: Option<std::path::PathBuf>,
    // Cached git state for the Source Control tab, refreshed on a timer rather
    // than every frame (statuses() walks the working tree). None = not inside a
    // git repository, or no project open.
    git_summary: Option<GitSummary>,
    git_refresh_at: std::time::Instant,
    paused: bool,
    clock: Clock,
    cam_focus_x: f64,
    cam_focus_y: f64,
    cam_distance: f32,
    cam_yaw: f32,
    cam_pitch: f32,
    dragging: bool,
    orbiting: bool,
    // Shift+Middle-drag pans the camera; plain Middle-drag orbits (see above).
    panning: bool,
    // Flythrough WASD speed, in world units/sec. Starts at CAM_PAN_SPEED and is
    // adjusted with the scroll wheel while flying (right mouse button held);
    // persists at whatever value it was last set to across flights.
    fly_speed: f32,
    // Mouse sensitivities, as multipliers on LOOK_SENS / ORBIT_SENS, and the
    // look-direction inversions. All persisted in editor.ron.
    look_sensitivity: f32,
    orbit_sensitivity: f32,
    invert_look_x: bool,
    invert_look_y: bool,
    drag_hold_seconds: f32,
    // Translate gizmo state. `gizmo` is recomputed each frame from the selection
    // (None when nothing is selected). `gizmo_drag` is the axis currently being
    // dragged, `gizmo_hover` the one under the cursor — 0 = X, 1 = Y, 2 = Z.
    gizmo: Option<GizmoScreen>,
    gizmo_drag: Option<usize>,
    gizmo_hover: Option<usize>,
    last_cursor: (f64, f64),
    selected: Option<usize>,
    show_help: bool,
    // Whether the performance overlay shows (a saved preference).
    show_stats: bool,
    // Frame timing for the performance overlay.
    perf: perf::FrameStats,
    // Whether the Play window shows its own overlay (F3 in Play), and the
    // Play window's frame timing. Off each time Play starts.
    show_play_stats: bool,
    play_perf: perf::FrameStats,
    // Whether the modules' Play panels are open (F1 in Play; see
    // `EditorModule::play_panel`). Closed each time Play starts.
    play_panels_open: bool,
    // Why the editor is drawing every frame right now, or None when it only
    // draws on change (shown in the performance overlay).
    redraw_reason: Option<String>,
    // When an idle editor next draws on its own.
    idle_wake: Option<std::time::Instant>,
    // When the water's waves last moved on (see `advance_waves`).
    wave_clock: Option<std::time::Instant>,
    // Where in the code the UI last asked to draw again straight away (from
    // egui's own record), for the overlay.
    egui_cause: Option<String>,
    // Unsaved-changes tracking. `dirty` is set by any authoring edit (every
    // mutating gesture pushes an undo snapshot) and cleared on save/load; the
    // script library is compared against `saved_scripts` as well, since typing
    // in the Script Editor deliberately isn't an undo step. The running
    // simulation moving things doesn't count as an edit.
    dirty: bool,
    saved_scripts: std::collections::BTreeMap<String, String>,
    // An action waiting on the unsaved-changes prompt.
    pending_unsaved: Option<GuardedAction>,
    // The scene to open once the unsaved-changes prompt is answered.
    pending_scene_switch: Option<String>,
    // The Scenes tab's own fields (names being typed, delete confirmation).
    scenes_tab: scenes::ScenesTabState,
    // A warning about versions to show in a window until dismissed: a
    // scene, prefab or project from a newer Frame Editor. See
    // `version_notice_ui`.
    version_notice: Option<String>,
    // The project a `run_player` build plays: its folder or its pack.
    // `None` in the editor.
    player_files: Option<ProjectFiles>,
    // Set when a `run_player` build could not open its game, so the program
    // ends with an error code.
    player_failed: bool,
    // The title last given to the window, so it's only set when it changes.
    window_title: String,
    // The preferences as last written to editor.ron.
    saved_prefs: EditorPrefs,
    egui_ctx: egui::Context,
    egui_state: Option<egui_winit::State>,
    // The layout of the dockable panels (Viewport, Scene, Inspector, Scripts).
    // egui_dock owns the arrangement and which tab is active; we just persist it.
    dock_state: egui_dock::DockState<Tab>,
    // The Viewport tab's on-screen rect (egui points) from last frame, or None if
    // it wasn't visible. window_event routes 3D input by this instead of by
    // is_pointer_over_egui, which would read true over the viewport tab.
    viewport_rect: Option<egui::Rect>,
    // Flythrough camera: true while the right mouse button is held over the
    // viewport. The cursor is grabbed and hidden, mouse motion looks around, and
    // WASD flies. A focus Z lets the camera move in the full look direction.
    fly_mode: bool,
    cam_focus_z: f64,
    // Free-camera eye position, used only while flying. Seeded from the orbit eye
    // on entry; WASD moves it and mouselook turns it in place (no orbit pivot).
    cam_eye: DVec3,
    // Whether an entity was picked during this fly session; if so, orbit resumes
    // around it on exit.
    fly_picked: bool,
    // Active tab in the bottom console dock.
    console_tab: ConsoleTab,
    // Lines shown in the console Output tab (also echoed to the terminal).
    log_lines: Vec<String>,
    // Which movement buttons (WASD) are currently held, read by the input system.
    input: InputState,
    // A separate "clean" game window for Play: its own window + GPU surface,
    // running a copy of the project's world with no editor chrome. None when not
    // playing. game_input drives Controlled entities; game_clock ticks the sim.
    game_window: Option<Arc<Window>>,
    // Where the game being played reads its files: the project folder, or
    // a game build's pack. Sounds and on-screen images are found here while
    // playing, so Play works the same with or without a project open.
    game_files: Option<ProjectFiles>,
    game_gpu: Option<GpuState>,
    // Features added by other crates (see `module_api`).
    modules: Rc<RefCell<Vec<Box<dyn EditorModule>>>>,
    game_world: Option<World>,
    // The Play window's own physics state, separate from `physics` above the
    // same way `game_world` is separate from `world`. `None` whenever
    // `game_world` is, created fresh each time Play starts (see
    // `open_game`/`close_game`) rather than reused, so a previous play
    // session's simulated bodies never leak into the next one.
    game_physics: Option<Physics>,
    // The Play window's own egui context and input state, used only to draw
    // UiText/UiImage entities as a flat screen-space overlay (see
    // render_game_ui). Unlike PoppedOutWindow, there's no dock tab to show
    // here -- the whole egui pass is just the overlay widgets -- so there's
    // no viewer to borrow and no need for a dedicated struct. `None` exactly
    // when `game_window` is.
    game_egui_ctx: Option<egui::Context>,
    game_egui_state: Option<egui_winit::State>,
    // UI-image textures uploaded for the current Play session, keyed by file
    // name under assets/ the same way `thumbnails` is keyed by model name.
    // Cleared on close_game so a new Play session never shows a stale image
    // left over from a previous one (or from a different project).
    game_ui_textures: std::collections::HashMap<String, egui::TextureHandle>,
    game_input: InputState,
    game_clock: Clock,
    // Set when the game window asks to close; the actual teardown happens in
    // about_to_wait, outside event dispatch, so dropping the wgpu surface on
    // Wayland doesn't segfault mid-event.
    game_closing: bool,
    // Whether the Play window holds the mouse (after a click in it), so the
    // mouse turns the view. Esc lets it go.
    game_mouse_held: bool,
    // A game build's own menu (see `game_menu`), and whether it is showing.
    // While it shows, the world runs but takes no input from the player.
    game_menu: Option<Box<dyn GameMenu>>,
    menu_open: bool,
    // Play driven by something else (a multiplayer server, say), started
    // from the game's menu: see `GameSession`.
    game_session: Option<Box<dyn GameSession>>,
    // When the menu last opened, for the camera it moves.
    menu_opened: std::time::Instant,
    // The player's overall volume (0 to 1), and what the mixer was last set
    // to, so it is only told when it changes.
    game_volume: f32,
    applied_volume: Option<f32>,
    // The loops modules hold while a game plays (see game_audio), and when
    // the last frame's sound was worked out, for the listener's dt.
    game_audio: game_audio::GameAudio,
    game_audio_at: Option<std::time::Instant>,
    // Dock tabs currently popped out into their own window (see
    // PoppedOutWindow, App::pop_out_tab). The Viewport tab is never a key
    // here -- it's the live 3D scene, not an egui panel, so popping it out
    // needs its own separate render-target work, not yet done.
    popped_out: std::collections::HashMap<Tab, PoppedOutWindow>,
    // Tabs whose popped-out window asked to close (the OS close button, or
    // its own "Dock back" button) this cycle. Actually torn down in
    // about_to_wait, same Wayland-segfault-avoidance reasoning as
    // game_closing above.
    popped_out_closing: Vec<Tab>,
    // Text in the "new script name" box on the Scripts tab (persists between frames).
    new_script_name: String,
    // Filter text for the Inspector's script picker (persists between frames).
    script_filter: String,
    // Which library script is open in the Script Editor's centre pane (by name).
    open_script: Option<String>,
    // In-progress script rename (original name, edit buffer), persists between
    // frames the same way open_script does. See scripts_tab_ui.
    renaming: Option<(String, String)>,
    // Whether the Plugins slide-down panel is currently shown, toggled from
    // its toolbar tab. Persists between frames the same way console_tab does.
    plugins_panel_open: bool,
    // Whether the Editor Settings window is currently open, toggled from
    // Edit > Editor settings….
    editor_settings_open: bool,
    // File > Build Game: whether its window is open, the open project's
    // build choices (saved in project.ron when a build starts), the build
    // running now, its latest progress lines, and how the last one ended.
    build_window_open: bool,
    build_settings: build::BuildSettings,
    build_job: Option<build::BuildJob>,
    build_progress: Vec<String>,
    build_result: Option<Result<build::BuildOutput, build::BuildError>>,
    // GPU texture for the toolbar logo, uploaded once on the first frame.
    logo_texture: Option<egui::TextureHandle>,
    // The kira audio backend. None if the host's audio device couldn't be
    // opened (a headless CI box, say); Sound components still exist and
    // edit fine, they just never actually play without a manager.
    audio_manager: Option<kira::AudioManager<kira::DefaultBackend>>,
    // A low-pass filter on everything heard, dry (off) until Play's camera
    // is under a liquid, and whether it is on now.
    audio_muffle: Option<kira::effect::filter::FilterHandle>,
    muffled: bool,
    // Loaded sound files, keyed by their name as a Sound component names
    // them, so the same file is decoded once and every play() after that is
    // a cheap Arc-backed clone (see StaticSoundData's own docs), not a
    // re-read from disk every time a Sound fires.
    sound_cache: std::collections::HashMap<String, kira::sound::static_sound::StaticSoundData>,
}
impl App {
    /// Append a line to the in-editor log (shown in the console Output tab) and
    /// echo it to the terminal. The buffer is capped so it can't grow forever.
    fn log(&mut self, msg: impl Into<String>) {
        let msg = msg.into();
        println!("{msg}");
        self.log_lines.push(msg);
        if self.log_lines.len() > 500 {
            self.log_lines.remove(0);
        }
    }
    // --- Editor actions ---
    // One definition per action. The keyboard and the menus are just two
    // triggers that call these; the behaviour lives in exactly one place.
    /// Toggle the simulation between playing and paused.
    fn toggle_pause(&mut self) {
        self.paused = !self.paused;
        self.log(if self.paused { "Paused" } else { "Playing" });
    }
    /// Advance the simulation exactly one tick. Only meaningful while paused.
    fn step_once(&mut self) {
        if self.paused {
            module_api::tick_modules(&self.modules, &mut self.world, 1.0 / TICK_RATE as f32);
            systems::gravity(&mut self.world);
            systems::movement(&mut self.world);
            module_api::sync_collision(&self.modules, &self.world, &mut self.physics);
            self.physics.step(&mut self.world, 1.0 / TICK_RATE as f32);
            systems::resolve_collisions(&mut self.world);
            systems::apply_parenting(&mut self.world);
            self.log("Stepped one tick");
        }
    }
    /// Clear the current selection.
    fn clear_selection(&mut self) {
        self.selected = None;
        self.log("Selection cleared");
    }
    /// Toggle the on-screen controls overlay. The choice persists across runs.
    fn toggle_help(&mut self) {
        self.show_help = !self.show_help;
        self.persist_prefs_if_changed();
    }
    /// Toggle the performance overlay. The choice persists across runs.
    fn toggle_stats(&mut self) {
        self.show_stats = !self.show_stats;
        self.persist_prefs_if_changed();
    }
    /// The overlay's line about vsync: what is asked for, what the window
    /// really gets, and what the system offers.
    fn present_line(&self) -> Option<String> {
        let gpu = self.gpu.as_ref()?;
        let wanted = present_mode_for(vsync_enabled());
        let got = resolved_present_mode(wanted, &gpu.present_modes);
        let offered: Vec<String> = gpu.present_modes.iter().map(|m| format!("{m:?}")).collect();
        Some(format!(
            "vsync {} ({got:?}; offered {})",
            if vsync_enabled() { "on" } else { "off" },
            offered.join(", ")
        ))
    }
    /// Why the editor should draw the next frame straight away, or None when
    /// nothing is moving and it can wait for something to happen.
    fn continuous_reason(&self, egui_delay: std::time::Duration) -> Option<String> {
        use frame_engine::input::Button;
        if !self.paused {
            return Some("simulation running".to_string());
        }
        if self.fly_mode {
            return Some("flying".to_string());
        }
        let held = [
            Button::Up,
            Button::Down,
            Button::Left,
            Button::Right,
            Button::Jump,
        ]
        .iter()
        .any(|&b| self.input.is_held(b));
        if held {
            return Some("keys held".to_string());
        }
        if self.game_window.is_some() {
            return Some("Play window open".to_string());
        }
        if let Some(name) = module_api::busy_module(&self.modules) {
            return Some(format!("{name} working"));
        }
        if egui_delay.is_zero() {
            return Some(match &self.egui_cause {
                Some(cause) => format!("panels asked: {cause}"),
                None => "panels asked".to_string(),
            });
        }
        None
    }
    /// The performance overlay's lines: frame times and memory, then each
    /// module's own lines, for the editor's and (while Play runs) the Play
    /// window's GPU copies.
    fn stats_lines(&self) -> Vec<String> {
        let mut lines = self.perf.lines();
        lines.push(match &self.redraw_reason {
            Some(why) => format!("drawing every frame ({why})"),
            None => "drawing only on change".to_string(),
        });
        lines.extend(self.present_line());
        let entities = self.world.positions.iter().filter(|p| p.is_some()).count();
        lines.push(format!("entities {entities}"));
        lines.push(format!(
            "undo {} steps, redo {} (each a whole copy of the world)",
            self.undo_stack.len(),
            self.redo_stack.len()
        ));
        let textures: usize = self
            .texture_cache
            .values()
            .map(|(_, _, rgba)| rgba.len())
            .sum();
        let models: usize = self
            .custom_meshes
            .values()
            .map(|m| m.vertices.len() * std::mem::size_of::<frame_engine::assets::MeshVertexData>())
            .sum();
        let sounds: usize = self
            .sound_cache
            .values()
            .map(|s| s.frames.len() * std::mem::size_of::<kira::Frame>())
            .sum();
        lines.push(format!(
            "textures {} ({}), models {} ({}), sounds {} ({})",
            perf::megabytes(textures as u64),
            self.texture_cache.len(),
            perf::megabytes(models as u64),
            self.custom_meshes.len(),
            perf::megabytes(sounds as u64),
            self.sound_cache.len()
        ));
        if let Some(report) = self
            .gpu
            .as_ref()
            .and_then(|g| g.device.generate_allocator_report())
        {
            lines.push(format!(
                "GPU allocator {} in use, {} reserved",
                perf::megabytes(report.total_allocated_bytes),
                perf::megabytes(report.total_reserved_bytes)
            ));
        }
        lines.extend(module_api::module_stats(&self.modules));
        for (label, gpu) in [
            ("viewport", self.gpu.as_ref()),
            ("Play", self.game_gpu.as_ref()),
        ] {
            let Some(gpu) = gpu else { continue };
            let mut gpu_lines: Vec<String> =
                gpu.module_gpu.iter().flat_map(|m| m.stats()).collect();
            let blades = gpu.grass_drawn.get();
            if blades > 0 {
                gpu_lines.push(format!(
                    "grass: {blades} blades drawn ({})",
                    grass_quality().label()
                ));
            }
            if !gpu_lines.is_empty() {
                lines.push(format!("GPU, {label} window:"));
                lines.extend(gpu_lines.into_iter().map(|l| format!("  {l}")));
            }
        }
        lines
    }
    /// The preferences as the editor holds them right now.
    fn current_prefs(&self) -> EditorPrefs {
        EditorPrefs {
            show_help: self.show_help,
            show_stats: self.show_stats,
            vsync: vsync_enabled(),
            fly_speed: self.fly_speed,
            look_sensitivity: self.look_sensitivity,
            orbit_sensitivity: self.orbit_sensitivity,
            invert_look_x: self.invert_look_x,
            invert_look_y: self.invert_look_y,
            drag_hold_seconds: self.drag_hold_seconds,
            shadows: shadows_enabled(),
            shadow_distance: shadow_distance(),
            sky: sky_enabled(),
            fog_amount: fog_amount(),
            clouds: cloud_amount(),
            grass: grass_quality(),
            sun_time: sun::time_of_day_enabled(),
            time_of_day: sun::time_of_day_hours(),
        }
    }
    /// Write editor.ron, but only if something actually changed since the last
    /// write, so calling this freely (on leaving flythrough, say) is cheap.
    fn persist_prefs_if_changed(&mut self) {
        let now = self.current_prefs();
        if now != self.saved_prefs {
            save_prefs(&now);
            self.saved_prefs = now;
        }
    }
    /// Adopt preferences edited in the Editor Settings window and save them.
    fn apply_prefs(&mut self, prefs: EditorPrefs) {
        let prefs = prefs.sanitized();
        self.show_help = prefs.show_help;
        self.show_stats = prefs.show_stats;
        set_vsync(prefs.vsync);
        self.fly_speed = prefs.fly_speed;
        self.look_sensitivity = prefs.look_sensitivity;
        self.orbit_sensitivity = prefs.orbit_sensitivity;
        self.invert_look_x = prefs.invert_look_x;
        self.invert_look_y = prefs.invert_look_y;
        self.drag_hold_seconds = prefs.drag_hold_seconds;
        set_scene_tree_drag_hold_seconds(self.drag_hold_seconds);
        set_shadows_enabled(prefs.shadows);
        set_shadow_distance(prefs.shadow_distance);
        set_sky_enabled(prefs.sky);
        set_fog_amount(prefs.fog_amount);
        set_cloud_amount(prefs.clouds);
        set_grass_quality(prefs.grass);
        apply_sun_prefs(&prefs);
        self.persist_prefs_if_changed();
    }
    /// The open project's folder.
    fn project_root(&self) -> Option<std::path::PathBuf> {
        self.project_dir.as_deref().map(|p| p.to_path_buf())
    }

    /// Open File > Build Game with this project's saved build choices.
    fn open_build_window(&mut self) {
        let Some(root) = self.project_root() else {
            self.log("Open a project to build it".to_string());
            return;
        };
        if self.build_job.is_none() {
            self.build_settings = read_manifest(&root).build;
            self.build_result = None;
            self.build_progress.clear();
        }
        self.build_window_open = true;
    }

    /// What the Build window shows this frame, or `None` with no project or
    /// no game to build.
    fn build_view(&self) -> Option<build::BuildView> {
        let builder = build::game_builder()?;
        let root = self.project_root()?;
        let name = self
            .project_name
            .clone()
            .unwrap_or_else(|| "Game".to_string());
        let start_scene = project::start_scene_path(&ProjectFiles::Folder(root.clone()))
            .map(|p| {
                p.rsplit('/')
                    .next()
                    .unwrap_or("")
                    .trim_end_matches(".ron")
                    .to_string()
            })
            .unwrap_or_default();
        Some(build::BuildView {
            name,
            start_scene,
            version: read_manifest(&root).version,
            program: builder.program.clone(),
            can_encrypt: builder.cipher.is_some(),
            unsaved: self.is_dirty(),
            output_folder: self.build_settings.output_folder(&root),
            running: self.build_job.is_some(),
            progress: self.build_progress.clone(),
            result: self.build_result.clone(),
        })
    }

    /// Take in what a running build has reported.
    fn poll_build(&mut self) {
        let Some(job) = &self.build_job else {
            return;
        };
        let mut finished = None;
        for event in job.poll() {
            match event {
                build::BuildEvent::Progress(line) => {
                    self.build_progress.push(line);
                    if self.build_progress.len() > 20 {
                        self.build_progress.remove(0);
                    }
                }
                build::BuildEvent::Done(result) => finished = Some(result),
            }
        }
        if let Some(result) = finished {
            self.build_job = None;
            match &result {
                Ok(output) => {
                    for zip in &output.zips {
                        self.log(format!("Built {}", zip.display()));
                    }
                }
                Err(error) => self.log(format!("Build failed: {}", error.message)),
            }
            self.build_result = Some(result);
        }
    }

    fn handle_build_action(&mut self, action: build::BuildUiAction) {
        match action {
            build::BuildUiAction::Start => self.start_build(),
            build::BuildUiAction::SaveScene => self.save_scene(),
            build::BuildUiAction::OpenFolder(folder) => build::open_folder(&folder),
            build::BuildUiAction::ChooseFolder => {
                let start = self
                    .project_root()
                    .map(|root| self.build_settings.output_folder(&root));
                let mut dialog = rfd::FileDialog::new().set_title("Where builds go");
                if let Some(start) = start.filter(|p| p.is_dir()).or_else(|| self.project_root()) {
                    dialog = dialog.set_directory(start);
                }
                if let Some(folder) = dialog.pick_folder() {
                    self.build_settings.output = folder.display().to_string();
                }
            }
        }
    }

    /// Save this project's build choices and start a build on its own thread.
    fn start_build(&mut self) {
        if self.build_job.is_some() {
            return;
        }
        let (Some(builder), Some(root)) = (build::game_builder(), self.project_root()) else {
            return;
        };
        let mut manifest = read_manifest(&root);
        manifest.build = self.build_settings.clone();
        write_manifest(&root, &manifest);
        let name = self
            .project_name
            .clone()
            .unwrap_or_else(|| "Game".to_string());
        self.build_progress = vec!["Starting…".to_string()];
        self.build_result = None;
        self.build_job = Some(build::BuildJob::start(build::BuildRequest {
            project: root,
            name,
            version: manifest.version,
            settings: self.build_settings.clone(),
            builder: builder.clone(),
        }));
    }

    /// True if the open scene has changes that haven't been saved.
    fn is_dirty(&self) -> bool {
        self.dirty || self.world.script_library != self.saved_scripts
    }
    /// Record that the world now matches what is on disk (after a save or a
    /// load), clearing the unsaved-changes flag.
    fn mark_saved(&mut self) {
        self.dirty = false;
        self.saved_scripts = self.world.script_library.clone();
    }
    /// Run an action that discards unsaved changes, or, if there are any, park
    /// it behind the unsaved-changes prompt instead.
    fn request_guarded(&mut self, action: GuardedAction, event_loop: &ActiveEventLoop) {
        if matches!(self.mode, AppMode::Editor) && self.is_dirty() {
            self.pending_unsaved = Some(action);
        } else {
            self.run_guarded(action, event_loop);
        }
    }
    fn run_guarded(&mut self, action: GuardedAction, event_loop: &ActiveEventLoop) {
        match action {
            GuardedAction::Quit => event_loop.exit(),
            GuardedAction::CloseProject => self.close_project(),
            GuardedAction::ReloadScene => self.reload_scene(),
            GuardedAction::OpenScene => self.open_scene(),
            GuardedAction::SwitchScene => {
                if let Some(name) = self.pending_scene_switch.take() {
                    self.open_scene_named(&name);
                }
            }
        }
    }
    /// Snapshot the world onto the undo stack before a mutating action. Clears
    /// the redo stack (a new edit invalidates any redo history) and caps history.
    fn push_undo(&mut self) {
        const MAX_UNDO: usize = 50;
        self.undo_stack.push(self.world.clone());
        if self.undo_stack.len() > MAX_UNDO {
            self.undo_stack.remove(0);
        }
        self.redo_stack.clear();
        self.dirty = true;
    }
    /// Restore the previous world state, moving the current one onto the redo
    /// stack.
    fn undo(&mut self) {
        if let Some(prev) = self.undo_stack.pop() {
            self.redo_stack.push(self.world.clone());
            self.world = prev;
            self.selected = None;
            self.dirty = true;
            self.log("Undo".to_string());
        } else {
            self.log("Nothing to undo".to_string());
        }
    }
    /// Re-apply an undone change.
    fn redo(&mut self) {
        if let Some(next) = self.redo_stack.pop() {
            self.undo_stack.push(self.world.clone());
            self.world = next;
            self.selected = None;
            self.dirty = true;
            self.log("Redo".to_string());
        } else {
            self.log("Nothing to redo".to_string());
        }
    }
    /// Enter or leave the flythrough camera. Entering grabs and hides the cursor
    /// so mouse motion becomes look input; leaving restores it. Locked grab gives
    /// raw motion for smooth mouselook; Confined is a fallback if Locked isn't
    /// supported (some Wayland setups).
    fn set_fly(&mut self, on: bool) {
        if self.fly_mode == on {
            return;
        }
        self.fly_mode = on;
        if on {
            // Seed the free camera from the current orbit eye, so flight begins
            // exactly where the orbit view was.
            let offset = Vec3::new(
                self.cam_pitch.cos() * self.cam_yaw.sin(),
                self.cam_pitch.sin(),
                self.cam_pitch.cos() * self.cam_yaw.cos(),
            ) * self.cam_distance;
            self.cam_eye = DVec3::new(self.cam_focus_x, self.cam_focus_y, self.cam_focus_z)
                + offset.as_dvec3();
            self.fly_picked = false;
        } else {
            // Leaving flight: remember the fly speed if scrolling changed it.
            self.persist_prefs_if_changed();
            // Return to orbit. Pivot around the entity picked while flying, or —
            // if none — a point straight ahead, so the view doesn't jump. Then
            // rebuild yaw/pitch/distance so the orbit eye stays where flight left
            // it (looking at the pivot).
            if !self.fly_picked {
                let ahead = self.cam_eye
                    + (view_forward(self.cam_yaw, self.cam_pitch) * self.cam_distance).as_dvec3();
                self.cam_focus_x = ahead.x;
                self.cam_focus_y = ahead.y;
                self.cam_focus_z = ahead.z;
            }
            let focus = DVec3::new(self.cam_focus_x, self.cam_focus_y, self.cam_focus_z);
            let offset = (self.cam_eye - focus).as_vec3();
            let dist = offset.length();
            if dist > 0.001 {
                self.cam_distance = dist.clamp(10.0, 2000.0);
                self.cam_pitch = (offset.y / dist).asin().clamp(-1.4, 1.4);
                self.cam_yaw = offset.x.atan2(offset.z);
            }
        }
        if let Some(window) = &self.window {
            if on {
                let _ = window
                    .set_cursor_grab(winit::window::CursorGrabMode::Locked)
                    .or_else(|_| window.set_cursor_grab(winit::window::CursorGrabMode::Confined));
                window.set_cursor_visible(false);
            } else {
                let _ = window.set_cursor_grab(winit::window::CursorGrabMode::None);
                window.set_cursor_visible(true);
            }
        }
    }
    /// Spawn a new entity at the camera focus and select it.
    fn spawn_at_focus(&mut self) {
        self.push_undo();
        let id = self.world.spawn(
            Position {
                x: self.cam_focus_x,
                y: self.cam_focus_y,
                z: 0.0,
            },
            Velocity {
                dx: 0.0,
                dy: 0.0,
                dz: 0.0,
            },
        );
        self.selected = Some(id);
        self.log(format!("Spawned entity {id}"));
    }
    /// Spawn a fresh entity from a saved prefab at the camera focus, and
    /// select it — the prefab equivalent of `spawn_at_focus`. Does nothing
    /// if `name` isn't a prefab this project has (a stale button from a
    /// frame where the file existed and was since deleted, say).
    fn spawn_prefab_at_focus(&mut self, name: &str) {
        let Some(prefab) = self.prefabs.get(name).cloned() else {
            self.log(format!("No such prefab: '{name}'"));
            return;
        };
        self.push_undo();
        let id = self.world.spawn_from_prefab(
            &prefab,
            Position {
                x: self.cam_focus_x,
                y: self.cam_focus_y,
                z: 0.0,
            },
        );
        self.selected = Some(id);
        self.log(format!("Spawned entity {id} from prefab '{name}'"));
    }
    /// Save entity `id`'s current shape as a reusable prefab file under the
    /// open project's `prefabs/` folder (created if missing), named `name`.
    /// Overwrites an existing prefab of the same name; there's no separate
    /// rename or delete for prefabs yet, only Save. This is the unlinked
    /// stamp itself: existing entities keep no memory of which prefab, if
    /// any, they were spawned from, so saving over one never touches them.
    fn save_entity_as_prefab(&mut self, id: usize, name: &str) {
        let Some(prefab) = self.world.capture_prefab(id) else {
            self.log(format!(
                "Couldn't save prefab: entity {id} no longer exists"
            ));
            return;
        };
        let Some(root) = self.project_dir.as_deref().map(|p| p.to_path_buf()) else {
            self.log("Couldn't save prefab: no project open".to_string());
            return;
        };
        let dir = root.join("prefabs");
        if let Err(e) = std::fs::create_dir_all(&dir) {
            self.log(format!("Couldn't save prefab: {e}"));
            return;
        }
        let text = match ron::ser::to_string_pretty(&prefab, ron::ser::PrettyConfig::default()) {
            Ok(text) => text,
            Err(e) => {
                self.log(format!("Couldn't save prefab: {e}"));
                return;
            }
        };
        if let Err(e) = std::fs::write(dir.join(format!("{name}.ron")), text) {
            self.log(format!("Couldn't save prefab: {e}"));
            return;
        }
        self.prefabs.insert(name.to_string(), prefab);
        self.log(format!("Saved prefab '{name}'"));
    }

    /// Makes `child` a child of `target`, requested by dragging one row onto
    /// another in the Scene tab's tree (see `scene_tab_ui`); that function
    /// already rejects a drop onto the entity itself or one of its own
    /// descendants, so this is only ever called with a drop that's safe to
    /// apply. Keeps the entity's existing offset if it was already parented
    /// to something else — only the `entity` field changes — or starts at
    /// `Parent::default()`'s zero offset for a fresh attachment, the same
    /// default the Inspector's "Attached to another entity" checkbox uses.
    fn reparent_entity(&mut self, child: usize, target: usize) {
        if child == target {
            return;
        }
        self.push_undo();
        let offset = self.world.parents.get(child).copied().unwrap_or_default();
        self.world.parents.insert(
            child,
            frame_engine::world::Parent {
                entity: target,
                ..offset
            },
        );
        self.log(format!("Entity {child} is now a child of entity {target}"));
    }

    /// Despawn the selected entity, if any.
    fn despawn_selected(&mut self) {
        if let Some(id) = self.selected {
            self.push_undo();
            self.world.despawn(id);
            self.selected = None;
            self.log(format!("Despawned entity {id}"));
        }
    }
    /// The open scene's name (`World` for `scenes/World.ron`).
    fn current_scene_name(&self) -> Option<String> {
        self.current_scene_path
            .as_ref()
            .and_then(|p| p.file_stem())
            .and_then(|s| s.to_str())
            .map(String::from)
    }

    /// Save the open scene: the scene to `scenes/<name>.ron`, and the
    /// project's shared data and scripts to their own files (see `project`).
    fn save_scene(&mut self) {
        let (Some(root), Some(name)) = (self.project_root(), self.current_scene_name()) else {
            self.log("Open a project to save a scene".to_string());
            return;
        };
        match project::save_scene(&root, &name, &self.world) {
            Ok(changes) => {
                self.mark_saved();
                self.log(format!("Saved scene '{name}'"));
                for change in changes {
                    self.log(format!("  {change}"));
                }
                stamp_project(&root);
            }
            Err(e) => {
                if self.world.newer_format().is_some() {
                    self.version_notice = Some(format!("Not saved: {e}."));
                }
                self.log(format!("Save failed: {e}"));
            }
        }
    }

    /// After a scene loads (or fails to), say so if it came from a newer
    /// Frame Editor: it opened but can't be saved over, or it can't be
    /// read here at all.
    fn check_scene_version(&mut self, result: Result<(), String>) {
        match result {
            Ok(()) => {
                if let Some(found) = self.world.newer_format() {
                    let text = format!(
                        "This scene was saved by a newer Frame Editor (save format {found}; this is {}, which writes format {}). It opened, but anything this editor doesn't know about isn't shown, so Save is turned off for it. Use Save As to keep a copy, or update Frame Editor.",
                        frame_engine::version::ENGINE,
                        frame_engine::world::SCENE_FORMAT
                    );
                    self.log(text.clone());
                    self.version_notice = Some(text);
                }
            }
            Err(e) => {
                if e.contains("newer Frame Editor") {
                    self.version_notice = Some(format!("Could not open: {e}."));
                }
            }
        }
    }

    /// Save the open scene under a new name in the project's `scenes/`
    /// folder, and carry on editing the new one.
    fn save_scene_as(&mut self) {
        let Some(root) = self.project_root() else {
            return;
        };
        let scenes_dir = root.join(project::SCENES_DIR);
        let _ = std::fs::create_dir_all(&scenes_dir);
        let picked = rfd::FileDialog::new()
            .add_filter("Frame scene", &["ron"])
            .set_directory(&scenes_dir)
            .set_file_name(format!(
                "{} copy.ron",
                self.current_scene_name()
                    .unwrap_or_else(|| "Scene".to_string())
            ))
            .set_title("Save scene as")
            .save_file();
        let Some(path) = picked else {
            return;
        };
        let Some(name) = scene_name_in(&scenes_dir, &path) else {
            self.log(format!(
                "Scenes are kept in the project's scenes folder ({}); choose a name there",
                scenes_dir.display()
            ));
            return;
        };
        // Choosing a new file on purpose is the way to keep a scene from a
        // newer editor; the copy is in this editor's format.
        self.world.clear_newer_format();
        match project::save_scene(&root, &name, &self.world) {
            Ok(changes) => {
                self.mark_saved();
                self.log(format!("Saved scene as '{name}'"));
                for change in changes {
                    self.log(format!("  {change}"));
                }
                stamp_project(&root);
                self.current_scene_path = Some(root.join(project::scene_file(&name)));
            }
            Err(e) => self.log(format!("Save failed: {e}")),
        }
    }

    /// Pick a scene from the project's `scenes/` folder and open it.
    fn open_scene(&mut self) {
        let Some(root) = self.project_root() else {
            return;
        };
        let scenes_dir = root.join(project::SCENES_DIR);
        let picked = rfd::FileDialog::new()
            .add_filter("Frame scene", &["ron"])
            .set_directory(&scenes_dir)
            .set_title("Open scene")
            .pick_file();
        let Some(path) = picked else {
            return;
        };
        match scene_name_in(&scenes_dir, &path) {
            Some(name) => self.open_scene_named(&name),
            None => self.log(format!(
                "Only scenes in this project's scenes folder can be opened ({})",
                scenes_dir.display()
            )),
        }
    }

    /// Open one of the project's scenes by name, replacing the open one.
    /// Undo history belongs to the scene it was made in, so it is cleared.
    fn open_scene_named(&mut self, name: &str) {
        let Some(root) = self.project_root() else {
            return;
        };
        let files = ProjectFiles::Folder(root.clone());
        match project::load_scene(&files, name) {
            Ok(world) => {
                self.world = world;
                self.selected = None;
                self.undo_stack.clear();
                self.redo_stack.clear();
                self.current_scene_path = Some(root.join(project::scene_file(name)));
                // Plugins' scripts and the models' materials go back in.
                self.load_project_assets();
                self.mark_saved();
                self.log(format!("Opened scene '{name}'"));
                self.check_scene_version(Ok(()));
            }
            Err(e) => {
                self.log(format!("Could not open scene '{name}': {e}"));
                self.check_scene_version(Err(e));
            }
        }
    }

    /// Reload the open scene from disk, discarding unsaved changes.
    fn reload_scene(&mut self) {
        let Some(name) = self.current_scene_name() else {
            self.log("No scene to reload".to_string());
            return;
        };
        self.open_scene_named(&name);
    }

    /// Take in script files changed outside the editor since the last load
    /// or save (see `scenes::merge_outside_edits`). Runs when the editor
    /// window gets focus back.
    fn pick_up_outside_script_edits(&mut self) {
        let Some(root) = self.project_root() else {
            return;
        };
        if !root.join(project::SHARED_FILE).is_file() {
            return;
        }
        let (disk, errors) = project::read_scripts(&ProjectFiles::Folder(root));
        for e in errors {
            self.log(format!("Could not read script {e}"));
        }
        let installed = &self.world.installed_plugins;
        let lines = scenes::merge_outside_edits(
            &mut self.world.script_library,
            &mut self.saved_scripts,
            &disk,
            |name| {
                name.split_once('/')
                    .is_some_and(|(owner, _)| installed.contains_key(owner))
            },
        );
        for line in lines {
            self.log(line);
        }
    }

    /// What the Scenes tab shows.
    fn scenes_view(&self) -> Option<scenes::ScenesView> {
        let root = self.project_root()?;
        let files = ProjectFiles::Folder(root.clone());
        Some(scenes::ScenesView {
            scenes: project::list_scenes(&files),
            open: self.current_scene_name(),
            start: project::start_scene_path(&files)
                .and_then(|p| {
                    p.strip_prefix("scenes/")
                        .and_then(|n| n.strip_suffix(".ron"))
                        .map(String::from)
                })
                .unwrap_or_default(),
            unsaved: self.is_dirty(),
        })
    }

    /// Carry out a Scenes tab action.
    fn handle_scene_action(&mut self, action: scenes::SceneAction, event_loop: &ActiveEventLoop) {
        let Some(root) = self.project_root() else {
            return;
        };
        let result = match action {
            scenes::SceneAction::Open(name) => {
                self.pending_scene_switch = Some(name);
                self.request_guarded(GuardedAction::SwitchScene, event_loop);
                Ok(None)
            }
            scenes::SceneAction::New(name) => scenes::create_scene(&root, &name, &default_world())
                .map(|()| {
                    self.pending_scene_switch = Some(name.clone());
                    self.request_guarded(GuardedAction::SwitchScene, event_loop);
                    Some(format!("Created scene '{name}'"))
                }),
            scenes::SceneAction::Duplicate(from, to) => scenes::duplicate_scene(&root, &from, &to)
                .map(|()| Some(format!("Copied scene '{from}' to '{to}'"))),
            scenes::SceneAction::Rename(from, to) => scenes::rename_scene(&root, &from, &to)
                .map(|()| Some(format!("Renamed scene '{from}' to '{to}'"))),
            scenes::SceneAction::Delete(name) => {
                scenes::delete_scene(&root, &name).map(|()| Some(format!("Deleted scene '{name}'")))
            }
            scenes::SceneAction::SetStart(name) => {
                scenes::set_start_scene(&root, &name);
                Ok(Some(format!("The game now starts on '{name}'")))
            }
        };
        match result {
            Ok(Some(message)) => self.log(message),
            Ok(None) => {}
            Err(e) => self.log(format!("Scenes: {e}")),
        }
    }

    /// Play the open scene in a game window, as it is now, saved or not.
    fn play_open_scene(&mut self, event_loop: &ActiveEventLoop) {
        let (Some(root), Some(name)) = (self.project_root(), self.current_scene_name()) else {
            return;
        };
        self.start_play(
            event_loop,
            ProjectFiles::Folder(root),
            Some((name, self.world.clone())),
        );
    }

    /// Draw the launcher screen: an egui-only frame (no simulation, no 3D) with
    /// buttons to create or open a project. Mirrors the editor's egui→render
    /// handoff, but renders an empty 3D scene behind the UI.
    fn draw_launcher(&mut self, event_loop: &ActiveEventLoop) {
        let mut action: Option<LauncherAction> = None;
        let recent = self.recent_projects.clone();
        let mut name_input = std::mem::take(&mut self.new_project_name);
        // Project-settings window state, lifted so the closure doesn't touch self.
        let was_settings_open = self.settings_open;
        let pending_delete = self.pending_delete.clone();
        let mut settings_open = self.settings_open;
        let mut s_name = std::mem::take(&mut self.settings_name);
        let mut s_desc = std::mem::take(&mut self.settings_description);
        let mut s_version = std::mem::take(&mut self.settings_version);
        let notice = self.version_notice.clone();
        let mut dismiss_notice = false;
        let (jobs, tex_delta, ppp) = if let (Some(state), Some(window)) =
            (self.egui_state.as_mut(), self.window.as_ref())
        {
            let raw_input = state.take_egui_input(window);
            let full_output = self.egui_ctx.run_ui(raw_input, |ui| {
                egui::CentralPanel::default().show(ui, |ui| {
                    ui.add_space(12.0);
                                    ui.heading(app_name());
                    ui.add_space(10.0);
                    // Header bar: create a named project, or open an existing one.
                    ui.horizontal(|ui| {
                        ui.label("New project:");
                        ui.add(
                            egui::TextEdit::singleline(&mut name_input)
                                .hint_text("name")
                                .desired_width(200.0),
                        );
                        if ui.button("Create…").clicked() {
                            action = Some(LauncherAction::NewProject);
                        }
                        ui.add_space(16.0);
                        if ui.button("Open existing project…").clicked() {
                            action = Some(LauncherAction::OpenProject);
                        }
                    });
                    ui.add_space(12.0);
                    ui.separator();
                    ui.add_space(8.0);

                    if recent.is_empty() {
                        ui.weak("No projects yet — create one above to begin.");
                    } else {
                        ui.heading("Projects");
                        ui.add_space(8.0);
                        egui::ScrollArea::vertical()
                            .auto_shrink([false, false])
                            .show(ui, |ui| {
                                for proj in &recent {
                                    ui.group(|ui| {
                                        // Fill the width so each project is a
                                        // full-width row: name and date on the
                                        // left, actions pushed to the right.
                                        ui.set_min_width(ui.available_width());
                                        ui.horizontal(|ui| {
                                            ui.vertical(|ui| {
                                                ui.label(
                                                    egui::RichText::new(&proj.name)
                                                        .size(18.0)
                                                        .strong(),
                                                );
                                            });
                                            ui.with_layout(
                                                egui::Layout::right_to_left(egui::Align::Center),
                                                |ui| {
                                                    if pending_delete.as_deref()
                                                        == Some(proj.root.as_path())
                                                    {
                                                        // Confirm step: this card's
                                                        // Delete was clicked.
                                                        if ui.button("Cancel").clicked() {
                                                            action = Some(
                                                                LauncherAction::CancelDelete,
                                                            );
                                                        }
                                                        if ui
                                                            .button(
                                                                egui::RichText::new(
                                                                    "Delete folder",
                                                                )
                                                                .color(egui::Color32::from_rgb(
                                                                    0xe0, 0x6c, 0x6c,
                                                                )),
                                                            )
                                                            .clicked()
                                                        {
                                                            action = Some(
                                                                LauncherAction::DeleteProject(
                                                                    proj.root.clone(),
                                                                ),
                                                            );
                                                        }
                                                        ui.weak("Deletes the folder and everything in it.");
                                                    } else {
                                                        if ui.button("Delete").clicked() {
                                                            action = Some(
                                                                LauncherAction::ConfirmDelete(
                                                                    proj.root.clone(),
                                                                ),
                                                            );
                                                        }
                                                        if ui.button("Settings").clicked() {
                                                            action =
                                                                Some(LauncherAction::OpenSettings(
                                                                    proj.root.clone(),
                                                                ));
                                                        }
                                                        if ui.button("Play").clicked() {
                                                            action = Some(LauncherAction::PlayRecent(
                                                                proj.root.clone(),
                                                            ));
                                                        }
                                                        if ui.button("Edit").clicked() {
                                                            action = Some(LauncherAction::OpenRecent(
                                                                proj.root.clone(),
                                                            ));
                                                        }
                                                    }
                                                },
                                            );
                                        });
                                        if !proj.description.is_empty() {
                                            ui.add_space(4.0);
                                            egui::ScrollArea::vertical()
                                                .id_salt(("card_desc", &proj.root))
                                                .max_height(60.0)
                                                .auto_shrink([false, true])
                                                .show(ui, |ui| {
                                                    ui.label(&proj.description);
                                                });
                                        }
                                        if !proj.version.is_empty() {
                                            ui.add_space(6.0);
                                            ui.horizontal(|ui| {
                                                ui.weak(format!(
                                                    "Last edited {}",
                                                    format_edited(proj.modified)
                                                ));
                                                ui.with_layout(
                                                    egui::Layout::right_to_left(
                                                        egui::Align::Center,
                                                    ),
                                                    |ui| {
                                                        ui.add_space(4.0);
                                                        ui.weak(format!("v{}", proj.version));
                                                    },
                                                );
                                            });
                                        } else {
                                            ui.add_space(6.0);
                                            ui.weak(format!(
                                                "Last edited {}",
                                                format_edited(proj.modified)
                                            ));
                                        }
                                    });
                                    ui.add_space(6.0);
                                }
                            });
                    }
                });
                if let Some(text) = &notice {
                    dismiss_notice |= version_notice_ui(ui.ctx(), text);
                }
                // Project-settings window, floating above the launcher. Closing
                // it (its X or Save) flips `settings_open`, which triggers a save.
                if settings_open {
                    let mut keep_open = true;
                    let mut save_clicked = false;
                    egui::Window::new("Project settings")
                        .collapsible(false)
                        .resizable(false)
                        .open(&mut keep_open)
                        .show(ui.ctx(), |ui| {
                            egui::Grid::new("settings_grid")
                                .num_columns(2)
                                .spacing([8.0, 8.0])
                                .show(ui, |ui| {
                                    ui.label("Name");
                                    ui.text_edit_singleline(&mut s_name);
                                    ui.end_row();
                                    ui.label("Version");
                                    ui.text_edit_singleline(&mut s_version);
                                    ui.end_row();
                                });
                            ui.add_space(6.0);
                            ui.label("Description");
                            ui.text_edit_multiline(&mut s_desc);
                            ui.add_space(10.0);
                            ui.weak("Closing saves. Renaming changes the scene file.");
                            ui.add_space(6.0);
                            if ui.button("Save & close").clicked() {
                                save_clicked = true;
                            }
                        });
                    if !keep_open || save_clicked {
                        settings_open = false;
                    }
                }
            });
            state.handle_platform_output(window, full_output.platform_output);
            let ppp = full_output.pixels_per_point;
            let jobs = self.egui_ctx.tessellate(full_output.shapes, ppp);
            (jobs, full_output.textures_delta, ppp)
        } else {
            (Vec::new(), egui::TexturesDelta::default(), 1.0)
        };
        if let Some(gpu) = &mut self.gpu {
            gpu.sync_modules(&[], DVec3::ZERO);
            gpu.render(
                &[],
                &[0, 0, 0],
                &[],
                Mat4::IDENTITY.to_cols_array_2d(),
                &[LightRaw::zeroed(); MAX_LIGHTS],
                &jobs,
                &tex_delta,
                ppp,
            );
        }
        self.new_project_name = name_input;
        if was_settings_open && !settings_open {
            // The window closed this frame (X or Save) — persist and refresh.
            self.save_settings(s_name, s_desc, s_version);
        } else {
            self.settings_open = settings_open;
            self.settings_name = s_name;
            self.settings_description = s_desc;
            self.settings_version = s_version;
        }
        if dismiss_notice {
            self.version_notice = None;
        }
        match action {
            Some(LauncherAction::NewProject) => self.new_project(),
            Some(LauncherAction::OpenProject) => self.open_project(),
            Some(LauncherAction::OpenRecent(root)) => self.open_project_at(root),
            Some(LauncherAction::PlayRecent(root)) => {
                self.start_play(event_loop, ProjectFiles::Folder(root), None)
            }
            Some(LauncherAction::OpenSettings(root)) => self.open_settings(root),
            Some(LauncherAction::ConfirmDelete(root)) => self.pending_delete = Some(root),
            Some(LauncherAction::CancelDelete) => self.pending_delete = None,
            Some(LauncherAction::DeleteProject(root)) => self.delete_project(root),
            None => {}
        }
        if let Some(window) = &self.window {
            window.request_redraw();
        }
    }

    /// Create a project: take the name typed on the launcher, pick a folder,
    /// and make a first scene, `scenes/<name>.ron`, which is also the start
    /// scene, and `project.ron` with the project's name.
    fn new_project(&mut self) {
        let name = self.new_project_name.trim().to_string();
        if name.is_empty() {
            self.log("Type a project name first".to_string());
            return;
        }
        if name.contains(['/', '\\']) {
            self.log("Project name can't contain slashes".to_string());
            return;
        }
        let Some(folder) = rfd::FileDialog::new()
            .set_title("Choose a folder for the new project")
            .pick_folder()
        else {
            return;
        };
        if !project::valid_scene_name(&name) {
            self.log(format!("'{name}' can't be used as a file name"));
            return;
        }
        // The first scene is named after the project, and is its start scene.
        let world = default_world();
        match project::save_scene(&folder, &name, &world) {
            Ok(_) => {
                write_manifest(
                    &folder,
                    &ProjectManifest {
                        name: name.clone(),
                        start_scene: name.clone(),
                        version: "0.1.0".to_string(),
                        ..Default::default()
                    },
                );
                self.world = world;
                self.new_project_name.clear();
                self.add_recent_project(&folder);
                let scene_path = folder.join(project::scene_file(&name));
                self.enter_editor(folder, scene_path);
            }
            Err(e) => self.log(format!("Could not create project: {e}")),
        }
    }

    /// Open a project: pick its folder and load the scene inside it.
    fn open_project(&mut self) {
        let Some(root) = rfd::FileDialog::new()
            .set_title("Open a project folder")
            .pick_folder()
        else {
            return;
        };
        self.open_project_at(root);
    }

    /// Open the project at a known folder (the folder picker and the recent
    /// list both route here). Loads the folder's scene file.
    fn open_project_at(&mut self, root: std::path::PathBuf) {
        let files = ProjectFiles::Folder(root.clone());
        // A project from before scenes had their own folder is converted
        // first (with a backup), then opened like any other.
        if project::is_old_layout(&files) {
            match scenes::convert_old_project(&root) {
                Ok(done) => {
                    for line in done {
                        self.log(line);
                    }
                }
                Err(e) => {
                    self.log(format!(
                        "Could not convert this project to the scenes layout: {e}"
                    ));
                    return;
                }
            }
        }
        let Some(scene) = files.scene() else {
            self.log("That folder has no scene to open".to_string());
            return;
        };
        let scene_path = root.join(&scene);
        // The project's own stamp first: it names the editor version,
        // which reads better than a save format number.
        let manifest = read_manifest(&root);
        let newer_project = frame_engine::version::is_newer_than_this(&manifest.editor);
        match project::load_scene_at(&files, &scene) {
            Ok(world) => {
                self.world = world;
                self.undo_stack.clear();
                self.redo_stack.clear();
                self.add_recent_project(&root);
                self.enter_editor(root.clone(), scene_path);
                self.check_scene_version(Ok(()));
                if newer_project && self.version_notice.is_none() {
                    let text = format!(
                        "This project was last saved with Frame Editor {}, and this is {}. It opened fine, but a newer editor may have used features this one doesn't have. Updating Frame Editor is the safe choice.",
                        manifest.editor,
                        frame_engine::version::ENGINE
                    );
                    self.log(text.clone());
                    self.version_notice = Some(text);
                }
            }
            Err(e) => {
                self.log(format!("Could not open project: {e}"));
                if newer_project {
                    self.version_notice = Some(format!(
                        "Could not open this project. It was last saved with Frame Editor {}, and this is {}. Update Frame Editor to open it. ({e})",
                        manifest.editor,
                        frame_engine::version::ENGINE
                    ));
                } else {
                    self.check_scene_version(Err(e.to_string()));
                }
            }
        }
    }

    /// Record a project as recently used and refresh the sorted launcher list.
    fn add_recent_project(&mut self, root: &std::path::Path) {
        let mut roots = read_recent_projects();
        roots.retain(|r| r != root);
        roots.insert(0, root.to_path_buf());
        roots.truncate(20);
        write_recent_projects(&roots);
        self.recent_projects = sorted_recent_projects();
    }

    /// Close the open project and return to the launcher. Does not save — use
    /// Save (F5) first to keep changes.
    fn close_project(&mut self) {
        self.custom_meshes.clear();
        if let Some(gpu) = &mut self.gpu {
            gpu.set_custom_meshes(&[]);
        }
        self.mode = AppMode::Launcher;
        self.project_name = None;
        self.current_scene_path = None;
        self.project_dir = None;
        self.selected = None;
        self.paused = false;
        self.world = World::default();
        self.recent_projects = sorted_recent_projects();
        if let Some(window) = &self.window {
            window.set_title(app_name());
        }
        self.window_title.clear();
        self.mark_saved();
    }

    /// Open the project-settings window for a project, loading its current name
    /// (the scene stem) and manifest (description, version) into the fields.
    fn open_settings(&mut self, root: std::path::PathBuf) {
        let name = project_display_name(&root);
        let manifest = read_manifest(&root);
        self.settings_orig_name = name.clone();
        self.settings_name = name;
        self.settings_description = manifest.description;
        self.settings_version = manifest.version;
        self.settings_root = Some(root);
        self.settings_open = true;
    }

    /// Save the project-settings window's fields into the manifest. The
    /// project's name lives there now, so renaming moves no files. Called
    /// when the window closes.
    fn save_settings(&mut self, name: String, description: String, version: String) {
        let Some(root) = self.settings_root.clone() else {
            return;
        };
        let mut manifest = read_manifest(&root);
        let new_name = name.trim();
        if !new_name.is_empty() && new_name != self.settings_orig_name {
            manifest.name = new_name.to_string();
            self.log(format!("Renamed project to '{new_name}'"));
        }
        // The start scene and build choices are kept as they are; the
        // Scenes tab and File > Build Game edit those.
        manifest.description = description;
        manifest.version = version;
        write_manifest(&root, &manifest);
        self.settings_open = false;
        self.settings_root = None;
        self.recent_projects = sorted_recent_projects();
    }

    /// Delete a project: remove its folder (and everything in it) from disk and
    /// drop it from the recent-projects list. Only reachable via the card's
    /// two-step Delete -> Delete folder confirmation.
    fn delete_project(&mut self, root: std::path::PathBuf) {
        self.pending_delete = None;
        let name = root
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("project")
            .to_string();
        match std::fs::remove_dir_all(&root) {
            Ok(()) => self.log(format!("Deleted project '{name}'")),
            Err(e) => self.log(format!("Could not delete '{name}': {e}")),
        }
        // Rewrite the recents file without this folder, whether or not the
        // filesystem delete succeeded partially.
        if let Some(path) = recent_projects_file() {
            if let Ok(text) = std::fs::read_to_string(&path) {
                let kept: Vec<&str> = text
                    .lines()
                    .filter(|line| {
                        let line = line.trim();
                        !line.is_empty() && std::path::Path::new(line) != root.as_path()
                    })
                    .collect();
                let _ = std::fs::write(&path, kept.join("\n"));
            }
        }
        self.recent_projects = sorted_recent_projects();
    }

    /// Play a project: open a separate, clean game window (its own GPU surface),
    /// load the project's scene into it, and run the simulation there with no
    /// editor chrome. Closing the window returns to the launcher. `files` is
    /// the project folder, or a game build's pack; everything Play reads
    /// comes through it.
    fn start_play(
        &mut self,
        event_loop: &ActiveEventLoop,
        files: ProjectFiles,
        scene: Option<(String, World)>,
    ) {
        if self.game_window.is_some() {
            return; // already playing
        }
        // The editor's Play scene hands over the scene being edited; the
        // launcher and a game build start on the project's start scene.
        let loaded = match scene {
            Some(given) => Ok(given),
            None => project::load_start_scene(&files),
        };
        let (name, mut world) = match loaded {
            Ok(loaded) => loaded,
            Err(e) => {
                self.log(format!("Could not play project: {e}"));
                return;
            }
        };
        // Plugins are re-scanned fresh here too, same reasoning as the model
        // load just below: the scene file only has whatever was true as of
        // the last save, and Play should reflect what's really on disk now
        // (a plugin edited or dropped in since), not a stale snapshot.
        // Enabled/disabled choices already saved in the scene are kept.
        let (manifests, scripts, errors) = load_plugins_from(&files);
        for e in errors {
            self.log(format!("Plugins: {e}"));
        }
        merge_plugins_into_world(&mut world, manifests, scripts);
        // A game on its own is titled with its own name and grouped under its
        // own app id; the editor's Play window says what it is playing.
        let player = matches!(self.mode, AppMode::Player);
        let title = if player {
            app_name().to_string()
        } else {
            format!("{name} — Play")
        };
        #[allow(unused_mut)]
        let mut attributes = Window::default_attributes().with_title(title);
        #[cfg(target_os = "linux")]
        if player {
            use winit::platform::wayland::WindowAttributesExtWayland;
            attributes = attributes.with_name(app_id(), app_id());
        }
        let window = match event_loop.create_window(attributes) {
            Ok(w) => Arc::new(w),
            Err(e) => {
                self.log(format!("Could not open game window: {e}"));
                return;
            }
        };
        let (mut gpu, shared) = GpuState::new(
            window.clone(),
            self.gpu_shared.as_ref(),
            &self.modules.borrow(),
        );
        // A game on its own has no editor window, so the game window is the
        // first and makes the shared device.
        if self.gpu_shared.is_none() {
            self.gpu_shared = Some(shared);
        }
        // The game window has its own GpuState (pipelines, buffers,
        // egui_renderer), so the project's models get parsed and uploaded
        // for it separately, even though it shares the same underlying
        // device as the main window now. Play can start from the launcher
        // with no project open in the editor, so load from `files`.
        let (models, errors) = load_models_from(&files);
        for e in errors {
            self.log(format!("Model load failed: {e}"));
        }
        self.game_custom_names = models.keys().cloned().collect();
        let refs: Vec<&frame_engine::assets::MeshData> = models.values().collect();
        // A fresh, local decode cache: Play starts far less often than a
        // material gets tweaked in the editor, so there's no need to share
        // App::texture_cache here.
        let mut game_texture_cache = std::collections::HashMap::new();
        let materials = build_material_gpu_data(
            &self.game_custom_names,
            &world.mesh_meta,
            Some(&files),
            &mut game_texture_cache,
        );
        let pairs: Vec<(&frame_engine::assets::MeshData, &MaterialGpuData)> =
            refs.iter().copied().zip(materials.iter()).collect();
        gpu.set_custom_meshes(&pairs);
        // The game window's own egui context, used only for the UiText/
        // UiImage overlay (see render_game_ui) -- same creation shape as
        // pop_out_tab's PoppedOutWindow, just kept as loose fields instead
        // of a struct since there's no window-to-tab map needed for just
        // one window.
        let game_egui_ctx = egui::Context::default();
        let game_egui_state = egui_winit::State::new(
            game_egui_ctx.clone(),
            egui::ViewportId::ROOT,
            window.as_ref(),
            None,
            None,
            None,
        );
        window.request_redraw();
        self.game_gpu = Some(gpu);
        self.game_world = Some(world);
        // Fresh physics state for this play session; see `game_physics`'s
        // own doc comment for why it isn't reused across Play runs.
        self.game_physics = Some(Physics::new(GRAVITY_Y));
        self.game_input = InputState::new();
        self.game_clock = Clock::new(TICK_RATE, MAX_CATCHUP_TICKS);
        self.show_play_stats = false;
        self.play_panels_open = false;
        self.play_perf = perf::FrameStats::frames_only(std::time::Instant::now());
        self.game_egui_ctx = Some(game_egui_ctx);
        self.game_egui_state = Some(game_egui_state);
        // Cleared rather than just left from a previous session: an image
        // name can mean a different file in a different project.
        self.game_ui_textures.clear();
        // The mouse is only taken when the window is clicked: grabbing it
        // before the window has focus can leave it stuck and hidden.
        self.game_mouse_held = false;
        self.game_window = Some(window);
        self.game_files = Some(files);
        self.log(format!("Playing '{name}'"));
    }

    /// Change the game window to another scene of the same project, as a
    /// script's `load_scene` asks: fresh physics and the new scene's own
    /// entities. Globals carry across, so scripts can keep things like a
    /// score between scenes.
    fn switch_game_scene(&mut self, name: &str) {
        let Some(files) = self.game_files.clone() else {
            return;
        };
        let mut world = match project::load_scene(&files, name) {
            Ok(world) => world,
            Err(e) => {
                self.log(format!("load_scene: couldn't load '{name}': {e}"));
                return;
            }
        };
        let (manifests, scripts, errors) = load_plugins_from(&files);
        for e in errors {
            self.log(format!("Plugins: {e}"));
        }
        merge_plugins_into_world(&mut world, manifests, scripts);
        if let Some(old) = &self.game_world {
            world.globals = old.globals.clone();
        }
        self.game_world = Some(world);
        self.game_physics = Some(Physics::new(GRAVITY_Y));
        self.game_ui_textures.clear();
        if !matches!(self.mode, AppMode::Player)
            && let Some(window) = &self.game_window
        {
            window.set_title(&format!("{name} — Play"));
        }
        self.log(format!("Changed scene to '{name}'"));
    }

    /// Close the game window and stop playing. GPU state is dropped first (its
    /// surface holds a window handle) — the same teardown order as on exit.
    fn close_game(&mut self) {
        self.end_game_session();
        // A playing sound outlives its handle in kira, so loops are stopped
        // here or they would carry on after the window closes.
        self.game_audio.stop_all();
        self.game_audio_at = None;
        self.game_gpu = None;
        self.game_world = None;
        self.game_physics = None;
        self.game_egui_ctx = None;
        self.game_egui_state = None;
        self.game_ui_textures.clear();
        self.game_window = None;
        self.game_files = None;
        self.game_mouse_held = false;
        self.game_input = InputState::new();
    }

    /// Advance and render the game window: tick the game world on its own clock
    /// (always running), draw it with the shared 3D pipeline, and no egui/overlay.
    fn render_game(&mut self) {
        self.play_perf.begin(std::time::Instant::now());
        // The player's volume, from the game's menu: told to the mixer only
        // when it changes.
        if self.applied_volume != Some(self.game_volume)
            && let Some(manager) = self.audio_manager.as_mut()
        {
            manager.main_track().set_volume(
                kira::Decibels(game_menu::volume_decibels(self.game_volume)),
                kira::Tween::default(),
            );
            self.applied_volume = Some(self.game_volume);
        }
        let owed = self.game_clock.advance(true);
        // Sounds come from the files Play started with (the project folder,
        // or a game build's pack), not from whatever the editor has open.
        let game_files = self.game_files.clone();
        let mut sound_messages = Vec::new();
        for _ in 0..owed {
            if let Some(world) = self.game_world.as_mut() {
                systems::collision(world);
                // Input first, then scripts: a script can see (and override,
                // per entity, by touching move_dx/move_dz/move_jump) whatever
                // the built-in Controlled input already asked for this tick,
                // rather than the keyboard unconditionally overwriting
                // whatever the script just set.
                // In a session (a multiplayer server, say) the session moves
                // the player's character, not this program: it gets the
                // input, and there is no local movement, actions or physics.
                match self.game_session.as_mut() {
                    Some(session) => session.tick(world, &self.game_input),
                    None => {
                        systems::input_movement(world, &self.game_input);
                        systems::input_actions(world, &self.game_input);
                    }
                }
                systems::run_scripts(world, &mut self.script_runtime, &self.game_input);
                module_api::tick_modules(&self.modules, world, 1.0 / TICK_RATE as f32);
                systems::gravity(world);
                systems::movement(world);
                if self.game_session.is_none()
                    && let Some(physics) = self.game_physics.as_mut()
                {
                    module_api::sync_collision(&self.modules, world, physics);
                    physics.step(world, 1.0 / TICK_RATE as f32);
                }
                systems::resolve_collisions(world);
                // Last, so a mounted camera (or anything else riding on a
                // Parent) sees this tick's real final position, not the
                // parent's position from before movement/physics ran.
                systems::apply_parenting(world);
                sound_messages.extend(update_sounds(
                    world,
                    self.audio_manager.as_mut(),
                    &mut self.sound_cache,
                    game_files.as_ref(),
                ));
            }
        }
        // Logged after the loop, once world's borrow above has ended. self.log
        // needs &mut self, which would conflict with the still-live `world`
        // borrow if called from inside the loop.
        for msg in sound_messages {
            self.log(msg);
        }
        // A session that ended by itself (the server went away, say): back
        // to the menu, which is told why.
        if let Some(reason) = self.game_session.as_ref().and_then(|s| s.finished()) {
            self.end_game_session();
            if let Some(menu) = self.game_menu.as_mut() {
                menu.session_ended(&reason);
            }
            if !self.menu_open {
                self.open_game_menu();
            }
            self.log(format!("Left the session: {reason}"));
        }
        // A script asked for another scene: swap to it now the tick is done.
        if let Some(name) = self
            .game_world
            .as_mut()
            .and_then(|w| w.pending_scene.take())
        {
            self.switch_game_scene(&name);
        }
        let (width, height) = match &self.game_window {
            Some(w) => {
                let size = w.inner_size();
                (size.width.max(1), size.height.max(1))
            }
            None => return,
        };
        // The active Camera entity, if any (lowest id) -- found once and
        // reused both to exclude it from its own render below (a first-
        // person camera doesn't draw its own mesh around its own eye) and to
        // build the view itself.
        let active_camera_id = self
            .game_world
            .as_ref()
            .and_then(|world| find_camera_entity(world));
        // While a game's menu shows, the menu may move the camera itself.
        let menu_view = if self.menu_open {
            let seconds = self.menu_opened.elapsed().as_secs_f64();
            match (self.game_menu.as_mut(), self.game_world.as_ref()) {
                (Some(menu), Some(world)) => menu.view(world, seconds).filter(|v| {
                    let d = DVec3::from(v.target) - DVec3::from(v.eye);
                    v.eye.iter().chain(&v.target).all(|c| c.is_finite()) && d.length() > 1e-3
                }),
                _ => None,
            }
        } else {
            None
        };
        // Everything in Play is drawn relative to a point near what the player
        // sees: the menu's camera, the active Camera entity, or the editor's
        // own focus when the scene has none.
        let game_origin = match menu_view {
            Some(view) => origin::snap(DVec3::from(view.eye)),
            None => self
                .game_world
                .as_ref()
                .zip(active_camera_id)
                .and_then(|(world, id)| world.positions.get(id))
                .map(|p| origin::snap(DVec3::new(p.x, p.y, p.z)))
                .unwrap_or_else(|| {
                    origin::snap(DVec3::new(
                        self.cam_focus_x,
                        self.cam_focus_y,
                        self.cam_focus_z,
                    ))
                }),
        };
        // Where Play is looking from, for modules that stream detail around
        // the viewer: the menu's camera, the active Camera entity, or the
        // editor's own focus. A Camera that is itself a character looks
        // from its eyes, not the middle of its body.
        let game_viewer = match menu_view {
            Some(view) => view.eye,
            None => self
                .game_world
                .as_ref()
                .zip(active_camera_id)
                .and_then(|(world, id)| {
                    systems::character_eyes(world, id)
                        .map(|(eye, _)| eye)
                        .or_else(|| world.positions.get(id).map(|p| [p.x, p.y, p.z]))
                })
                .unwrap_or([self.cam_focus_x, self.cam_focus_y, self.cam_focus_z]),
        };
        self.play_game_audio(menu_view, active_camera_id, game_viewer, game_files.as_ref());
        let (instances, group_counts, lights) = match &self.game_world {
            Some(world) => {
                let anchors = module_api::anchor_names(&self.modules);
                let empty = std::collections::HashSet::new();
                let (instances, group_counts) = build_instances(
                    world,
                    None,
                    &empty,
                    &self.game_custom_names,
                    &anchors,
                    active_camera_id,
                    game_origin,
                );
                // A module (a day-night clock, say) may move the sun.
                sun::set_module_sun(module_api::module_sun(&self.modules, world));
                sun::set_module_moon(module_api::module_moon(&self.modules, world));
                sun::set_module_wind(module_api::module_wind(&self.modules, world));
                set_view_reach(module_api::module_view_distance(&self.modules, world));
                set_tide(module_api::module_tide(&self.modules, world));
                let under = module_api::module_underwater(&self.modules, world, game_viewer);
                set_underwater(under, game_viewer[1]);
                // Under water, sound is muffled.
                if under.is_some() != self.muffled {
                    self.muffled = under.is_some();
                    if let Some(muffle) = self.audio_muffle.as_mut() {
                        let mix = if self.muffled {
                            kira::Mix::WET
                        } else {
                            kira::Mix::DRY
                        };
                        muffle.set_mix(
                            mix,
                            kira::Tween {
                                duration: UNDERWATER_EASE,
                                ..Default::default()
                            },
                        );
                    }
                }
                (instances, group_counts, build_lights(world, game_origin))
            }
            None => return,
        };
        // If the scene has a Camera entity, Play renders from it: what the
        // player actually sees. Otherwise, fall back to the editor's own
        // orbit camera, frozen at whatever it was showing when Play started
        // (the pre-existing behaviour, unchanged for a scene with no camera).
        let menu_proj = menu_view.map(|view| {
            let eye = origin::relative(DVec3::from(view.eye), game_origin);
            let target = origin::relative(DVec3::from(view.target), game_origin);
            let aspect = width as f32 / height.max(1) as f32;
            (camera_projection(aspect) * Mat4::look_at_rh(eye, target, Vec3::Y)).to_cols_array_2d()
        });
        let view_proj = menu_proj
            .or_else(|| {
                active_camera_id
                    .zip(self.game_world.as_ref())
                    .and_then(|(id, world)| {
                        camera_entity_view_proj(world, id, width, height, game_origin)
                    })
            })
            .unwrap_or_else(|| {
                let focus = (DVec3::new(self.cam_focus_x, self.cam_focus_y, self.cam_focus_z)
                    - game_origin)
                    .as_vec3();
                camera_view_proj(
                    focus.x,
                    focus.y,
                    focus.z,
                    self.cam_distance,
                    self.cam_yaw,
                    self.cam_pitch,
                    width,
                    height,
                )
            });
        let (egui_paint_jobs, egui_textures_delta, egui_ppp) = self.render_game_ui();
        let module_scenes = self
            .game_world
            .as_ref()
            .map(|world| {
                let eye = self.viewer_world();
                module_api::module_scenes(
                    &self.modules,
                    world,
                    false,
                    &[game_viewer, [eye.x, eye.y, eye.z]],
                )
            })
            .unwrap_or_default();
        if let Some(gpu) = self.game_gpu.as_mut() {
            gpu.sync_modules(&module_scenes, game_origin);
            gpu.render(
                &instances,
                &group_counts,
                &[],
                view_proj,
                &lights,
                &egui_paint_jobs,
                &egui_textures_delta,
                egui_ppp,
            );
        }
        self.play_perf.end(std::time::Instant::now());
        if let Some(window) = &self.game_window {
            window.request_redraw();
        }
    }

    /// Ask every module (and the game's menu) what should be heard this
    /// frame, from where Play is drawn, and play it (see `game_audio`).
    fn play_game_audio(
        &mut self,
        menu_view: Option<MenuView>,
        camera: Option<usize>,
        position: [f64; 3],
        files: Option<&ProjectFiles>,
    ) {
        let now = std::time::Instant::now();
        let dt = self
            .game_audio_at
            .map_or(0.0, |t| now.duration_since(t).as_secs_f32())
            .min(0.25);
        self.game_audio_at = Some(now);
        // The listener's right, from whatever Play draws from: the menu's
        // camera, the Camera entity (a character's eyes if it rides one), or
        // the editor's own orbit camera.
        let forward = match (menu_view, camera.zip(self.game_world.as_ref())) {
            (Some(view), _) => (DVec3::from(view.target) - DVec3::from(view.eye)).as_vec3(),
            (None, Some((id, world))) => {
                let rotation = match frame_engine::systems::character_eyes(world, id) {
                    Some((_, look)) => frame_engine::world::Rotation {
                        yaw: look.yaw,
                        pitch: look.pitch,
                        roll: 0.0,
                    },
                    None => world.rotations.get(id).copied().unwrap_or_default(),
                };
                Vec3::from(rotation.forward())
            }
            (None, None) => view_forward(self.cam_yaw, self.cam_pitch),
        };
        let right = forward.cross(Vec3::Y).normalize_or(Vec3::X);
        let listener = module_api::Listener {
            position,
            right: right.to_array(),
            dt,
            in_menu: self.menu_open,
        };
        let mut frame = module_api::AudioFrame::default();
        if let Some(world) = self.game_world.as_ref() {
            module_api::module_audio(&self.modules, world, &listener, &mut frame);
        }
        if let Some(menu) = self.game_menu.as_mut() {
            for sound in menu.take_sounds() {
                frame.play(&sound, 1.0, 1.0);
            }
        }
        let messages = self.game_audio.apply(
            frame,
            &listener,
            self.audio_manager.as_mut(),
            &mut self.sound_cache,
            files,
        );
        for message in messages {
            self.log(message);
        }
    }

    /// The Play overlay's lines (F3 in Play): the frame rate and times,
    /// memory, the simulation's tick rate, how many entities there are and
    /// where the player is, then each module's own lines
    /// (`EditorModule::play_stats`). Short, for reading over the game.
    fn play_stats_lines(&self) -> Vec<String> {
        let mut lines = self.play_perf.frame_lines("game");
        lines.extend(perf::memory_line());
        lines.push(format!("{TICK_RATE} ticks a second"));
        if let Some(world) = self.game_world.as_ref() {
            let entities = world.positions.iter().filter(|p| p.is_some()).count();
            lines.push(format!("entities {entities}"));
            if let Some(p) = find_camera_entity(world).and_then(|id| world.positions.get(id)) {
                lines.push(format!("at {:.1}, {:.1}, {:.1}", p.x, p.y, p.z));
            }
            lines.extend(module_api::module_play_stats(&self.modules, world));
        }
        lines
    }

    /// Build this frame's egui overlay for the Play window: every UiText and
    /// UiImage entity in `game_world`, drawn as a screen-space panel
    /// positioned by its normalized x/y, instead of as 3D geometry (see
    /// build_instances, which skips these same entities for exactly that
    /// reason). Not interactive yet -- v1 is display only -- so every Area
    /// below is `.interactable(false)`, just enough for render_game to pass
    /// real paint jobs into GpuState::render instead of the empty stand-ins
    /// it used before this existed. A game's own menu (`GameMenu`), while it
    /// shows, is drawn here too, and is interactive. Returns the empty
    /// defaults if there's no
    /// game window/egui state yet (Play isn't running).
    fn render_game_ui(&mut self) -> (Vec<egui::ClippedPrimitive>, egui::TexturesDelta, f32) {
        let (Some(window), Some(ctx)) = (self.game_window.clone(), self.game_egui_ctx.clone())
        else {
            return (Vec::new(), egui::TexturesDelta::default(), 1.0);
        };
        let stats = self.show_play_stats.then(|| self.play_stats_lines());
        let Some(state) = self.game_egui_state.as_mut() else {
            return (Vec::new(), egui::TexturesDelta::default(), 1.0);
        };
        let raw_input = state.take_egui_input(&window);
        // Resolve any not-yet-seen UiImage's texture before entering the
        // egui closure below, the same lazily-cached-on-first-use shape as
        // `thumbnails`/`texture_cache` -- the closure only reads
        // `game_ui_textures`, it never decodes a file itself.
        if let (Some(world), Some(files)) = (self.game_world.as_ref(), self.game_files.as_ref()) {
            let names: Vec<String> = world
                .ui_images
                .iter()
                .flatten()
                .map(|image| image.name.clone())
                .filter(|name| !name.is_empty() && !self.game_ui_textures.contains_key(name))
                .collect();
            for name in names {
                let Some(path) = files.find_by_name("assets", &name) else {
                    continue;
                };
                let Ok(bytes) = files.read(&path) else {
                    continue;
                };
                let Ok(decoded) = image::load_from_memory(&bytes) else {
                    continue;
                };
                let rgba = decoded.to_rgba8();
                let (w, h) = rgba.dimensions();
                let color_image = egui::ColorImage::from_rgba_unmultiplied(
                    [w as usize, h as usize],
                    &rgba.into_raw(),
                );
                let handle = ctx.load_texture(
                    format!("ui-image-{name}"),
                    color_image,
                    egui::TextureOptions::LINEAR,
                );
                self.game_ui_textures.insert(name, handle);
            }
        }
        // The game's menu, while it shows: taken out for the frame so it can
        // draw while the world is borrowed, and put back after.
        let mut menu = if self.menu_open {
            self.game_menu.take()
        } else {
            None
        };
        let mut settings = GameSettings {
            look_sensitivity: self.look_sensitivity,
            invert_look_y: self.invert_look_y,
            volume: self.game_volume,
        };
        let mut action = GameMenuAction::Stay;
        let Some(world) = self.game_world.as_ref() else {
            self.game_menu = self.game_menu.take().or(menu);
            return (Vec::new(), egui::TexturesDelta::default(), 1.0);
        };
        // The game's own on-screen text and images are hidden behind its
        // menu.
        let (texts, images): (
            Vec<frame_engine::world::UiText>,
            Vec<frame_engine::world::UiImage>,
        ) = if menu.is_some() {
            (Vec::new(), Vec::new())
        } else {
            (
                world.ui_texts.iter().flatten().cloned().collect(),
                world.ui_images.iter().flatten().cloned().collect(),
            )
        };
        let textures = &self.game_ui_textures;
        // What to tell the player while the mouse is free: how to start, and
        // why nothing moves when nothing in the scene is Controlled.
        // A game with its own menu goes back to it on the second Esc.
        let has_menu = self.game_menu.is_some();
        let panels_open = self.play_panels_open;
        let has_panels = self.modules.borrow().iter().any(|m| m.has_play_panel());
        let hint = (!self.game_mouse_held && menu.is_none() && !panels_open).then(|| {
            if world.controlled.iter().flatten().next().is_none() {
                "Nothing in this scene is Controlled: tick Controlled on your \
                 character in the editor and save (Ctrl+S). Esc closes."
                    .to_string()
            } else if has_menu {
                "Click to play: the mouse looks around, WASD moves, Shift runs, \
                 C crouches, Space jumps, E picks up or puts down, Q uses the other \
                 hand, the mouse buttons use what you hold, F3 shows the numbers. Esc frees the mouse, Esc again opens the menu."
                    .to_string()
                    + if has_panels { " F1 opens the game's panel." } else { "" }
            } else {
                "Click to play: the mouse looks around, WASD moves, Shift runs, \
                 C crouches, Space jumps, E picks up or puts down, Q uses the other \
                 hand, the mouse buttons use what you hold, F3 shows the numbers. Esc frees the mouse, Esc again closes."
                    .to_string()
                    + if has_panels { " F1 opens the game's panel." } else { "" }
            }
        });
        let modules = &self.modules;
        let aiming = self.game_mouse_held;
        let full_output = ctx.run_ui(raw_input, |ui| {
            let screen = ui.max_rect();
            if aiming {
                // A small dot in the middle while the mouse looks around:
                // where the view, and a held tool, points.
                let painter = ui.ctx().layer_painter(egui::LayerId::new(
                    egui::Order::Foreground,
                    egui::Id::new("play-crosshair"),
                ));
                let centre = screen.center();
                painter.circle_filled(centre, 3.0, egui::Color32::from_black_alpha(140));
                painter.circle_filled(centre, 1.8, egui::Color32::from_white_alpha(230));
            }
            if let Some(stats) = &stats {
                egui::Area::new(egui::Id::new("play-stats"))
                    .anchor(egui::Align2::LEFT_TOP, egui::vec2(8.0, 8.0))
                    .order(egui::Order::Foreground)
                    .interactable(false)
                    .show(ui.ctx(), |ui| {
                        egui::Frame::new()
                            .fill(egui::Color32::from_black_alpha(160))
                            .inner_margin(egui::Margin::symmetric(10, 6))
                            .corner_radius(4.0)
                            .show(ui, |ui| {
                                for line in stats {
                                    ui.label(
                                        egui::RichText::new(line)
                                            .monospace()
                                            .size(13.0)
                                            .color(egui::Color32::WHITE),
                                    );
                                }
                            });
                    });
            }
            if let Some(hint) = &hint {
                egui::Area::new(egui::Id::new("play-hint"))
                    .anchor(egui::Align2::CENTER_BOTTOM, egui::vec2(0.0, -24.0))
                    .order(egui::Order::Foreground)
                    .interactable(false)
                    .show(ui.ctx(), |ui| {
                        egui::Frame::new()
                            .fill(egui::Color32::from_black_alpha(160))
                            .inner_margin(egui::Margin::symmetric(12, 8))
                            .corner_radius(6.0)
                            .show(ui, |ui| {
                                ui.label(
                                    egui::RichText::new(hint)
                                        .size(15.0)
                                        .color(egui::Color32::WHITE),
                                );
                            });
                    });
            }
            for text in &texts {
                if text.text.is_empty() {
                    continue;
                }
                let pos = egui::pos2(
                    screen.min.x + text.x * screen.width(),
                    screen.min.y + text.y * screen.height(),
                );
                let color = egui::Color32::from_rgb(
                    (text.color.r.clamp(0.0, 1.0) * 255.0) as u8,
                    (text.color.g.clamp(0.0, 1.0) * 255.0) as u8,
                    (text.color.b.clamp(0.0, 1.0) * 255.0) as u8,
                );
                egui::Area::new(egui::Id::new((
                    "ui-text",
                    text.x.to_bits(),
                    text.y.to_bits(),
                    &text.text,
                )))
                .fixed_pos(pos)
                .order(egui::Order::Foreground)
                .interactable(false)
                .show(ui.ctx(), |ui| {
                    ui.label(
                        egui::RichText::new(&text.text)
                            .size(text.font_size)
                            .color(color),
                    );
                });
            }
            for image in &images {
                let Some(handle) = textures.get(&image.name) else {
                    continue;
                };
                let size = egui::vec2(image.width * screen.width(), image.height * screen.height());
                let pos = egui::pos2(
                    screen.min.x + image.x * screen.width(),
                    screen.min.y + image.y * screen.height(),
                );
                egui::Area::new(egui::Id::new((
                    "ui-image",
                    image.x.to_bits(),
                    image.y.to_bits(),
                    &image.name,
                )))
                .fixed_pos(pos)
                .order(egui::Order::Foreground)
                .interactable(false)
                .show(ui.ctx(), |ui| {
                    ui.add(egui::Image::new(handle).fit_to_exact_size(size));
                });
            }
            if panels_open && menu.is_none() {
                for module in modules.borrow_mut().iter_mut() {
                    module.play_panel(ui.ctx(), world);
                }
            }
            if let Some(menu) = menu.as_mut() {
                action = menu.show(ui.ctx(), world, &mut settings);
            }
        });
        state.handle_platform_output(&window, full_output.platform_output);
        if menu.is_some() {
            self.game_menu = menu;
            let settings = settings.sanitized();
            self.look_sensitivity = settings.look_sensitivity;
            self.invert_look_y = settings.invert_look_y;
            self.game_volume = settings.volume;
        }
        match action {
            GameMenuAction::Stay => {}
            GameMenuAction::Play => {
                // The menu's session, if it has one; one already running
                // carries on.
                if self.game_session.is_none() {
                    self.game_session = self.game_menu.as_mut().and_then(|m| m.take_session());
                }
                // Straight into play: the click on the menu's button was the
                // click that takes the mouse.
                self.menu_open = false;
                grab_cursor(&window, true);
                self.game_mouse_held = true;
            }
            GameMenuAction::Leave => self.end_game_session(),
            GameMenuAction::Quit => self.game_closing = true,
        }
        let ppp = full_output.pixels_per_point;
        let jobs = ctx.tessellate(full_output.shapes, ppp);
        (jobs, full_output.textures_delta, ppp)
    }

    /// End the session being played, if any, letting it tidy the world, and
    /// start local physics afresh: the session moved things physics was not
    /// told about.
    fn end_game_session(&mut self) {
        if let Some(mut session) = self.game_session.take() {
            if let Some(world) = self.game_world.as_mut() {
                session.end(world);
            }
            self.game_physics = Some(Physics::new(GRAVITY_Y));
        }
    }

    /// Open the modules' Play panels, letting go of the mouse and every held
    /// key, or close them and take the mouse back for looking around. Does
    /// nothing when no module has a panel.
    fn toggle_play_panels(&mut self) {
        if self.play_panels_open {
            self.play_panels_open = false;
            if let Some(window) = &self.game_window {
                grab_cursor(window, true);
                window.request_redraw();
            }
            self.game_mouse_held = true;
            return;
        }
        if !self.modules.borrow().iter().any(|m| m.has_play_panel()) {
            return;
        }
        self.release_game_mouse();
        self.game_input = InputState::new();
        self.play_panels_open = true;
    }

    /// Show the game's menu again over the running world, letting go of the
    /// mouse and every held key.
    fn open_game_menu(&mut self) {
        self.play_panels_open = false;
        self.release_game_mouse();
        self.game_input = InputState::new();
        self.menu_open = true;
        self.menu_opened = std::time::Instant::now();
    }

    /// Give the mouse back from the game window.
    fn release_game_mouse(&mut self) {
        if let Some(window) = &self.game_window {
            grab_cursor(window, false);
            window.request_redraw();
        }
        self.game_mouse_held = false;
        // A freed mouse holds no action.
        self.game_input.set(Button::Use, false);
        self.game_input.set(Button::UseAlt, false);
        self.game_input.set(Button::Interact, false);
        self.game_input.set(Button::Swap, false);
    }

    /// Handle an event addressed to the game window.
    fn game_window_event(&mut self, event: WindowEvent) {
        // Forwarded first, same as popped_out_window_event: the egui overlay
        // needs to see resizes and (eventually, once it's interactive) input
        // regardless of which arm below also reacts to this event.
        let window = self.game_window.clone();
        if let (Some(state), Some(window)) = (self.game_egui_state.as_mut(), window.as_ref()) {
            let _ = state.on_window_event(window, &event);
        }
        match event {
            WindowEvent::CloseRequested => self.game_closing = true,
            WindowEvent::Focused(focused) => {
                // Losing focus lets the mouse go and drops held keys. Getting
                // it back takes nothing: a click takes the mouse again.
                if !focused {
                    self.release_game_mouse();
                    self.game_input = InputState::new();
                }
            }
            // The menu takes the mouse for its own buttons (egui saw the
            // click above); nothing reaches the game.
            WindowEvent::MouseInput { .. } if self.menu_open => {}
            // So do the panels (F1).
            WindowEvent::MouseInput { .. } if self.play_panels_open => {}
            WindowEvent::MouseInput { state, button, .. } => {
                let pressed = state == ElementState::Pressed;
                if !self.game_mouse_held {
                    // A click in the window takes the mouse for looking
                    // around; that click does nothing else.
                    if pressed
                        && button == MouseButton::Left
                        && let Some(window) = &self.game_window
                    {
                        grab_cursor(window, true);
                        self.game_mouse_held = true;
                        window.request_redraw();
                    }
                } else {
                    // With the mouse held, its buttons are the actions: left
                    // the main one, right the second.
                    match button {
                        MouseButton::Left => self.game_input.set(Button::Use, pressed),
                        MouseButton::Right => self.game_input.set(Button::UseAlt, pressed),
                        _ => {}
                    }
                }
            }
            WindowEvent::Resized(size) => {
                if let Some(gpu) = self.game_gpu.as_mut() {
                    gpu.resize(size.width, size.height);
                }
            }
            WindowEvent::RedrawRequested => self.render_game(),
            WindowEvent::KeyboardInput { event, .. } => {
                let pressed = event.state == ElementState::Pressed;
                if let PhysicalKey::Code(code) = event.physical_key {
                    match code {
                        // F3: the Play overlay (frame rate, memory, where
                        // the player is, each module's numbers).
                        KeyCode::F3 if pressed && !event.repeat => {
                            self.show_play_stats = !self.show_play_stats;
                        }
                        // The menu's own keys are egui's; the game gets none.
                        _ if self.menu_open => {}
                        // F1: the modules' panels (see `play_panel`). The
                        // mouse is let go while they are open, and taken
                        // back for looking around when they close.
                        KeyCode::F1 if pressed && !event.repeat => self.toggle_play_panels(),
                        // Esc closes the panels, leaving the mouse free.
                        KeyCode::Escape if pressed && self.play_panels_open => {
                            self.play_panels_open = false;
                        }
                        // While they are open, keys are theirs (typing in a
                        // box), not the game's.
                        _ if self.play_panels_open => {}
                        // Esc lets the mouse go; pressed again, it brings
                        // back the game's menu, or closes a game without one.
                        KeyCode::Escape if pressed => {
                            if self.game_mouse_held {
                                self.release_game_mouse();
                            } else if self.game_menu.is_some() {
                                self.open_game_menu();
                            } else {
                                self.game_closing = true;
                            }
                        }
                        KeyCode::KeyW => self.game_input.set(Button::Up, pressed),
                        KeyCode::KeyA => self.game_input.set(Button::Left, pressed),
                        KeyCode::KeyS => self.game_input.set(Button::Down, pressed),
                        KeyCode::KeyD => self.game_input.set(Button::Right, pressed),
                        KeyCode::Space => self.game_input.set(Button::Jump, pressed),
                        KeyCode::ShiftLeft | KeyCode::ShiftRight => {
                            self.game_input.set(Button::Run, pressed)
                        }
                        KeyCode::ControlLeft | KeyCode::KeyC => {
                            self.game_input.set(Button::Crouch, pressed)
                        }
                        KeyCode::KeyE => self.game_input.set(Button::Interact, pressed),
                        KeyCode::KeyQ => self.game_input.set(Button::Swap, pressed),
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }

    /// Pop a docked tab out into its own real, separate OS window (draggable
    /// to another monitor), sharing this same running App/World the way the
    /// game window does. Mirrors start_play's shape: its own Window, its own
    /// GpuState. The Viewport tab is deliberately never offered this (see
    /// EditorTabViewer::context_menu) — it's the live 3D scene rendered
    /// behind egui, not a panel, and popping it out would need its own
    /// duplicated render target, a separate, bigger piece of work.
    fn pop_out_tab(&mut self, event_loop: &ActiveEventLoop, tab: Tab) {
        if matches!(tab, Tab::Viewport) || self.popped_out.contains_key(&tab) {
            return;
        }
        let Some(path) = self.dock_state.find_tab(&tab) else {
            return; // not currently docked — already popped out, or gone
        };
        self.dock_state.remove_tab(path);
        let part = match tab {
            Tab::Scene => "Scene",
            Tab::Inspector => "Inspector",
            Tab::Scripts => "Script Editor",
            Tab::Source => "Source Control",
            Tab::Viewport => unreachable!("Viewport is never popped out"),
        };
        let title = format!("{} — {part}", app_name());
        let attributes = Window::default_attributes().with_title(title);
        let window = match event_loop.create_window(attributes) {
            Ok(w) => Arc::new(w),
            Err(e) => {
                self.log(format!("Could not open a window for this tab: {e}"));
                // Put it back where it came from rather than losing the tab.
                self.dock_state.push_to_first_leaf(tab);
                return;
            }
        };
        // Reuses the main window's shared Instance/Device -- see SharedGpu's
        // own doc comment for why this window must not create its own.
        let (gpu, _shared) = GpuState::new(
            window.clone(),
            self.gpu_shared.as_ref(),
            &self.modules.borrow(),
        );
        let egui_ctx = egui::Context::default();
        apply_editor_theme(&egui_ctx);
        let egui_state = egui_winit::State::new(
            egui_ctx.clone(),
            egui::ViewportId::ROOT,
            window.as_ref(),
            None,
            None,
            None,
        );
        window.request_redraw();
        self.popped_out.insert(
            tab,
            PoppedOutWindow {
                window,
                gpu,
                egui_ctx,
                egui_state,
            },
        );
    }

    /// Close a popped-out tab's window and dock it back into the main one.
    /// Called from about_to_wait via popped_out_closing, not directly from
    /// event dispatch — dropping a wgpu surface from inside its own
    /// CloseRequested handler is the same Wayland segfault close_game's own
    /// doc comment warns about.
    fn dock_back_tab(&mut self, tab: Tab) {
        if let Some(popped) = self.popped_out.remove(&tab) {
            // Explicit, not just relying on field declaration order: gpu's
            // wgpu surface holds a handle into window, so it must go first --
            // same ordering as close_game's own teardown.
            let PoppedOutWindow {
                gpu,
                window,
                egui_ctx,
                egui_state,
            } = popped;
            // This window rendered and presented a frame as recently as this
            // same tick (the "Dock back" click itself is drawn and presented
            // before we ever get here). Presentation is not necessarily
            // finished just because present() returned -- on Wayland in
            // particular, destroying the surface/device before the compositor
            // has actually acknowledged that last frame is a well-known way
            // to segfault in the driver rather than in our own code. Block
            // until the GPU has genuinely finished everything queued for
            // this device before tearing anything down.
            let _ = gpu.device.poll(wgpu::PollType::Wait {
                submission_index: None,
                timeout: None,
            });
            drop(gpu);
            drop(window);
            drop(egui_ctx);
            drop(egui_state);
            self.dock_state.push_to_first_leaf(tab);
        }
    }

    /// Draw and present one popped-out tab's own window this frame, reusing
    /// the very same EditorTabViewer the main dock just drew from, so both
    /// stay in perfect sync — an edit in a popped-out Inspector and one in a
    /// still-docked tab this same frame both land in the caller's one
    /// drain-back afterwards, rather than each window keeping its own,
    /// possibly conflicting, snapshot of the world.
    fn render_popped_out(&mut self, tab: Tab, viewer: &mut EditorTabViewer) {
        let mut dock_back = false;
        let render_result = {
            let Some(popped) = self.popped_out.get_mut(&tab) else {
                return;
            };
            let window = popped.window.clone();
            let raw_input = popped.egui_state.take_egui_input(&window);
            let full_output = popped.egui_ctx.clone().run_ui(raw_input, |ui| {
                ui.horizontal(|ui| {
                    if ui.button("⟵ Dock back to main window").clicked() {
                        dock_back = true;
                    }
                });
                ui.separator();
                let mut tab_copy = tab;
                egui_dock::TabViewer::ui(viewer, ui, &mut tab_copy);
            });
            popped
                .egui_state
                .handle_platform_output(&window, full_output.platform_output);
            let ppp = full_output.pixels_per_point;
            let jobs = popped.egui_ctx.tessellate(full_output.shapes, ppp);
            (jobs, full_output.textures_delta, ppp)
        };
        let (egui_paint_jobs, egui_textures_delta, egui_ppp) = render_result;
        if let Some(popped) = self.popped_out.get_mut(&tab) {
            popped.gpu.render(
                &[],
                &[],
                &[],
                Mat4::IDENTITY.to_cols_array_2d(),
                &[LightRaw::zeroed(); MAX_LIGHTS],
                &egui_paint_jobs,
                &egui_textures_delta,
                egui_ppp,
            );
            popped.window.request_redraw();
        }
        if dock_back {
            self.popped_out_closing.push(tab);
        }
    }

    /// Handle an event addressed to one of the popped-out tab windows.
    fn popped_out_window_event(&mut self, tab: Tab, event: WindowEvent) {
        if let Some(popped) = self.popped_out.get_mut(&tab) {
            let _ = popped.egui_state.on_window_event(&popped.window, &event);
        }
        match event {
            WindowEvent::CloseRequested => self.popped_out_closing.push(tab),
            WindowEvent::Resized(size) => {
                if let Some(popped) = self.popped_out.get_mut(&tab) {
                    popped.gpu.resize(size.width, size.height);
                }
            }
            // The real draw happens once per frame, batched with the main
            // window's own redraw (see render_popped_out, called from its
            // RedrawRequested arm) so every window paints from the same
            // EditorTabViewer snapshot. Just make sure another frame is
            // coming if the window system wants this one repainted on its
            // own (e.g. after being uncovered on its own monitor).
            WindowEvent::RedrawRequested => {
                if let Some(popped) = self.popped_out.get(&tab) {
                    popped.window.request_redraw();
                }
            }
            _ => {}
        }
    }

    /// Switch from the launcher into the editor with a project loaded.
    // Rebuild the Assets tab previews from the loaded models.
    fn rebuild_thumbnails(&mut self) {
        self.thumbnails.clear();
        for (name, data) in &self.custom_meshes {
            let image = render_thumbnail(data);
            let handle = self.egui_ctx.load_texture(
                format!("thumb-{name}"),
                image,
                egui::TextureOptions::LINEAR,
            );
            self.thumbnails.insert(name.clone(), handle);
        }
    }

    // Import a model: pick an .obj, copy it into the project's assets folder,
    // parse it, record its half extents, and upload it to the GPU.
    /// File > Import audio: pick one or more audio files and import each.
    fn import_audio(&mut self) {
        if self.project_dir.is_none() {
            self.log("Open a project before importing audio".to_string());
            return;
        }
        let Some(picked) = rfd::FileDialog::new()
            .add_filter("Audio", AUDIO_EXTENSIONS)
            .pick_files()
        else {
            return; // dialog cancelled
        };
        for path in picked {
            self.import_audio_from(path);
        }
    }

    /// Import one audio file into the folder currently open in the Assets
    /// tab (the assets root by default), so it lands wherever the user is
    /// working. The file is decoded once first, so a broken or unsupported
    /// file is caught here with a clear message rather than failing silently
    /// the first time something tries to play it. A Sound finds its file by
    /// name anywhere under assets/ (see `find_audio`), so if a file with the
    /// same name already exists somewhere in there, that file is replaced in
    /// place instead of adding a second copy the lookup can't tell apart.
    fn import_audio_from(&mut self, picked: std::path::PathBuf) {
        let Some(assets) = self.project_dir.as_deref().map(|p| p.join("assets")) else {
            self.log("Open a project before importing audio".to_string());
            return;
        };
        let Some(name) = picked
            .file_name()
            .and_then(|f| f.to_str())
            .map(String::from)
        else {
            return;
        };
        if let Err(e) = kira::sound::static_sound::StaticSoundData::from_file(&picked) {
            self.log(format!("Could not read audio '{name}': {e:?}"));
            return;
        }
        let existing = find_audio(&assets, &name);
        let dest = match &existing {
            Some(path) => path.clone(),
            None => {
                let folder = assets.join(&self.assets_subdir);
                let folder = if folder.is_dir() {
                    folder
                } else {
                    assets.clone()
                };
                if let Err(e) = std::fs::create_dir_all(&folder) {
                    self.log(format!("Could not create assets folder: {e}"));
                    return;
                }
                folder.join(&name)
            }
        };
        // Copying onto itself fails on some platforms, so skip when the
        // picked file already is the destination.
        if picked != dest {
            if let Err(e) = std::fs::copy(&picked, &dest) {
                self.log(format!("Could not copy audio into assets: {e}"));
                return;
            }
        }
        // Drop any cached decode of the old file, so the next play uses the
        // new one. The cache is keyed by whatever a Sound's name says, which
        // may be an older path form, so match on the file name part too.
        self.sound_cache
            .retain(|key, _| key.rsplit(['/', '\\']).next() != Some(name.as_str()));
        let shown = dest
            .strip_prefix(&assets)
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| name.clone());
        if existing.is_some() {
            self.log(format!("Replaced audio 'assets/{shown}'"));
        } else {
            self.log(format!("Imported audio 'assets/{shown}'"));
        }
    }

    /// File > Import texture: pick one or more image files and import each,
    /// the same "one at a time, shared per-file work factored out" shape as
    /// Import audio.
    fn import_texture(&mut self) {
        if self.project_dir.is_none() {
            self.log("Open a project before importing a texture".to_string());
            return;
        }
        let Some(picked) = rfd::FileDialog::new()
            .add_filter("Image", IMAGE_EXTENSIONS)
            .pick_files()
        else {
            return; // dialog cancelled
        };
        for path in picked {
            self.import_texture_from(path);
        }
    }

    /// Import one image file into the folder currently open in the Assets
    /// tab, the same "decode once to catch a broken file early, land wherever
    /// the user is working, replace an existing same-name file in place"
    /// shape `import_audio_from` already uses for audio. A texture is found
    /// by name anywhere under assets/ (see `find_image`), matching how a
    /// Sound or a Mesh::Custom resolves its own file.
    fn import_texture_from(&mut self, picked: std::path::PathBuf) {
        let Some(assets) = self.project_dir.as_deref().map(|p| p.join("assets")) else {
            self.log("Open a project before importing a texture".to_string());
            return;
        };
        let Some(name) = picked
            .file_name()
            .and_then(|f| f.to_str())
            .map(String::from)
        else {
            return;
        };
        if let Err(e) = image::open(&picked) {
            self.log(format!("Could not read image '{name}': {e:?}"));
            return;
        }
        let existing = find_image(&assets, &name);
        let dest = match &existing {
            Some(path) => path.clone(),
            None => {
                let folder = assets.join(&self.assets_subdir);
                let folder = if folder.is_dir() {
                    folder
                } else {
                    assets.clone()
                };
                if let Err(e) = std::fs::create_dir_all(&folder) {
                    self.log(format!("Could not create assets folder: {e}"));
                    return;
                }
                folder.join(&name)
            }
        };
        if picked != dest {
            if let Err(e) = std::fs::copy(&picked, &dest) {
                self.log(format!("Could not copy image into assets: {e}"));
                return;
            }
        }
        // Drop any cached decode of the old file, same reasoning
        // import_audio_from's sound_cache invalidation already documents.
        self.texture_cache
            .retain(|key, _| key.rsplit(['/', '\\']).next() != Some(name.as_str()));
        // Any mesh whose material already points at this exact file name
        // needs its bind group rebuilt with the new pixels, not just its
        // cache entry dropped.
        self.refresh_custom_mesh_gpu();
        let shown = dest
            .strip_prefix(&assets)
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| name.clone());
        if existing.is_some() {
            self.log(format!("Replaced image 'assets/{shown}'"));
        } else {
            self.log(format!("Imported image 'assets/{shown}'"));
        }
    }

    /// Import dropped files: .obj as a model, a supported audio format as
    /// audio, a supported image format as a texture, anything else skipped
    /// with a message.
    fn import_dropped(&mut self, path: std::path::PathBuf) {
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase())
            .unwrap_or_default();
        if ext == "obj" {
            self.import_model_from(path);
        } else if AUDIO_EXTENSIONS.contains(&ext.as_str()) {
            self.import_audio_from(path);
        } else if IMAGE_EXTENSIONS.contains(&ext.as_str()) {
            self.import_texture_from(path);
        } else {
            let name = path
                .file_name()
                .map(|f| f.to_string_lossy().to_string())
                .unwrap_or_default();
            self.log(format!(
                "Can't import '{name}': not a model, a supported audio file, or a supported image"
            ));
        }
    }

    fn import_model(&mut self) {
        if self.project_dir.is_none() {
            self.log("Open a project before importing a model".to_string());
            return;
        }
        let Some(picked) = rfd::FileDialog::new()
            .add_filter("OBJ model", &["obj"])
            .pick_file()
        else {
            return; // dialog cancelled
        };
        self.import_model_from(picked);
    }

    /// Import one .obj into the project's assets folder. Shared by the File
    /// menu and drag and drop.
    /// Rebuild every custom mesh's GPU material bind group from whatever's
    /// currently in `self.world.mesh_meta`, decoding any newly-referenced
    /// texture through `self.texture_cache` (so an already-decoded one is
    /// never re-read). Called after anything that can change what a mesh
    /// should look like: importing/reimporting a model, reloading a project,
    /// or editing a material in the Assets tab.
    fn refresh_custom_mesh_gpu(&mut self) {
        let custom_names: Vec<String> = self.custom_meshes.keys().cloned().collect();
        let refs: Vec<&frame_engine::assets::MeshData> = self.custom_meshes.values().collect();
        let project = self
            .project_dir
            .as_deref()
            .map(|p| ProjectFiles::Folder(p.to_path_buf()));
        let materials = build_material_gpu_data(
            &custom_names,
            &self.world.mesh_meta,
            project.as_ref(),
            &mut self.texture_cache,
        );
        let pairs: Vec<(&frame_engine::assets::MeshData, &MaterialGpuData)> =
            refs.iter().copied().zip(materials.iter()).collect();
        if let Some(gpu) = &mut self.gpu {
            gpu.set_custom_meshes(&pairs);
        }
    }
    fn import_model_from(&mut self, picked: std::path::PathBuf) {
        let Some(root) = self.project_dir.as_deref().map(|p| p.to_path_buf()) else {
            self.log("Open a project before importing a model".to_string());
            return;
        };
        let Some(file_name) = picked.file_name().map(|f| f.to_owned()) else {
            return;
        };
        let Some(name) = picked
            .file_stem()
            .and_then(|st| st.to_str())
            .map(String::from)
        else {
            return;
        };
        let text = match std::fs::read_to_string(&picked) {
            Ok(t) => t,
            Err(e) => {
                self.log(format!("Could not read model: {e}"));
                return;
            }
        };
        let data = match frame_engine::assets::parse_obj(&text) {
            Ok(d) => d,
            Err(e) => {
                self.log(format!("Could not parse model: {e}"));
                return;
            }
        };
        let assets = root.join("assets");
        if let Err(e) = std::fs::create_dir_all(&assets) {
            self.log(format!("Could not create assets folder: {e}"));
            return;
        }
        let dest = assets.join(&file_name);
        // Copying onto itself fails on some platforms, so skip when the picked
        // file is already in the assets folder.
        if picked != dest {
            if let Err(e) = std::fs::copy(&picked, &dest) {
                self.log(format!("Could not copy model into assets: {e}"));
                return;
            }
        }
        let replaced = self.custom_meshes.contains_key(&name);
        // Re-importing a model (replacing it) must not silently reset a
        // material already assigned to it, so the existing entry's material
        // is carried over rather than defaulted; only half_extents actually
        // needs recomputing from the freshly parsed geometry.
        let material = self
            .world
            .mesh_meta
            .get(&name)
            .map(|m| m.material.clone())
            .unwrap_or_default();
        self.world.mesh_meta.insert(
            name.clone(),
            frame_engine::world::MeshMeta {
                half_extents: data.half_extents,
                material,
            },
        );
        self.custom_meshes.insert(name.clone(), data);
        self.refresh_custom_mesh_gpu();
        self.rebuild_thumbnails();
        if replaced {
            self.log(format!("Replaced model '{name}'"));
        } else {
            self.log(format!("Imported model '{name}'"));
        }
    }

    // Parse the open project's models, record their half extents in the
    // world, and upload them to the editor GPU.
    fn load_project_assets(&mut self) {
        self.custom_meshes.clear();
        if let Some(root) = self.project_dir.as_deref().map(|p| p.to_path_buf()) {
            let (models, errors) = load_project_models(&root);
            for e in errors {
                self.log(format!("Model load failed: {e}"));
            }
            self.custom_meshes = models;

            let (manifests, scripts, errors) = load_project_plugins(&root);
            for e in errors {
                self.log(format!("Plugins: {e}"));
            }
            if !manifests.is_empty() {
                self.log(format!("Found {} plugin(s)", manifests.len()));
            }
            merge_plugins_into_world(&mut self.world, manifests, scripts);

            let (prefabs, errors) = load_project_prefabs(&root);
            for e in errors {
                self.log(format!("Prefabs: {e}"));
            }
            if !prefabs.is_empty() {
                self.log(format!("Found {} prefab(s)", prefabs.len()));
            }
            self.prefabs = prefabs;
        }
        for (name, data) in &self.custom_meshes {
            // A project reload must not reset a mesh's already-assigned
            // material back to default (same reasoning as import_model_from
            // above): carry over whatever's already in mesh_meta for this
            // name, recomputing only half_extents from the freshly parsed
            // geometry.
            let material = self
                .world
                .mesh_meta
                .get(name)
                .map(|m| m.material.clone())
                .unwrap_or_default();
            self.world.mesh_meta.insert(
                name.clone(),
                frame_engine::world::MeshMeta {
                    half_extents: data.half_extents,
                    material,
                },
            );
        }
        if !self.custom_meshes.is_empty() {
            self.log(format!("Loaded {} model(s)", self.custom_meshes.len()));
        }
        self.refresh_custom_mesh_gpu();
        self.rebuild_thumbnails();
    }

    /// Turn one plugin on or off. Enabling re-scans that plugin's scripts
    /// folder and merges them into script_library; disabling removes every
    /// script currently in the library under that plugin's prefix. An
    /// entity that has one of those scripts assigned when it's disabled
    /// keeps a dangling reference, the same safe handling a deleted
    /// hand-written script already gets: the runtime skips it, the
    /// Inspector flags it.
    fn set_plugin_enabled(&mut self, plugin_name: &str, enabled: bool) {
        if let Some(installed) = self.world.installed_plugins.get_mut(plugin_name) {
            installed.enabled = enabled;
        }
        self.dirty = true;
        let prefix = format!("{plugin_name}/");
        if enabled {
            if let Some(root) = self.project_dir.as_deref().map(|p| p.to_path_buf()) {
                let (_, scripts, errors) = load_project_plugins(&root);
                for e in errors {
                    self.log(format!("Plugins: {e}"));
                }
                for (name, source) in scripts {
                    if name.starts_with(&prefix) {
                        self.world.script_library.insert(name, source);
                    }
                }
            }
        } else {
            self.world
                .script_library
                .retain(|k, _| !k.starts_with(&prefix));
        }
        self.log(format!(
            "{} plugin '{plugin_name}'",
            if enabled { "Enabled" } else { "Disabled" }
        ));
    }

    /// Run a clicked plugin action. `PluginActionKind` is a closed enum, so
    /// this match is the complete list of everything a plugin action can
    /// ever cause; there is no path from here into arbitrary plugin code.
    fn run_plugin_action(&mut self, kind: frame_engine::world::PluginActionKind) {
        match kind {
            frame_engine::world::PluginActionKind::RunScript { script } => {
                // Every entity currently assigned this exact script, run
                // once, right now, through the same ScriptRuntime::run a
                // normal tick already uses for it. Shared per-tick state
                // (such as held input) reflects whatever the last real tick
                // set, not a fresh begin_tick, since this isn't a tick of
                // its own.
                let ids: Vec<usize> = self
                    .world
                    .scripts
                    .iter()
                    .enumerate()
                    .filter_map(|(id, slot)| slot.as_ref().filter(|s| s.uses == script).map(|_| id))
                    .collect();
                if ids.is_empty() {
                    self.log(format!("No entity currently uses script '{script}'"));
                    return;
                }
                for id in &ids {
                    self.script_runtime.run(&mut self.world, *id);
                }
                self.log(format!(
                    "Ran '{script}' once for {} entit{}",
                    ids.len(),
                    if ids.len() == 1 { "y" } else { "ies" }
                ));
            }
            frame_engine::world::PluginActionKind::ToggleValue { name } => {
                // Every live entity that already has a value under this
                // name; toggling can't originate one on an entity that
                // doesn't, the same rule fields and scripts both follow.
                let ids: Vec<usize> = self
                    .world
                    .positions
                    .iter()
                    .enumerate()
                    .filter_map(|(id, slot)| slot.as_ref().map(|_| id))
                    .collect();
                let mut toggled = 0usize;
                for id in ids {
                    if let Some(value) = self.world.get_dynamic::<f64>(&name, id).copied() {
                        let new_value = if value > 0.5 { 0.0 } else { 1.0 };
                        self.world.insert_dynamic::<f64>(&name, id, new_value);
                        toggled += 1;
                    }
                }
                self.log(format!(
                    "Toggled '{name}' on {toggled} entit{}",
                    if toggled == 1 { "y" } else { "ies" }
                ));
            }
            frame_engine::world::PluginActionKind::ToggleGlobal { name } => {
                let current = self.world.globals.get(&name).copied().unwrap_or(0.0);
                let new_value = if current > 0.5 { 0.0 } else { 1.0 };
                self.world.globals.insert(name.clone(), new_value);
                self.log(format!("Toggled global '{name}'"));
            }
        }
    }

    fn enter_editor(&mut self, root: std::path::PathBuf, scene_path: std::path::PathBuf) {
        let name = project_display_name(&root);
        self.project_dir = Some(root);
        self.current_scene_path = Some(scene_path);
        self.selected = None;
        // Open paused: arrange the scene first, press P when ready to run.
        self.paused = true;
        self.mode = AppMode::Editor;
        if let Some(window) = &self.window {
            window.set_title(&format!("{} — {name}", app_name()));
        }
        self.log(format!("Opened project '{name}'"));
        self.project_name = Some(name);
        self.load_project_assets();
        // Whatever loading the project's plugins merged in counts as the
        // starting point, not as an unsaved edit.
        self.window_title.clear();
        self.mark_saved();
    }
    /// The point everything is drawn relative to this frame: the camera's
    /// position (the free-camera eye while flying, the orbit focus otherwise),
    /// snapped to a grid. See `origin`.
    fn view_origin(&self) -> DVec3 {
        let reference = if self.fly_mode {
            self.cam_eye
        } else {
            DVec3::new(self.cam_focus_x, self.cam_focus_y, self.cam_focus_z)
        };
        origin::snap(reference)
    }
    /// Where the viewport camera really is, in world space (`f64`): the
    /// free-camera eye while flying, or the orbit eye otherwise.
    fn viewer_world(&self) -> DVec3 {
        if self.fly_mode {
            self.cam_eye
        } else {
            let offset = DVec3::new(
                (self.cam_pitch.cos() * self.cam_yaw.sin()) as f64,
                self.cam_pitch.sin() as f64,
                (self.cam_pitch.cos() * self.cam_yaw.cos()) as f64,
            ) * self.cam_distance as f64;
            DVec3::new(self.cam_focus_x, self.cam_focus_y, self.cam_focus_z) + offset
        }
    }
    /// Where Play is looking from, in world space, while Play is running: the
    /// active Camera entity, or the editor's own focus when the scene has none.
    fn game_viewer_world(&self) -> Option<[f64; 3]> {
        let world = self.game_world.as_ref()?;
        Some(
            find_camera_entity(world)
                .and_then(|id| {
                    // A character camera sees from its eyes.
                    systems::character_eyes(world, id)
                        .map(|(eye, _)| eye)
                        .or_else(|| world.positions.get(id).map(|p| [p.x, p.y, p.z]))
                })
                .unwrap_or([self.cam_focus_x, self.cam_focus_y, self.cam_focus_z]),
        )
    }
    /// A world position as drawn this frame: relative to the render origin.
    fn rel(&self, p: &Position) -> Vec3 {
        origin::relative(DVec3::new(p.x, p.y, p.z), self.view_origin())
    }
    /// The current view-projection matrix, in render-origin space: an orbit
    /// around the focus normally, or a free camera from `cam_eye` while flying.
    fn view_matrix(&self, width: u32, height: u32) -> Mat4 {
        let origin = self.view_origin();
        if self.fly_mode {
            let aspect = width as f32 / height.max(1) as f32;
            let forward = view_forward(self.cam_yaw, self.cam_pitch);
            let eye = (self.cam_eye - origin).as_vec3();
            let view = Mat4::look_at_rh(eye, eye + forward, Vec3::Y);
            let proj = camera_projection(aspect);
            proj * view
        } else {
            let focus = (DVec3::new(self.cam_focus_x, self.cam_focus_y, self.cam_focus_z) - origin)
                .as_vec3();
            camera_matrix(
                focus.x,
                focus.y,
                focus.z,
                self.cam_distance,
                self.cam_yaw,
                self.cam_pitch,
                width,
                height,
            )
        }
    }
    /// Where the camera actually is, in render-origin space: the free-camera
    /// eye while flying, or the orbit eye (focus plus the orbit offset)
    /// otherwise.
    fn camera_eye(&self) -> Vec3 {
        let origin = self.view_origin();
        if self.fly_mode {
            (self.cam_eye - origin).as_vec3()
        } else {
            let offset = Vec3::new(
                self.cam_pitch.cos() * self.cam_yaw.sin(),
                self.cam_pitch.sin(),
                self.cam_pitch.cos() * self.cam_yaw.cos(),
            ) * self.cam_distance;
            (DVec3::new(self.cam_focus_x, self.cam_focus_y, self.cam_focus_z) - origin).as_vec3()
                + offset
        }
    }

    /// Recompute the gizmo's screen geometry for the current selection and camera.
    /// Arms are sized as a fraction of the camera-to-entity distance, so the gizmo
    /// stays about the same size on screen however far away the entity is. Also
    /// refreshes which arm the cursor is hovering.
    fn update_gizmo(&mut self, width: u32, height: u32) {
        self.gizmo = None;
        // No gizmo while flying: the cursor is locked, so there's nothing to grab.
        if self.fly_mode {
            self.gizmo_hover = None;
            return;
        }
        let Some(id) = self.selected else {
            self.gizmo_hover = None;
            return;
        };
        let Some(p) = self.world.positions.get(id).copied() else {
            self.gizmo_hover = None;
            return;
        };
        let (w, h) = (width as f32, height as f32);
        let eye = self.camera_eye();
        let pr = self.rel(&p);
        let dist = (pr - eye).length();
        let len = (dist * GIZMO_SCREEN_FRAC).max(1.0);
        let vp = self.view_matrix(width, height);
        let origin = project(vp, pr.x, pr.y, pr.z, w, h);
        let ex = project(vp, pr.x + len, pr.y, pr.z, w, h);
        let ey = project(vp, pr.x, pr.y + len, pr.z, w, h);
        let ez = project(vp, pr.x, pr.y, pr.z + len, w, h);
        if let (Some(origin), Some(ex), Some(ey), Some(ez)) = (origin, ex, ey, ez) {
            self.gizmo = Some(GizmoScreen {
                origin,
                ends: [ex, ey, ez],
                len,
            });
        }
        // Hover follows the cursor unless a drag is already in progress.
        self.gizmo_hover = if self.gizmo_drag.is_some() {
            self.gizmo_drag
        } else {
            self.gizmo_hit()
        };
    }

    /// Which gizmo arm, if any, the cursor is close enough to grab. Nearest wins,
    /// so crossing arms behave sensibly.
    fn gizmo_hit(&self) -> Option<usize> {
        let g = self.gizmo?;
        let cursor = (self.last_cursor.0 as f32, self.last_cursor.1 as f32);
        let mut best: Option<(usize, f32)> = None;
        for (axis, end) in g.ends.iter().enumerate() {
            let d = dist_to_segment(cursor, g.origin, *end);
            let better = match best {
                None => true,
                Some((_, bd)) => d < bd,
            };
            if d <= GIZMO_PICK_PX && better {
                best = Some((axis, d));
            }
        }
        best.map(|(axis, _)| axis)
    }

    fn pick(&mut self) {
        let (width_u, height_u) = match &self.window {
            Some(window) => {
                let size = window.inner_size();
                (size.width.max(1), size.height.max(1))
            }
            None => return,
        };
        let width = width_u as f32;
        let height = height_u as f32;
        let vp = self.view_matrix(width_u, height_u);
        let pick_origin = self.view_origin();
        // While flying the cursor is locked, so aim from screen centre (a
        // crosshair): look at an entity and click to pick it. Otherwise use the
        // cursor.
        let (cursor_x, cursor_y) = if self.fly_mode {
            (width * 0.5, height * 0.5)
        } else {
            (self.last_cursor.0 as f32, self.last_cursor.1 as f32)
        };
        let mut picked: Option<usize> = None;
        for (id, slot) in self.world.positions.iter().enumerate() {
            if let Some(p) = slot {
                // Hit-box grows with the entity's scale so picking matches what's drawn.
                let scale = self.world.scales.get(id).copied().unwrap_or_default();
                let half_x = QUAD_SIZE * 0.5 * scale.x;
                let half_y = QUAD_SIZE * 0.5 * scale.y;
                let pr = origin::relative(DVec3::new(p.x, p.y, p.z), pick_origin);
                let center = project(vp, pr.x, pr.y, pr.z, width, height);
                let corner = project(vp, pr.x + half_x, pr.y + half_y, pr.z, width, height);
                if let (Some((cx, cy)), Some((ex, ey))) = (center, corner) {
                    let half_w = (ex - cx).abs();
                    let half_h = (ey - cy).abs();
                    if (cursor_x - cx).abs() <= half_w && (cursor_y - cy).abs() <= half_h {
                        picked = Some(id);
                    }
                }
            }
        }
        if let Some(id) = picked {
            self.selected = Some(id);
            // Picking while flying makes that entity the orbit pivot for when we
            // drop back to orbit.
            if self.fly_mode {
                if let Some(p) = self.world.positions.get(id) {
                    self.cam_focus_x = p.x;
                    self.cam_focus_y = p.y;
                    self.cam_focus_z = p.z;
                    self.fly_picked = true;
                }
            }
            if let (Some(p), Some(v)) =
                (self.world.positions.get(id), self.world.velocities.get(id))
            {
                println!(
                    "Selected entity {} | pos ({:.1}, {:.1}, {:.1}) | vel ({:.2}, {:.2}, {:.2})",
                    id, p.x, p.y, p.z, v.dx, v.dy, v.dz,
                );
            }
        }
    }
}
// Decode the embedded logo into a winit window icon (shown in the title bar and
// the OS taskbar). Returns None if it can't decode, so the window still opens.
// NOTE: honoured on X11 and Windows; Wayland ignores it and takes the taskbar
// icon from a matching .desktop file instead.
fn load_window_icon() -> Option<Icon> {
    let rgba = image::load_from_memory(LOGO_PNG).ok()?.to_rgba8();
    let (width, height) = rgba.dimensions();
    Icon::from_rgba(rgba.into_raw(), width, height).ok()
}
impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        // A game on its own opens only the game window.
        if matches!(self.mode, AppMode::Player) {
            if self.game_window.is_none() {
                if let Some(files) = self.player_files.clone() {
                    self.start_play(event_loop, files, None);
                }
                if self.game_window.is_none() {
                    // What went wrong was logged; a player on Windows has no
                    // console to see it in, so it goes in the message too.
                    let reasons: Vec<&str> = self
                        .log_lines
                        .iter()
                        .rev()
                        .take(3)
                        .rev()
                        .map(String::as_str)
                        .collect();
                    report_player_error(
                        app_name(),
                        &format!("{} could not start.\n\n{}", app_name(), reasons.join("\n")),
                    );
                    self.player_failed = true;
                    event_loop.exit();
                }
            }
            return;
        }
        apply_editor_theme(&self.egui_ctx);
        #[allow(unused_mut)]
        let mut attributes = Window::default_attributes()
            .with_title(app_name())
            .with_window_icon(load_window_icon());
        // Wayland ignores the in-process icon above; it matches this app-id to a
        // frame-editor.desktop file and reads the taskbar icon from there.
        #[cfg(target_os = "linux")]
        {
            use winit::platform::wayland::WindowAttributesExtWayland;
            attributes = attributes.with_name(app_id(), app_id());
        }
        let window = Arc::new(event_loop.create_window(attributes).unwrap());
        let (gpu, shared) = GpuState::new(
            window.clone(),
            self.gpu_shared.as_ref(),
            &self.modules.borrow(),
        );
        self.gpu = Some(gpu);
        self.gpu_shared = Some(shared);
        // egui input state. Lives here because it needs the window; the egui
        // Context it shares already exists on App.
        let egui_state = egui_winit::State::new(
            self.egui_ctx.clone(),
            egui::ViewportId::ROOT,
            window.as_ref(),
            None,
            None,
            None,
        );
        self.egui_state = Some(egui_state);
        window.request_redraw();
        self.window = Some(window);
    }
    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        // An idle editor sleeps until its next wake time (see RedrawRequested).
        match self.idle_wake {
            Some(at) if std::time::Instant::now() >= at => {
                self.idle_wake = None;
                if let Some(window) = &self.window {
                    window.request_redraw();
                }
                event_loop.set_control_flow(winit::event_loop::ControlFlow::Wait);
            }
            Some(at) => {
                event_loop.set_control_flow(winit::event_loop::ControlFlow::WaitUntil(at));
            }
            None => event_loop.set_control_flow(winit::event_loop::ControlFlow::Wait),
        }
        // Tear down a closing game window here, after all events for this cycle
        // are dispatched. Dropping the wgpu surface from inside the window's own
        // CloseRequested handler segfaults on Wayland; doing it here — outside
        // event dispatch, with the display still alive — is safe.
        if self.game_closing {
            self.close_game();
            self.game_closing = false;
            // A game on its own ends with its window.
            if matches!(self.mode, AppMode::Player) {
                event_loop.exit();
            }
        }
        // Same reasoning, same deferred-teardown pattern, for any popped-out
        // tab window that asked to close this cycle (its own X button, or its
        // "Dock back" button).
        for tab in std::mem::take(&mut self.popped_out_closing) {
            self.dock_back_tab(tab);
        }
    }
    fn device_event(
        &mut self,
        _event_loop: &ActiveEventLoop,
        _device_id: DeviceId,
        event: DeviceEvent,
    ) {
        // Raw mouse motion drives mouselook while flying. Raw motion (rather than
        // cursor position) keeps looking smooth with the cursor grabbed in place.
        if let DeviceEvent::MouseMotion { delta: (dx, dy) } = event {
            // In the Play window the mouse turns the characters' view: right
            // turns right, and up looks up unless inverted.
            if self.game_mouse_held
                && let Some(world) = self.game_world.as_mut()
            {
                let look_x =
                    LOOK_SENS * self.look_sensitivity * if self.invert_look_x { -1.0 } else { 1.0 };
                let look_y =
                    LOOK_SENS * self.look_sensitivity * if self.invert_look_y { -1.0 } else { 1.0 };
                systems::turn_view(world, dx as f32 * look_x, -(dy as f32) * look_y);
                if let Some(window) = &self.game_window {
                    window.request_redraw();
                }
            }
            if self.fly_mode {
                // view_forward points toward -sin(yaw), so a larger yaw looks
                // left. Subtract so moving the mouse right looks right.
                let look_x =
                    LOOK_SENS * self.look_sensitivity * if self.invert_look_x { -1.0 } else { 1.0 };
                let look_y =
                    LOOK_SENS * self.look_sensitivity * if self.invert_look_y { -1.0 } else { 1.0 };
                self.cam_yaw -= dx as f32 * look_x;
                // Un-inverted: moving the mouse down looks down (pitch increases).
                self.cam_pitch += dy as f32 * look_y;
                self.cam_pitch = self.cam_pitch.clamp(-1.4, 1.4);
                if let Some(window) = &self.window {
                    window.request_redraw();
                }
            }
        }
    }
    fn exiting(&mut self, _event_loop: &ActiveEventLoop) {
        // Release GPU and window resources here, while winit's platform
        // connection (the Wayland display) is still alive. If we let these drop
        // later, after the event loop has torn down, the wgpu surface's
        // destructor touches Wayland objects that are already gone, which is the
        // segfault on exit. Order matters: GPU state first (its surface holds a
        // handle to the window), then egui, then the window last.
        self.gpu = None;
        self.egui_state = None;
        self.window = None;
        self.game_gpu = None;
        self.game_world = None;
        self.game_window = None;
        self.popped_out.clear();
    }
    fn window_event(&mut self, event_loop: &ActiveEventLoop, id: WindowId, event: WindowEvent) {
        // Events addressed to the separate game window go to its own handler.
        if self.game_window.as_ref().is_some_and(|w| w.id() == id) {
            self.game_window_event(event);
            return;
        }
        // Same for a popped-out tab window.
        if let Some(tab) = self
            .popped_out
            .iter()
            .find(|(_, p)| p.window.id() == id)
            .map(|(tab, _)| *tab)
        {
            self.popped_out_window_event(tab, event);
            return;
        }
        // Anything that happens to the window (a key, the mouse, a resize) is
        // a reason to draw again. Drawing itself is not.
        if !matches!(event, WindowEvent::RedrawRequested) {
            if let Some(window) = &self.window {
                window.request_redraw();
            }
        }
        // Feed every event to egui so its own widgets (dragging the panel,
        // future buttons/sliders) keep working. We deliberately ignore the
        // returned `consumed` flag: in 0.35 it's driven by egui's *interaction*
        // state, which is the wrong question for a viewport editor and gates
        // press/release differently once a button goes down — that's what stuck
        // our drag/orbit state and killed viewport input.
        if let (Some(state), Some(window)) = (self.egui_state.as_mut(), self.window.as_ref()) {
            let _ = state.on_window_event(window, &event);
        }
        // The viewport is now an egui_dock tab, so is_pointer_over_egui() reads
        // true over it and would kill 3D input. Instead we route 3D input by the
        // viewport tab's own body rect: allow picking/orbit/zoom only when the
        // cursor is inside it. viewport_rect is in egui points; the winit cursor
        // is in physical pixels, so divide by pixels_per_point to compare. None
        // (viewport tab hidden behind another tab) means no 3D input.
        let over_viewport = self.viewport_rect.is_some_and(|r| {
            let ppp = self.egui_ctx.pixels_per_point();
            let p = egui::pos2(
                self.last_cursor.0 as f32 / ppp,
                self.last_cursor.1 as f32 / ppp,
            );
            r.contains(p)
        });
        let ui_wants_keys = self.egui_ctx.egui_wants_keyboard_input();
        match event {
            WindowEvent::CloseRequested => {
                println!("Close requested");
                self.request_guarded(GuardedAction::Quit, event_loop);
            }
            // One event per file when several are dropped at once. winit
            // 0.30 sends this on Windows, macOS and X11, but not on Wayland.
            WindowEvent::DroppedFile(path) => self.import_dropped(path),
            WindowEvent::Resized(size) => {
                if let Some(gpu) = &mut self.gpu {
                    gpu.resize(size.width, size.height);
                }
            }
            WindowEvent::ModifiersChanged(mods) => {
                self.ctrl_held = mods.state().control_key();
                self.shift_held = mods.state().shift_key();
            }
            WindowEvent::Focused(true) => {
                if matches!(self.mode, AppMode::Editor) {
                    self.pick_up_outside_script_edits();
                }
            }
            WindowEvent::Focused(false) => {
                // Losing focus (e.g. Alt+Tab) drops the fly key without a release
                // event, so leave fly mode here too and free the cursor.
                self.set_fly(false);
            }
            WindowEvent::MouseInput { state, button, .. } if over_viewport || self.fly_mode => {
                let pressed = state == ElementState::Pressed;
                match button {
                    MouseButton::Left => {
                        if pressed {
                            // A press on a gizmo arm starts a drag on that axis —
                            // it takes priority over both camera pan and picking,
                            // so grabbing a handle never reselects or moves the
                            // view. One undo step covers the whole drag.
                            if let Some(axis) = self.gizmo_hit() {
                                self.push_undo();
                                self.gizmo_drag = Some(axis);
                                self.gizmo_hover = Some(axis);
                            } else {
                                // No longer drives camera pan (Shift+Middle does
                                // that now); kept as plain drag-state tracking in
                                // case a future box-select wants it.
                                self.dragging = true;
                                self.pick();
                            }
                        } else {
                            self.dragging = false;
                            self.gizmo_drag = None;
                        }
                    }
                    // Middle button held = orbit; Shift+Middle held = pan instead.
                    MouseButton::Middle => {
                        if pressed {
                            if self.shift_held {
                                self.panning = true;
                            } else {
                                self.orbiting = true;
                            }
                        } else {
                            self.orbiting = false;
                            self.panning = false;
                        }
                    }
                    // Right button held = flythrough camera (mouselook plus WASD),
                    // seeded from the current orbit view. Held rather than a
                    // modifier key so it can't collide with a window manager's own
                    // Alt-drag bindings.
                    MouseButton::Right => {
                        self.set_fly(pressed);
                    }
                    _ => {}
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                let dx = (position.x - self.last_cursor.0) as f32;
                let dy = (position.y - self.last_cursor.1) as f32;
                if let (Some(axis), Some(g), Some(id)) =
                    (self.gizmo_drag, self.gizmo, self.selected)
                {
                    // Map the screen drag onto the world axis: measure how far the
                    // cursor moved *along* the arm's on-screen direction, as a
                    // fraction of that arm's screen length, then apply the same
                    // fraction of its world length. Screen and world stay in step,
                    // so the entity tracks the cursor at any camera angle.
                    let (ax, ay) = (g.ends[axis].0 - g.origin.0, g.ends[axis].1 - g.origin.1);
                    let len2 = ax * ax + ay * ay;
                    // An arm pointing nearly at the camera collapses to a dot on
                    // screen; dragging it would be meaningless, so ignore it.
                    if len2 > 1.0 {
                        let t = (dx * ax + dy * ay) / len2;
                        let step = t * g.len;
                        if let Some(p) = self.world.positions.get_mut(id) {
                            match axis {
                                0 => p.x += f64::from(step),
                                1 => p.y += f64::from(step),
                                _ => p.z += f64::from(step),
                            }
                        }
                    }
                } else if self.orbiting {
                    // Sweep the orbit. Drag right -> swing around; drag up ->
                    // rise over the top. Pitch is clamped just short of the
                    // poles so the up vector never degenerates.
                    self.cam_yaw += dx * ORBIT_SENS * self.orbit_sensitivity;
                    self.cam_pitch -= dy * ORBIT_SENS * self.orbit_sensitivity;
                    self.cam_pitch = self.cam_pitch.clamp(-1.4, 1.4);
                } else if self.panning {
                    if let Some(window) = &self.window {
                        let height_px = window.inner_size().height.max(1) as f32;
                        let visible_world_height =
                            2.0 * self.cam_distance * (FOV_DEGREES.to_radians() * 0.5).tan();
                        let world_per_px = visible_world_height / height_px;
                        self.cam_focus_x -= f64::from(dx * world_per_px);
                        self.cam_focus_y += f64::from(dy * world_per_px);
                    }
                }
                self.last_cursor = (position.x, position.y);
            }
            WindowEvent::MouseWheel { delta, .. } if over_viewport || self.fly_mode => {
                let scroll = match delta {
                    MouseScrollDelta::LineDelta(_, y) => y,
                    MouseScrollDelta::PixelDelta(pos) => pos.y as f32,
                };
                // While flying, scroll adjusts the flythrough speed instead of
                // zooming the orbit camera (there's no orbit distance to zoom
                // while flying). The chosen speed sticks for the rest of the
                // hold and carries over into the next flight too.
                if self.fly_mode {
                    if scroll > 0.0 {
                        self.fly_speed *= 1.1;
                    } else if scroll < 0.0 {
                        self.fly_speed /= 1.1;
                    }
                    self.fly_speed = self.fly_speed.clamp(0.1, 200.0);
                } else {
                    if scroll > 0.0 {
                        self.cam_distance /= 1.1;
                    } else if scroll < 0.0 {
                        self.cam_distance *= 1.1;
                    }
                    self.cam_distance = self.cam_distance.clamp(10.0, 2000.0);
                }
            }
            WindowEvent::KeyboardInput { event, .. } if matches!(self.mode, AppMode::Editor) => {
                // Level-triggered movement input (WASD). Updated on both press
                // and release so the input system always sees what is held right
                // now. Tracked even when egui holds keyboard focus — otherwise
                // ticking the Controlled checkbox (which keeps focus) would
                // silently swallow WASD and the entity could never be driven.
                // The edge-triggered editor actions below still defer to egui
                // via `ui_wants_keys`.
                if let PhysicalKey::Code(code) = event.physical_key {
                    let pressed = event.state == ElementState::Pressed;
                    match code {
                        KeyCode::KeyW => self.input.set(Button::Up, pressed),
                        KeyCode::KeyA => self.input.set(Button::Left, pressed),
                        // Ctrl+S is Save, not "move down".
                        KeyCode::KeyS if !(pressed && self.ctrl_held) => {
                            self.input.set(Button::Down, pressed)
                        }
                        KeyCode::KeyD => self.input.set(Button::Right, pressed),
                        KeyCode::Space => self.input.set(Button::Jump, pressed),
                        KeyCode::ShiftLeft | KeyCode::ShiftRight => {
                            self.input.set(Button::Run, pressed)
                        }
                        KeyCode::KeyC => self.input.set(Button::Crouch, pressed),
                        _ => {}
                    }
                }
                // Ctrl+S saves, even while a text field (the Script Editor,
                // say) has focus, since that's exactly when you most want it.
                if self.ctrl_held
                    && event.state == ElementState::Pressed
                    && !event.repeat
                    && matches!(self.mode, AppMode::Editor)
                    && event.physical_key == PhysicalKey::Code(KeyCode::KeyS)
                {
                    self.save_scene();
                }
                if !ui_wants_keys && event.state == ElementState::Pressed {
                    if let PhysicalKey::Code(code) = event.physical_key {
                        // Undo / redo (Ctrl+Z, Ctrl+Y or Ctrl+Shift+Z). Fires only
                        // when no egui widget holds focus, so it doesn't fight a
                        // text field's own editing.
                        if self.ctrl_held && !event.repeat {
                            match code {
                                KeyCode::KeyZ if self.shift_held => self.redo(),
                                KeyCode::KeyZ => self.undo(),
                                KeyCode::KeyY => self.redo(),
                                _ => {}
                            }
                        }
                        // Position nudge — moves the selected entity along a world
                        // axis. Runs on auto-repeat too, so holding a key glides.
                        let nudge = match code {
                            KeyCode::ArrowLeft => Some((-EDIT_STEP, 0.0, 0.0)),
                            KeyCode::ArrowRight => Some((EDIT_STEP, 0.0, 0.0)),
                            KeyCode::ArrowUp => Some((0.0, EDIT_STEP, 0.0)),
                            KeyCode::ArrowDown => Some((0.0, -EDIT_STEP, 0.0)),
                            KeyCode::PageUp => Some((0.0, 0.0, EDIT_STEP)),
                            KeyCode::PageDown => Some((0.0, 0.0, -EDIT_STEP)),
                            _ => None,
                        };
                        if let Some((dx, dy, dz)) = nudge {
                            if let Some(id) = self.selected {
                                // One undo step per press-and-hold: snapshot on the
                                // first press, not on each auto-repeat.
                                if !event.repeat {
                                    self.push_undo();
                                }
                                if let Some(p) = self.world.positions.get_mut(id) {
                                    p.x += f64::from(dx);
                                    p.y += f64::from(dy);
                                    p.z += f64::from(dz);
                                }
                            }
                        } else if !event.repeat {
                            // One-shot actions — fire once per fresh press (no
                            // auto-repeat). Each calls the same method the menus do.
                            match code {
                                // P, not Space: Space is Jump.
                                KeyCode::KeyP => self.toggle_pause(),
                                // Period steps the sim one tick while paused.
                                // Kept off S so it doesn't collide with WASD.
                                KeyCode::Period => self.step_once(),
                                KeyCode::Escape => self.clear_selection(),
                                // H: toggle the controls overlay.
                                KeyCode::KeyH => self.toggle_help(),
                                // F3: toggle the performance overlay.
                                KeyCode::F3 => self.toggle_stats(),
                                // N: spawn a new entity at the camera focus, and select it.
                                KeyCode::KeyN => self.spawn_at_focus(),
                                // Delete: despawn the selected entity.
                                KeyCode::Delete => self.despawn_selected(),
                                // F5: save the current world to disk.
                                KeyCode::F5 => self.save_scene(),
                                // F9: reload the world from disk, discarding the current one.
                                KeyCode::F9 => {
                                    self.request_guarded(GuardedAction::ReloadScene, event_loop)
                                }
                                _ => {}
                            }
                        }
                    }
                }
            }
            WindowEvent::RedrawRequested => {
                // On the launcher screen there's no simulation and no 3D — draw
                // just the launcher UI and stop.
                if matches!(self.mode, AppMode::Launcher) {
                    self.draw_launcher(event_loop);
                    return;
                }
                let now = std::time::Instant::now();
                // Water moves while the world does.
                if let Some(last) = self.wave_clock {
                    if !self.paused || self.game_window.is_some() {
                        advance_waves(now.saturating_duration_since(last).as_secs_f32());
                    }
                }
                self.wave_clock = Some(now);
                self.perf.begin(now);
                // Window title: the project name, with " *" while there are
                // unsaved changes.
                if let (Some(name), Some(window)) =
                    (self.project_name.as_ref(), self.window.as_ref())
                {
                    let scene = self.current_scene_name().unwrap_or_default();
                    let title = format!(
                        "{} — {name} — {scene}{}",
                        app_name(),
                        if self.is_dirty() { " *" } else { "" }
                    );
                    if title != self.window_title {
                        window.set_title(&title);
                        self.window_title = title;
                    }
                }
                // Flythrough: while flying (right mouse held over the viewport),
                // WASD moves the camera through the scene along the direction it's
                // looking. Uses the actual view vectors, so it's correct at any
                // angle. While playing, WASD instead drives Controlled entities.
                if self.fly_mode {
                    let forward = view_forward(self.cam_yaw, self.cam_pitch);
                    let right = forward.cross(Vec3::Y).normalize_or_zero();
                    let mut mv = Vec3::ZERO;
                    if self.input.is_held(Button::Up) {
                        mv += forward;
                    }
                    if self.input.is_held(Button::Down) {
                        mv -= forward;
                    }
                    if self.input.is_held(Button::Right) {
                        mv += right;
                    }
                    if self.input.is_held(Button::Left) {
                        mv -= right;
                    }
                    self.cam_eye += (mv.normalize_or_zero() * self.fly_speed).as_dvec3();
                }
                let owed = self.clock.advance(!self.paused);
                // Same folder the Assets tab and imported models resolve
                // against, computed here too since the tick loop runs before
                // that local is set up further down.
                let tick_project = self
                    .project_dir
                    .as_deref()
                    .map(|p| ProjectFiles::Folder(p.to_path_buf()));
                let mut sound_messages = Vec::new();
                for _ in 0..owed {
                    // Detection runs first each tick, so a script can read whether
                    // its entity is colliding *this* tick (via the `hit` variable)
                    // and react before movement is applied.
                    systems::collision(&mut self.world);
                    // While flying, WASD moves the camera, not Controlled entities.
                    if !self.fly_mode {
                        systems::input_movement(&mut self.world, &self.input);
                    }
                    // Scripts run after input, not before: a script can see
                    // (and override, per entity, by touching
                    // move_dx/move_dz/move_jump) whatever the built-in
                    // Controlled input already asked for this tick, rather
                    // than the keyboard unconditionally overwriting whatever
                    // the script just set.
                    systems::run_scripts(&mut self.world, &mut self.script_runtime, &self.input);
                    module_api::tick_modules(
                        &self.modules,
                        &mut self.world,
                        1.0 / TICK_RATE as f32,
                    );
                    systems::gravity(&mut self.world);
                    systems::movement(&mut self.world);
                    module_api::sync_collision(&self.modules, &self.world, &mut self.physics);
                    self.physics.step(&mut self.world, 1.0 / TICK_RATE as f32);
                    systems::resolve_collisions(&mut self.world);
                    // Last, so a mounted camera (or anything else riding on
                    // a Parent) sees this tick's real final position.
                    systems::apply_parenting(&mut self.world);
                    sound_messages.extend(update_sounds(
                        &mut self.world,
                        self.audio_manager.as_mut(),
                        &mut self.sound_cache,
                        tick_project.as_ref(),
                    ));
                }
                // Logged after the loop ends, once self.world's borrow above
                // is done. self.log needs &mut self, which would conflict
                // with a still-live world borrow from inside the loop.
                for msg in sound_messages {
                    self.log(msg);
                }
                // Scenes only change in a game window; here the editor stays
                // on the scene being edited.
                if let Some(name) = self.world.pending_scene.take() {
                    self.log(format!(
                        "A script asked for scene '{name}' (load_scene). Scenes change in Play scene, not in the editor's viewport."
                    ));
                }
                // Refresh collisions once more for the editor's red tint. Inside
                // the loop, detection ran at each tick's start (before that tick's
                // movement); this recomputes it at the final, on-screen positions,
                // and also keeps the tint live while paused (when the loop above
                // doesn't run at all) or while dragging entities around.
                systems::collision(&mut self.world);
                let colliding: std::collections::HashSet<usize> = self
                    .world
                    .collisions
                    .iter()
                    .flat_map(|&(a, b, _)| [a, b])
                    .collect();
                let selected = self.selected;
                // Per-primitive instance buckets from the world (see build_instances).
                let custom_names: Vec<String> = self.custom_meshes.keys().cloned().collect();
                let anchors = module_api::anchor_names(&self.modules);
                let view_origin = self.view_origin();
                let (instances, group_counts) = build_instances(
                    &self.world,
                    selected,
                    &colliding,
                    &custom_names,
                    &anchors,
                    None,
                    view_origin,
                );
                sun::set_module_sun(module_api::module_sun(&self.modules, &self.world));
                sun::set_module_moon(module_api::module_moon(&self.modules, &self.world));
                sun::set_module_wind(module_api::module_wind(&self.modules, &self.world));
                set_view_reach(module_api::module_view_distance(&self.modules, &self.world));
                set_tide(module_api::module_tide(&self.modules, &self.world));
                let eye = self.viewer_world();
                set_underwater(
                    module_api::module_underwater(
                        &self.modules,
                        &self.world,
                        [eye.x, eye.y, eye.z],
                    ),
                    eye.y,
                );
                let lights = build_lights(&self.world, view_origin);
                // Features from editor modules. While a mouse button is held
                // (dragging an Inspector slider, say) they keep showing what
                // they had rather than rebuilding every frame.
                let module_scenes = {
                    let hold = self.egui_ctx.input(|i| i.pointer.any_down());
                    let eye = self.viewer_world();
                    let mut viewers = vec![[eye.x, eye.y, eye.z]];
                    // While Play runs its camera is a viewer too, so a module
                    // builds for both windows and neither waits on the other.
                    viewers.extend(self.game_viewer_world());
                    module_api::module_scenes(&self.modules, &self.world, hold, &viewers)
                };
                let (width, height) = match &self.window {
                    Some(window) => {
                        let size = window.inner_size();
                        (size.width, size.height)
                    }
                    None => (1, 1),
                };
                // The selected entity's ID/POS/VEL now live in the Inspector
                // panel, so the old top-left readout is retired. The controls
                // legend below is the only remaining hand-rolled overlay.
                let mut text_instances: Vec<TextInstance> = Vec::new();
                // Controls overlay (toggle with H), anchored bottom-left. Drawn
                // at a smaller pixel size than the inspector so it reads as
                // secondary furniture.
                if self.show_help {
                    let help = "CONTROLS   H TO HIDE\n\n\
                                                    P  PLAY PAUSE\n\n\
                                                    .  STEP WHEN PAUSED\n\n\
                                                    WASD  DRIVE OR FLY\n\n\
                                                    SPACE  JUMP\n\n\
                                                    N  SPAWN ENTITY\n\n\
                                                    DEL  DESPAWN SELECTED\n\n\
                                                    ARROWS  MOVE X Y\n\n\
                                                    PGUP PGDN  MOVE Z\n\n\
                                                    F5 SAVE   F9 LOAD\n\n\
                                                    ESC  DESELECT\n\n\
                                                    LMB  SELECT\n\n\
                                                    DRAG ARROWS  MOVE ENTITY\n\n\
                                                    MMB  DRAG ORBIT\n\n\
                                                    SHIFT MMB  DRAG PAN\n\n\
                                                    HOLD RMB  FLY CAM\n\n\
                                                    WHEEL  ZOOM   WHEEL WHILE FLYING  FLY SPEED";
                    let pixel = 2.0;
                    let lines = help.lines().count() as f32;
                    let line_h = (font::GLYPH_HEIGHT as f32 + 1.0) * pixel;
                    let start_y = height as f32 - lines * line_h - 16.0;
                    text_instances.extend(build_text(
                        help,
                        16.0,
                        start_y,
                        pixel,
                        width as f32,
                        height as f32,
                    ));
                }
                let view_proj = self.view_matrix(width, height).to_cols_array_2d();
                // Refresh the translate gizmo for the current selection and camera,
                // then convert it to egui points for the Viewport tab to paint.
                self.update_gizmo(width, height);
                // Refresh the Source Control snapshot every couple of seconds —
                // statuses() walks the working tree, so not every frame.
                if self.git_refresh_at.elapsed().as_secs_f32() > 2.0 {
                    self.git_refresh_at = std::time::Instant::now();
                    self.git_summary = self.project_dir.as_deref().and_then(git_summary);
                }
                let ppp = self.egui_ctx.pixels_per_point();
                let gizmo_draw = self.gizmo.map(|g| {
                    let to_pos = |p: (f32, f32)| egui::pos2(p.0 / ppp, p.1 / ppp);
                    GizmoDraw {
                        origin: to_pos(g.origin),
                        ends: [to_pos(g.ends[0]), to_pos(g.ends[1]), to_pos(g.ends[2])],
                        active: self.gizmo_hover,
                    }
                });
                // --- Run egui for this frame ---
                // run_ui hands our closure a full-screen root Ui and runs the
                // begin/end pass internally. Panels shown into that root dock to
                // the window edges. In egui 0.35 the old SidePanel/TopBottomPanel
                // types were merged into one `Panel` (Panel::top/right/bottom/...).
                // Panels are solid but resizable (drag their inner edge); the
                // tabs/content are still a mockup wired to nothing but text and
                // the live log. We move state in/out via locals so the closure
                // never has to borrow `self`.
                let mut console_tab = self.console_tab;
                let log_lines = std::mem::take(&mut self.log_lines);
                // Assets tab state, lifted so the closure stays off self.
                let assets_root = self.project_dir.as_deref().map(|p| p.join("assets"));
                let mut assets_subdir = std::mem::take(&mut self.assets_subdir);
                let mut new_asset_folder = std::mem::take(&mut self.new_asset_folder);
                let thumbnails = self.thumbnails.clone();
                let mut move_pending = std::mem::take(&mut self.move_pending);
                // Snapshot for the Assets tab's material panel; a real edit
                // is staged here and applied after the pass (writing it needs
                // self.world.mesh_meta and a GPU re-upload, neither of which
                // the egui closure can borrow), same lift-then-write-back
                // shape as prefab_spawn_request just below.
                let mesh_meta_snapshot = self.world.mesh_meta.clone();
                let image_names = assets_root.as_deref().map(list_images).unwrap_or_default();
                let mut material_editing = std::mem::take(&mut self.material_editing);
                let mut material_edit: Option<(String, frame_engine::world::MeshMaterial)> = None;
                // Snapshot for the Prefabs tab to read; spawning is staged
                // here and applied after the pass (spawn_prefab_at_focus
                // needs self.world and the camera focus point, neither of
                // which the egui closure can borrow), the same
                // lift-then-write-back pattern plugin_action already uses.
                let prefabs = self.prefabs.clone();
                // The Scenes tab's list is read from disk only while it shows.
                self.poll_build();
                let mut build_open = self.build_window_open;
                let mut build_settings_edit = self.build_settings.clone();
                // Only worked out while the window is open: it reads project.ron.
                // Taken before the script library is lifted out of the world
                // below, or the unsaved check would see it empty.
                let build_view = if build_open { self.build_view() } else { None };
                let scenes_view = (self.console_tab == ConsoleTab::Scenes)
                    .then(|| self.scenes_view())
                    .flatten();
                let mut scenes_tab = std::mem::take(&mut self.scenes_tab);
                let mut scene_action: Option<scenes::SceneAction> = None;
                let mut prefab_spawn_request: Option<String> = None;
                let mut new_prefab_name = std::mem::take(&mut self.new_prefab_name);
                let script_library = std::mem::take(&mut self.world.script_library);
                let new_script_name = std::mem::take(&mut self.new_script_name);
                let script_filter = std::mem::take(&mut self.script_filter);
                let open_script = std::mem::take(&mut self.open_script);
                let renaming = std::mem::take(&mut self.renaming);
                // Compile-check the open script once per frame, before the egui
                // pass (the runtime lives on `self`, which the egui closure can't
                // borrow). This reflects the source as of frame start; an edit
                // made this frame shows its result next frame — the same one-frame
                // path the inspector edits use. None = no script open.
                let script_status: Option<Result<(), script::ScriptError>> = open_script
                    .as_ref()
                    .and_then(|name| script_library.get(name))
                    .map(|src| self.script_runtime.check(src));
                // Semantic pass: names the script uses that the API doesn't define
                // (a typo). Only meaningful when the source parses, which `warnings`
                // enforces by returning nothing for unparseable source.
                let script_warnings: Vec<script::ScriptError> = open_script
                    .as_ref()
                    .and_then(|name| script_library.get(name))
                    .map(|src| self.script_runtime.warnings(src))
                    .unwrap_or_default();
                // Live entity ids for the Scene list, plus the selected entity's
                // position/velocity lifted into a local so the egui closure never
                // touches `self`. Any edits get written back into the world after
                // the pass; selection changes flow through `new_selection`.
                let entity_ids: Vec<usize> = self
                    .world
                    .positions
                    .iter()
                    .enumerate()
                    .filter_map(|(i, slot)| slot.as_ref().map(|_| i))
                    .collect();
                // For the Scene tab's tree view: each listed entity's own
                // Parent.entity, if it has one. Missing/dangling parents are
                // resolved against the live entity_ids list inside
                // scene_tab_ui itself, not here.
                let entity_parents: std::collections::HashMap<usize, usize> = entity_ids
                    .iter()
                    .filter_map(|&id| self.world.parents.get(id).map(|p| (id, p.entity)))
                    .collect();
                let new_selection = self.selected;
                let edited = self.selected.and_then(|id| {
                    Some(EditedEntity {
                        id,
                        pos: *self.world.positions.get(id)?,
                        vel: *self.world.velocities.get(id)?,
                        color: self.world.colors.get(id).copied().unwrap_or_default(),
                        controlled: self.world.controlled.get(id).is_some(),
                        scale: self.world.scales.get(id).copied().unwrap_or_default(),
                        material: self.world.materials.get(id).copied().unwrap_or_default(),
                        rotation: self.world.rotations.get(id).copied().unwrap_or_default(),
                        script_source: self.world.scripts.get(id).map(|s| s.uses.clone()),
                        mesh: self.world.meshes.get(id).cloned().unwrap_or_default(),
                        is_static: self.world.statics.get(id).is_some(),
                        has_gravity: self.world.gravities.get(id).is_some(),
                        is_rigid_body: self.world.rigid_bodies.get(id).is_some(),
                        light: self.world.lights.get(id).copied(),
                        sound: self.world.sounds.get(id).cloned(),
                        is_camera: self.world.cameras.get(id).is_some(),
                        parent: self.world.parents.get(id).copied(),
                        ui_text: self.world.ui_texts.get(id).cloned(),
                        ui_image: self.world.ui_images.get(id).cloned(),
                        character: self.world.characters.get(id).copied(),
                        ext: ExtEdit::from_world(&self.world, id),
                    })
                });
                // For each enabled plugin's declared fields, keep the ones
                // the selected entity actually has a value under. A field
                // can't originate a value it doesn't already have, the same
                // rule the custom_<name> script variables follow.
                let custom_fields: Vec<EditableCustomField> = match self.selected {
                    Some(id) => self
                        .world
                        .installed_plugins
                        .values()
                        .filter(|plugin| plugin.enabled)
                        .flat_map(|plugin| plugin.manifest.fields.iter())
                        .filter_map(|field| {
                            self.world.get_dynamic::<f64>(&field.name, id).map(|value| {
                                EditableCustomField {
                                    entity: id,
                                    name: field.name.clone(),
                                    label: field.label.clone(),
                                    value: *value,
                                    min: field.min,
                                    max: field.max,
                                }
                            })
                        })
                        .collect(),
                    None => Vec::new(),
                };
                // Upload the logo to the GPU on the first frame, then reuse the handle.
                if self.logo_texture.is_none() {
                    let rgba = image::load_from_memory(LOGO_PNG)
                        .expect("embedded logo PNG should decode")
                        .to_rgba8();
                    let (w, h) = rgba.dimensions();
                    let color_image = egui::ColorImage::from_rgba_unmultiplied(
                        [w as usize, h as usize],
                        rgba.as_raw(),
                    );
                    self.logo_texture = Some(self.egui_ctx.load_texture(
                        "frame-editor-logo",
                        color_image,
                        egui::TextureOptions::LINEAR,
                    ));
                }
                let logo = self.logo_texture.clone();
                let paused = self.paused;
                let mut plugins_panel_open = self.plugins_panel_open;
                let mut editor_settings_open = self.editor_settings_open;
                let mut build_action: Option<build::BuildUiAction> = None;
                let mut prefs_edit = self.current_prefs();
                let pending_unsaved = self.pending_unsaved;
                let notice = self.version_notice.clone();
                let mut dismiss_notice = false;
                let project_label = self
                    .project_name
                    .clone()
                    .unwrap_or_else(|| "this project".to_string());
                let mut unsaved_choice: Option<UnsavedChoice> = None;
                // A snapshot, not a live borrow: the closure below can't hold
                // a reference into self.world. A checkbox toggle is detected
                // here and applied after the pass via plugin_toggle, the same
                // lift-then-write-back pattern menu_action and delete use.
                let installed_plugins: Vec<(String, frame_engine::world::InstalledPlugin)> = self
                    .world
                    .installed_plugins
                    .iter()
                    .map(|(name, plugin)| (name.clone(), plugin.clone()))
                    .collect();
                let mut plugin_toggle: Option<(String, bool)> = None;
                let mut plugin_action: Option<frame_engine::world::PluginActionKind> = None;
                // A snapshot, edited in place by panel field sliders and
                // written back wholesale after the pass. Global values
                // aren't tied to any entity, so there's none of the
                // entity-capture care the per-entity custom fields needed,
                // there's nothing here that could end up applied to the
                // wrong target.
                let mut globals_edit = self.world.globals.clone();
                let mut menu_action: Option<MenuAction> = None;
                // Move the dock's per-frame state into the viewer, and lift the
                // dock layout off `self` (swapping in a throwaway) so the egui
                // closure can borrow neither. Both are drained back afterwards.
                // Snapshot of the selected entity's editable state before the UI
                // pass, so we can tell if the Inspector changed it this frame.
                let edited_before = edited.clone();
                let custom_fields_before = custom_fields.clone();
                let mut dock_state =
                    std::mem::replace(&mut self.dock_state, egui_dock::DockState::new(Vec::new()));
                let mut viewer = EditorTabViewer {
                    entity_ids,
                    entity_parents,
                    scene_reparent_request: None,
                    selection: new_selection,
                    edited,
                    custom_fields,
                    script_library,
                    new_script_name,
                    script_filter,
                    open_script,
                    renaming,
                    renamed: None,
                    script_status,
                    script_warnings,
                    custom_mesh_names: self.custom_meshes.keys().cloned().collect(),
                    assets_root: assets_root.clone(),
                    git_summary: std::mem::take(&mut self.git_summary),
                    // Reset each frame; the Viewport tab sets it if it's visible.
                    viewport_rect: None,
                    gizmo: gizmo_draw,
                    stats: self.show_stats.then(|| self.stats_lines()),
                    pop_out_request: None,
                    preview_texture_id: self.gpu.as_ref().map(|g| g.preview_texture_id),
                    new_prefab_name,
                    save_prefab_request: None,
                    modules: self.modules.clone(),
                };
                // How soon the UI wants to draw again (a blinking cursor, an
                // animation); `MAX` for not at all.
                let mut egui_delay = std::time::Duration::MAX;
                let (egui_paint_jobs, egui_textures_delta, egui_ppp) = if let (
                    Some(state),
                    Some(window),
                ) =
                    (self.egui_state.as_mut(), self.window.as_ref())
                {
                    let raw_input = state.take_egui_input(window);
                    let full_output = self.egui_ctx.run_ui(raw_input, |ui| {
                            // Thin wood-to-green strip across the very top: the
                            // Outerface V1 theme's signature accent from the mockup.
                            egui::Panel::top("theme_strip")
                                .exact_size(3.0)
                                .resizable(false)
                                .show_separator_line(false)
                                .frame(egui::Frame::NONE)
                                .show(ui, |ui| {
                                    let rect = ui.max_rect();
                                    let mut mesh = egui::Mesh::default();
                                    // colored_vertex pushes a vertex and returns
                                    // nothing; indices are push order: 0 tl, 1 bl,
                                    // 2 tr, 3 br.
                                    mesh.colored_vertex(rect.left_top(), theme::WOOD);
                                    mesh.colored_vertex(rect.left_bottom(), theme::WOOD);
                                    mesh.colored_vertex(rect.right_top(), theme::ACCENT);
                                    mesh.colored_vertex(rect.right_bottom(), theme::ACCENT);
                                    mesh.add_triangle(0, 1, 2);
                                    mesh.add_triangle(1, 3, 2);
                                    ui.painter().add(egui::Shape::mesh(mesh));
                                });
                            // Top toolbar strip — fixed height, placeholder menu.
                            egui::Panel::top("toolbar").resizable(false).show(ui, |ui| {
                                ui.horizontal(|ui| {
                                    if let Some(logo) = &logo {
                                        ui.add(
                                            egui::Image::new(logo)
                                                .fit_to_exact_size(egui::vec2(20.0, 20.0)),
                                        );
                                        ui.separator();
                                    }
                                    ui.menu_button("File", |ui| {
                                        if ui.button("Open scene…").clicked() {
                                            menu_action = Some(MenuAction::OpenScene);
                                        }
                                        if ui.button("Import model…").clicked() {
                                            menu_action = Some(MenuAction::ImportModel);
                                        }
                                        if ui.button("Import audio…").clicked() {
                                            menu_action = Some(MenuAction::ImportAudio);
                                        }
                                        if ui.button("Import texture…").clicked() {
                                            menu_action = Some(MenuAction::ImportTexture);
                                        }
                                        if build::game_builder().is_some() {
                                            ui.separator();
                                            if ui.button("Build Game…").clicked() {
                                                menu_action = Some(MenuAction::BuildGame);
                                            }
                                        }
                                        ui.separator();
                                        if ui.button("Save scene").clicked() {
                                            menu_action = Some(MenuAction::SaveScene);
                                        }
                                        if ui.button("Save scene as…").clicked() {
                                            menu_action = Some(MenuAction::SaveSceneAs);
                                        }
                                        if ui.button("Reload scene").clicked() {
                                            menu_action = Some(MenuAction::ReloadScene);
                                        }
                                        ui.separator();
                                        if ui.button("Close project").clicked() {
                                            menu_action = Some(MenuAction::CloseProject);
                                        }
                                        if ui.button("Quit").clicked() {
                                            menu_action = Some(MenuAction::Quit);
                                        }
                                    });
                                    ui.menu_button("Edit", |ui| {
                                        if ui.button("Undo").clicked() {
                                            menu_action = Some(MenuAction::Undo);
                                        }
                                        if ui.button("Redo").clicked() {
                                            menu_action = Some(MenuAction::Redo);
                                        }
                                        ui.separator();
                                        if ui.button("Spawn entity").clicked() {
                                            menu_action = Some(MenuAction::SpawnEntity);
                                        }
                                        if ui.button("Despawn selected").clicked() {
                                            menu_action = Some(MenuAction::DespawnSelected);
                                        }
                                        ui.separator();
                                        if ui.button("Clear selection").clicked() {
                                            menu_action = Some(MenuAction::ClearSelection);
                                        }
                                        ui.separator();
                                        if ui.button("Editor settings…").clicked() {
                                            menu_action = Some(MenuAction::OpenEditorSettings);
                                        }
                                    });
                                    ui.menu_button("View", |ui| {
                                        let play_pause = if paused { "Play" } else { "Pause" };
                                        if ui.button(play_pause).clicked() {
                                            menu_action = Some(MenuAction::TogglePause);
                                        }
                                        if ui.button("Step one tick").clicked() {
                                            menu_action = Some(MenuAction::StepOnce);
                                        }
                                        ui.separator();
                                        if ui.button("Controls overlay").clicked() {
                                            menu_action = Some(MenuAction::ToggleHelp);
                                        }
                                        if ui.button("Performance overlay (F3)").clicked() {
                                            menu_action = Some(MenuAction::ToggleStats);
                                        }
                                    });
                                    ui.menu_button("Help", |ui| {
                                        if ui.button("About").clicked() {
                                            menu_action = Some(MenuAction::About);
                                        }
                                    });
                                    // A toggle, not a dropdown, since clicking it
                                    // opens or closes the plugins panel directly
                                    // rather than showing a list of items.
                                    if ui
                                        .selectable_label(plugins_panel_open, "Plugins")
                                        .clicked()
                                    {
                                        plugins_panel_open = !plugins_panel_open;
                                    }
                                    // Opens the game window on the scene being
                                    // edited, as it is now (saved or not).
                                    // The View menu's Play only runs the
                                    // simulation here in the viewport.
                                    if ui
                                        .button("Play scene")
                                        .on_hover_text("Play this scene in its own game window, as a player would. The launcher's Play and game builds start on the start scene instead.")
                                        .clicked()
                                    {
                                        menu_action = Some(MenuAction::PlayScene);
                                    }
                                });
                            });
                            // The Plugins slide-down: only shown when toggled on
                            // from the toolbar tab above. A Panel::top stacks
                            // directly below the toolbar and shrinks whatever's
                            // left for the dock area (the Viewport included)
                            // beneath it, the same way the toolbar and console
                            // panels already claim their own space.
                            if plugins_panel_open {
                                egui::Panel::top("plugins_panel")
                                    .resizable(false)
                                    .default_size(110.0)
                                    .show(ui, |ui| {
                                        let enabled: Vec<&(String, frame_engine::world::InstalledPlugin)> =
                                            installed_plugins.iter().filter(|(_, p)| p.enabled).collect();
                                        if enabled.is_empty() {
                                            ui.weak(
                                                "No plugins enabled. Turn some on from Edit > Editor settings…",
                                            );
                                        } else {
                                            egui::ScrollArea::horizontal().show(ui, |ui| {
                                                ui.horizontal(|ui| {
                                                    for (_, plugin) in &enabled {
                                                        ui.group(|ui| {
                                                            ui.set_min_width(160.0);
                                                            ui.vertical(|ui| {
                                                                ui.strong(&plugin.manifest.name);
                                                                ui.weak(format!(
                                                                    "v{} by {}",
                                                                    plugin.manifest.version,
                                                                    plugin.manifest.author
                                                                ));
                                                                if !plugin.manifest.description.is_empty() {
                                                                    ui.label(&plugin.manifest.description);
                                                                }
                                                                // A button per declared action. Clicking
                                                                // one only ever records which kind was
                                                                // chosen; the editor decides what that
                                                                // means and does it after this pass, the
                                                                // plugin itself never runs anything here.
                                                                for action in &plugin.manifest.actions {
                                                                    if ui.button(&action.label).clicked() {
                                                                        plugin_action = Some(action.kind.clone());
                                                                    }
                                                                }
                                                                // Panels: titled, global sections, not
                                                                // tied to this or any entity. A field only
                                                                // shows if globals_edit already has a
                                                                // value under its name; it can't
                                                                // originate one, same rule as everywhere
                                                                // else in this system.
                                                                for panel in &plugin.manifest.panels {
                                                                    ui.separator();
                                                                    ui.strong(&panel.title);
                                                                    for field in &panel.fields {
                                                                        if let Some(value) =
                                                                            globals_edit.get_mut(&field.name)
                                                                        {
                                                                            ui.horizontal(|ui| {
                                                                                ui.label(field.label.as_str());
                                                                                match (field.min, field.max) {
                                                                                    (Some(lo), Some(hi)) => {
                                                                                        ui.add(egui::Slider::new(
                                                                                            value,
                                                                                            lo..=hi,
                                                                                        ));
                                                                                    }
                                                                                    _ => {
                                                                                        let mut drag =
                                                                                            egui::DragValue::new(value)
                                                                                                .speed(0.1);
                                                                                        if let Some(lo) = field.min {
                                                                                            drag = drag
                                                                                                .range(lo..=f64::MAX);
                                                                                        }
                                                                                        if let Some(hi) = field.max {
                                                                                            drag = drag
                                                                                                .range(f64::MIN..=hi);
                                                                                        }
                                                                                        ui.add(drag);
                                                                                    }
                                                                                }
                                                                            });
                                                                        }
                                                                    }
                                                                    for action in &panel.actions {
                                                                        if ui.button(&action.label).clicked() {
                                                                            plugin_action =
                                                                                Some(action.kind.clone());
                                                                        }
                                                                    }
                                                                }
                                                            });
                                                        });
                                                    }
                                                });
                                            });
                                        }
                                    });
                            }
                            if let Some(text) = &notice {
                                dismiss_notice |= version_notice_ui(ui.ctx(), text);
                            }
                            // Unsaved-changes prompt, shown when quitting, closing
                            // the project or reloading would throw edits away.
                            if let Some(action) = pending_unsaved {
                                egui::Window::new("Unsaved changes")
                                    .collapsible(false)
                                    .resizable(false)
                                    .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
                                    .show(ui.ctx(), |ui| {
                                        ui.label(match action {
                                            GuardedAction::Quit => format!(
                                                "Save your changes to {project_label} before quitting?"
                                            ),
                                            GuardedAction::CloseProject => format!(
                                                "Save your changes to {project_label} before closing it?"
                                            ),
                                            GuardedAction::ReloadScene => format!(
                                                "Reloading discards your unsaved changes to {project_label}. Save first?"
                                            ),
                                            GuardedAction::OpenScene
                                            | GuardedAction::SwitchScene => format!(
                                                "Save your changes to {project_label} before opening another scene?"
                                            ),
                                        });
                                        ui.add_space(8.0);
                                        ui.horizontal(|ui| {
                                            if ui.button("Save").clicked() {
                                                unsaved_choice = Some(UnsavedChoice::Save);
                                            }
                                            if ui.button("Don't save").clicked() {
                                                unsaved_choice = Some(UnsavedChoice::Discard);
                                            }
                                            if ui.button("Cancel").clicked() {
                                                unsaved_choice = Some(UnsavedChoice::Cancel);
                                            }
                                        });
                                    });
                            }
                            if build_open {
                                if let Some(view) = &build_view {
                                    build_action = build::build_window_ui(
                                        ui.ctx(),
                                        &mut build_open,
                                        &mut build_settings_edit,
                                        view,
                                    );
                                }
                            }
                            // Editor Settings: a floating window, opened from
                            // Edit > Editor settings…. Just plugin toggles for
                            // now; a home for more editor-wide configuration
                            // later, the same way Project Settings grew on the
                            // launcher.
                            if editor_settings_open {
                                egui::Window::new("Editor Settings")
                                    .collapsible(false)
                                    .open(&mut editor_settings_open)
                                    .default_width(420.0)
                                    .show(ui.ctx(), |ui| {
                                      egui::ScrollArea::vertical()
                                        .max_height(560.0)
                                        .auto_shrink([false, true])
                                        .show(ui, |ui| {
                                        ui.heading("Preferences");
                                        ui.add_space(4.0);
                                        section_label(ui, "Flythrough camera (hold right mouse)");
                                        ui.add(
                                            egui::Slider::new(&mut prefs_edit.fly_speed, 0.1..=200.0)
                                                .logarithmic(true)
                                                .text("Fly speed"),
                                        );
                                        ui.weak("Scrolling while you fly changes this too, and it is remembered.");
                                        ui.add(
                                            egui::Slider::new(&mut prefs_edit.look_sensitivity, 0.1..=3.0)
                                                .text("Look sensitivity"),
                                        );
                                        outerface_checkbox(ui, &mut prefs_edit.invert_look_x, "Invert horizontal look");
                                        outerface_checkbox(ui, &mut prefs_edit.invert_look_y, "Invert vertical look");
                                        ui.add_space(6.0);
                                        section_label(ui, "Orbit camera (middle mouse)");
                                        ui.add(
                                            egui::Slider::new(&mut prefs_edit.orbit_sensitivity, 0.1..=3.0)
                                                .text("Orbit sensitivity"),
                                        );
                                        ui.add_space(6.0);
                                        section_label(ui, "Scene tab");
                                        ui.add(
                                            egui::Slider::new(&mut prefs_edit.drag_hold_seconds, 0.05..=1.5)
                                                .suffix(" s")
                                                .text("Hold before dragging"),
                                        );
                                        ui.weak("How long to hold a row before it starts a drag-to-reparent. Lower is quicker; too low and a click can turn into a drag.");
                                        ui.add_space(6.0);
                                        section_label(ui, "Rendering");
                                        outerface_checkbox(ui, &mut prefs_edit.vsync, "Limit to the screen's refresh rate (VSync)");
                                        ui.weak("On, the editor never draws faster than your screen shows, and draws nothing while nothing changes. Turn it off only to measure how fast it can go.");
                                        ui.add_space(4.0);
                                        outerface_checkbox(ui, &mut prefs_edit.shadows, "Shadows");
                                        ui.add(
                                            egui::Slider::new(&mut prefs_edit.shadow_distance, 20.0..=2000.0)
                                                .logarithmic(true)
                                                .text("Shadow distance"),
                                        );
                                        ui.weak("The first directional Light casts shadows. A shorter distance gives sharper shadows near the camera; a longer one reaches further. Turn shadows off if the viewport feels slow.");
                                        ui.add_space(4.0);
                                        outerface_checkbox(ui, &mut prefs_edit.sky, "Sky and fog");
                                        ui.add_enabled(
                                            prefs_edit.sky,
                                            egui::Slider::new(&mut prefs_edit.fog_amount, 0.0..=sky::FOG_AMOUNT_MAX)
                                                .text("Fog"),
                                        );
                                        ui.add_enabled(
                                            prefs_edit.sky,
                                            egui::Slider::new(&mut prefs_edit.clouds, 0.0..=1.0)
                                                .text("Clouds"),
                                        );
                                        ui.weak("A sky behind the scene, lit by the first directional Light: lower the light toward the horizon for a sunset, below it for night. Distant ground fades into the horizon colour; 0 turns the fog off. With the sky off the viewport goes back to a plain background.");
                                        ui.add_space(4.0);
                                        ui.horizontal(|ui| {
                                            egui::ComboBox::from_id_salt("grass_quality_picker")
                                                .selected_text(prefs_edit.grass.label())
                                                .show_ui(ui, |ui| {
                                                    for q in grass::GrassQuality::ALL {
                                                        ui.selectable_value(&mut prefs_edit.grass, q, q.label());
                                                    }
                                                });
                                            ui.label("Grass");
                                        });
                                        ui.weak("How much grass modules draw: blades close to the camera and how far grass reaches. Off draws none. High suits most graphics cards from the last few years.");
                                        ui.add_space(4.0);
                                        outerface_checkbox(ui, &mut prefs_edit.sun_time, "Sun by time of day");
                                        ui.add_enabled(
                                            prefs_edit.sun_time,
                                            egui::Slider::new(&mut prefs_edit.time_of_day, 0.0..=24.0)
                                                .custom_formatter(|h, _| {
                                                    format!("{:02}:{:02}", h as u32, (h.fract() * 60.0) as u32)
                                                })
                                                .text("Time of day"),
                                        );
                                        ui.weak("Puts the sun where it would be at this hour instead of using the Light's direction: it rises in the east, climbs, and sets in the west, and lights, shadows, sky and fog follow. The scene is not changed; turn this off and the Light's own direction is back. Nights are darker.");
                                        ui.add_space(6.0);
                                        if ui.button("Reset to defaults").clicked() {
                                            prefs_edit = EditorPrefs {
                                                show_help: prefs_edit.show_help,
                                                ..EditorPrefs::default()
                                            };
                                        }
                                        ui.separator();
                                        ui.heading("Plugins");
                                        ui.add_space(4.0);
                                        if installed_plugins.is_empty() {
                                            ui.weak("No plugins found in this project's plugins/ folder.");
                                        }
                                        for (name, plugin) in &installed_plugins {
                                            ui.horizontal(|ui| {
                                                let mut on = plugin.enabled;
                                                if outerface_checkbox(ui, &mut on, &plugin.manifest.name).changed() {
                                                    plugin_toggle = Some((name.clone(), on));
                                                }
                                                ui.weak(format!(
                                                    "v{} by {}",
                                                    plugin.manifest.version, plugin.manifest.author
                                                ));
                                            });
                                            if !plugin.manifest.description.is_empty() {
                                                ui.weak(&plugin.manifest.description);
                                            }
                                            ui.add_space(4.0);
                                        }
                                        ui.separator();
                                        credits_ui(ui);
                                        });
                                    });
                            }
                            // Bottom console dock — Output (the live log) and a
                            // Terminal placeholder. Full width; drag its top edge
                            // to resize.
                            egui::Panel::bottom("console")
                                .resizable(true)
                                .default_size(160.0)
                                .show(ui, |ui| {
                                    ui.horizontal(|ui| {
                                        ui.selectable_value(
                                            &mut console_tab,
                                            ConsoleTab::Output,
                                            "Output",
                                        );
                                        ui.selectable_value(
                                            &mut console_tab,
                                            ConsoleTab::Terminal,
                                            "Terminal",
                                        );
                                        ui.selectable_value(
                                            &mut console_tab,
                                            ConsoleTab::Assets,
                                            "Assets",
                                        );
                                        ui.selectable_value(
                                            &mut console_tab,
                                            ConsoleTab::Prefabs,
                                            "Prefabs",
                                        );
                                        ui.selectable_value(
                                            &mut console_tab,
                                            ConsoleTab::Scenes,
                                            "Scenes",
                                        );
                                    });
                                    ui.separator();
                                    match console_tab {
                                        ConsoleTab::Output => {
                                            egui::ScrollArea::vertical()
                                                .stick_to_bottom(true)
                                                .auto_shrink([false, false])
                                                .show(ui, |ui| {
                                                    if log_lines.is_empty() {
                                                        ui.weak("(no output yet)");
                                                    }
                                                    for line in &log_lines {
                                                        ui.monospace(line);
                                                    }
                                                });
                                        }
                                        ConsoleTab::Terminal => {
                                            ui.weak("(terminal goes here)");
                                        }
                                        ConsoleTab::Assets => {
                                            if let Some(edit) = assets_tab_ui(
                                                ui,
                                                assets_root.as_deref(),
                                                &mut assets_subdir,
                                                &mut new_asset_folder,
                                                &thumbnails,
                                                &mut move_pending,
                                                &mesh_meta_snapshot,
                                                &image_names,
                                                &mut material_editing,
                                            ) {
                                                material_edit = Some(edit);
                                            }
                                        }
                                        ConsoleTab::Prefabs => {
                                            if let Some(name) = prefabs_tab_ui(ui, &prefabs) {
                                                prefab_spawn_request = Some(name);
                                            }
                                        }
                                        ConsoleTab::Scenes => {
                                            if let Some(view) = &scenes_view {
                                                scene_action = scenes::scenes_tab_ui(
                                                    ui,
                                                    view,
                                                    &mut scenes_tab,
                                                );
                                            }
                                        }
                                    }
                                });
                            // Right inspector dock — Scene / Inspector tabs.
                            // Drag its left edge to resize.
                            // The dock fills the whole central area (below the
                            // toolbar, above the console). Frame::NONE keeps it
                            // from painting a background, so the Viewport tab —
                            // whose body we leave unpainted — shows the 3D scene
                            // rendered behind egui. This lets the Script Editor
                            // tab take over the centre, full-width.
                            egui::CentralPanel::default()
                                .frame(egui::Frame::NONE)
                                .show(ui, |ui| {
                                    // egui_dock has its own hardcoded default
                                    // look and ignores the ambient egui style
                                    // unless told otherwise -- without this,
                                    // every tab's chrome (bars, borders,
                                    // the active-tab highlight) stays stock
                                    // default no matter what Visuals we set.
                                    egui_dock::DockArea::new(&mut dock_state)
                                        .style(dock_style(ui.style()))
                                        .show_inside(ui, &mut viewer);
                                });
                        });
                    if let Some(v) = full_output.viewport_output.get(&egui::ViewportId::ROOT) {
                        egui_delay = v.repaint_delay;
                    }
                    self.egui_cause = if egui_delay.is_zero() {
                        self.egui_ctx.repaint_causes().first().map(short_cause)
                    } else {
                        None
                    };
                    state.handle_platform_output(window, full_output.platform_output);
                    let ppp = full_output.pixels_per_point;
                    let jobs = self.egui_ctx.tessellate(full_output.shapes, ppp);
                    (jobs, full_output.textures_delta, ppp)
                } else {
                    (Vec::new(), egui::TexturesDelta::default(), 1.0)
                };
                // Draw and present every popped-out tab's own window this same
                // frame too, reusing this same `viewer` (and therefore this
                // same snapshot of world state) the main dock just drew from —
                // so an edit made in a popped-out Inspector window and one
                // made in a still-docked tab in the same frame both land in
                // the one drain-back below, instead of racing each other.
                let popped_out_tabs: Vec<Tab> = self.popped_out.keys().copied().collect();
                for tab in popped_out_tabs {
                    // A popped-out window has its own GpuState and therefore
                    // its own egui_renderer, which never had the main
                    // window's preview_texture_id registered — drawing it
                    // there would reference a texture id that doesn't exist
                    // in that renderer. Simplest safe fix: no live preview
                    // image while the Inspector is popped out; the Camera
                    // checkbox and Parent section still work as normal.
                    if tab == Tab::Inspector {
                        viewer.preview_texture_id = None;
                    }
                    self.render_popped_out(tab, &mut viewer);
                }
                self.console_tab = console_tab;
                self.plugins_panel_open = plugins_panel_open;
                self.editor_settings_open = editor_settings_open;
                self.build_window_open = build_open;
                self.build_settings = build_settings_edit;
                if let Some(action) = build_action {
                    self.handle_build_action(action);
                }
                if prefs_edit != self.current_prefs() {
                    self.apply_prefs(prefs_edit);
                }
                if let Some((name, enabled)) = plugin_toggle {
                    self.set_plugin_enabled(&name, enabled);
                }
                if let Some(kind) = plugin_action {
                    self.run_plugin_action(kind);
                }
                // Written back unconditionally, the same way plugin
                // enable/disable isn't part of undo either: global panel
                // values live outside the Inspector's per-entity editing
                // gesture, so there's nothing here for undo to coalesce.
                self.world.globals = globals_edit;
                self.assets_subdir = assets_subdir;
                self.new_asset_folder = new_asset_folder;
                self.move_pending = move_pending;
                self.material_editing = material_editing;
                if let Some((name, material)) = material_edit {
                    if let Some(meta) = self.world.mesh_meta.get_mut(&name) {
                        meta.material = material;
                        self.dirty = true;
                    }
                    self.refresh_custom_mesh_gpu();
                }
                self.log_lines = log_lines;
                // Drain the dock layout and the tabs' state back onto self.
                self.dock_state = dock_state;
                // Pop out a tab now that self.dock_state is the real, current
                // layout again — pop_out_tab removes the tab from it, so this
                // has to run after the line just above, not before.
                if let Some(tab) = viewer.pop_out_request.take() {
                    self.pop_out_tab(event_loop, tab);
                }
                self.viewport_rect = viewer.viewport_rect;
                self.world.script_library = viewer.script_library;
                self.git_summary = viewer.git_summary;
                self.new_script_name = viewer.new_script_name;
                self.script_filter = viewer.script_filter;
                self.open_script = viewer.open_script;
                self.renaming = viewer.renaming;
                self.new_prefab_name = viewer.new_prefab_name;
                if let Some((id, name)) = viewer.save_prefab_request.take() {
                    self.save_entity_as_prefab(id, &name);
                }
                self.scenes_tab = scenes_tab;
                if let Some(action) = scene_action {
                    self.handle_scene_action(action, event_loop);
                }
                if let Some(name) = prefab_spawn_request {
                    self.spawn_prefab_at_focus(&name);
                }
                if let Some((old_name, new_name)) = viewer.renamed {
                    // Rewrite every entity's Script.uses that pointed at the old
                    // name, so a rename doesn't orphan references the way a
                    // delete deliberately can. The library key itself was
                    // already swapped in scripts_tab_ui; this is the part that
                    // needed world access, which that function doesn't have.
                    for slot in self.world.scripts.iter_mut() {
                        if let Some(script) = slot {
                            if script.uses == old_name {
                                script.uses = new_name.clone();
                            }
                        }
                    }
                    self.log(format!("Renamed script '{old_name}' to '{new_name}'"));
                }
                self.selected = viewer.selection;
                let edited = viewer.edited;
                let custom_fields = viewer.custom_fields;
                // Coalesce Inspector edits into one undo step per gesture: snapshot
                // on the first frame the values change, and reset when they stop.
                // (This runs before the write-back below, so the world is still in
                // its pre-edit state when we snapshot it.) Plugin fields count as
                // part of the same gesture as everything else in the Inspector.
                let inspector_changed =
                    edited != edited_before || custom_fields != custom_fields_before;
                if inspector_changed && !self.inspector_editing {
                    self.push_undo();
                    self.inspector_editing = true;
                } else if !inspector_changed {
                    self.inspector_editing = false;
                }
                // Push any inspector edits back into the world. The render this
                // frame already used the old values; the change shows next frame
                // (same one-frame path as the keyboard nudge).
                if let Some(EditedEntity {
                    id,
                    pos,
                    vel,
                    color,
                    controlled,
                    scale,
                    material,
                    rotation,
                    script_source,
                    mesh,
                    is_static,
                    has_gravity,
                    is_rigid_body,
                    light,
                    sound,
                    is_camera,
                    parent,
                    ui_text,
                    ui_image,
                    character,
                    ext,
                }) = edited
                {
                    if let Some(p) = self.world.positions.get_mut(id) {
                        *p = pos;
                    }
                    if let Some(v) = self.world.velocities.get_mut(id) {
                        *v = vel;
                    }
                    self.world.colors.insert(id, color);
                    self.world.scales.insert(id, scale);
                    self.world.materials.insert(id, material);
                    self.world.rotations.insert(id, rotation);
                    self.world.meshes.insert(id, mesh);
                    if controlled {
                        self.world.controlled.insert(id, Controlled);
                    } else {
                        self.world.controlled.remove(id);
                    }
                    if is_static {
                        self.world.statics.insert(id, Static);
                    } else {
                        self.world.statics.remove(id);
                    }
                    if has_gravity {
                        self.world.gravities.insert(id, Gravity);
                    } else {
                        self.world.gravities.remove(id);
                    }
                    if is_rigid_body {
                        self.world.rigid_bodies.insert(id, RigidBody);
                    } else {
                        self.world.rigid_bodies.remove(id);
                    }
                    match light {
                        Some(l) => {
                            self.world.lights.insert(id, l);
                        }
                        None => {
                            self.world.lights.remove(id);
                        }
                    }
                    if is_camera {
                        self.world.cameras.insert(id, Camera);
                    } else {
                        self.world.cameras.remove(id);
                    }
                    match parent {
                        Some(p) => {
                            self.world.parents.insert(id, p);
                        }
                        None => {
                            self.world.parents.remove(id);
                        }
                    }
                    match sound {
                        Some(s) => {
                            self.world.sounds.insert(id, s);
                        }
                        None => {
                            self.world.sounds.remove(id);
                        }
                    }
                    match script_source {
                        Some(uses) => {
                            self.world.scripts.insert(id, Script { uses });
                        }
                        None => {
                            self.world.scripts.remove(id);
                        }
                    }
                    match ui_text {
                        Some(t) => {
                            self.world.ui_texts.insert(id, t);
                        }
                        None => {
                            self.world.ui_texts.remove(id);
                        }
                    }
                    match ui_image {
                        Some(img) => {
                            self.world.ui_images.insert(id, img);
                        }
                        None => {
                            self.world.ui_images.remove(id);
                        }
                    }
                    match character {
                        Some(body) => {
                            self.world.characters.insert(id, body);
                        }
                        None => {
                            self.world.characters.remove(id);
                        }
                    }
                    ext.apply(&mut self.world, id);
                }
                // Drag-and-drop reparenting, applied *after* the Inspector's
                // own write-back directly above, not before it (where it
                // used to sit, right after the save-prefab drain). The
                // Inspector's `edited` snapshot is captured once at the top
                // of the frame, before the Scene tab (and any drag-and-drop
                // reparent) has even rendered — so if the row being dragged
                // was also the Inspector's currently-selected entity (the
                // common case: selecting it is usually how you'd start
                // dragging it), the write-back above would still be holding
                // that entity's *old* Parent state, and applying it after
                // the reparent used to silently undo the drop in the same
                // frame it happened: the Scene tree wouldn't show the new
                // nesting, the Inspector's "Attached to another entity"
                // checkbox would stay unticked, even though `reparent_entity`
                // below had already logged the change and briefly written
                // it. Ordering the drag-and-drop reparent last makes it the
                // one that actually sticks.
                if let Some((child, target)) = viewer.scene_reparent_request.take() {
                    self.reparent_entity(child, target);
                }
                // Plugin field edits write back the same unconditional way
                // every other single value in the Inspector does, each to
                // the entity captured alongside it, not whatever's currently
                // selected (which may have already changed this same frame).
                for field in &custom_fields {
                    self.world
                        .insert_dynamic::<f64>(&field.name, field.entity, field.value);
                }
                // A module's own change to the world, asked for from its
                // Inspector section (a button that spawns entities, say),
                // after an undo step.
                if module_api::modules_want_world_edit(&self.modules) {
                    self.push_undo();
                    module_api::edit_world_modules(&self.modules, &mut self.world);
                }
                // A menu item clicked this frame runs the same action method the
                // keyboard uses — one command, two triggers. This sits at
                // statement level (NOT inside the `edited` block above), so it
                // fires whether or not an entity is selected.
                // The unsaved-changes prompt's answer, applied here so Save sees
                // this frame's Inspector and Script Editor edits already written
                // back to the world above.
                if dismiss_notice {
                    self.version_notice = None;
                }
                if let Some(choice) = unsaved_choice {
                    if let Some(action) = self.pending_unsaved.take() {
                        match choice {
                            UnsavedChoice::Cancel => {}
                            UnsavedChoice::Discard => self.run_guarded(action, event_loop),
                            UnsavedChoice::Save => {
                                self.save_scene();
                                if self.is_dirty() {
                                    self.log("Not saved, so the action was cancelled".to_string());
                                } else {
                                    self.run_guarded(action, event_loop);
                                }
                            }
                        }
                    }
                }
                match menu_action {
                    Some(MenuAction::OpenScene) => {
                        self.request_guarded(GuardedAction::OpenScene, event_loop)
                    }
                    Some(MenuAction::ImportModel) => self.import_model(),
                    Some(MenuAction::ImportAudio) => self.import_audio(),
                    Some(MenuAction::ImportTexture) => self.import_texture(),
                    Some(MenuAction::SaveScene) => self.save_scene(),
                    Some(MenuAction::SaveSceneAs) => self.save_scene_as(),
                    Some(MenuAction::ReloadScene) => {
                        self.request_guarded(GuardedAction::ReloadScene, event_loop)
                    }
                    Some(MenuAction::CloseProject) => {
                        self.request_guarded(GuardedAction::CloseProject, event_loop)
                    }
                    Some(MenuAction::Undo) => self.undo(),
                    Some(MenuAction::Redo) => self.redo(),
                    Some(MenuAction::SpawnEntity) => self.spawn_at_focus(),
                    Some(MenuAction::DespawnSelected) => self.despawn_selected(),
                    Some(MenuAction::ClearSelection) => self.clear_selection(),
                    Some(MenuAction::TogglePause) => self.toggle_pause(),
                    Some(MenuAction::StepOnce) => self.step_once(),
                    Some(MenuAction::ToggleHelp) => self.toggle_help(),
                    Some(MenuAction::ToggleStats) => self.toggle_stats(),
                    Some(MenuAction::OpenEditorSettings) => self.editor_settings_open = true,
                    Some(MenuAction::BuildGame) => self.open_build_window(),
                    Some(MenuAction::PlayScene) => self.play_open_scene(event_loop),
                    Some(MenuAction::About) => {
                        self.log("Frame Editor — a hand-rolled Rust simulation engine and editor.");
                    }
                    Some(MenuAction::Quit) => self.request_guarded(GuardedAction::Quit, event_loop),
                    None => {}
                }
                // Camera preview: only while a Camera entity is selected, and
                // rendered before the main pass below so the Inspector's egui
                // draw call this same frame already sees this frame's view,
                // not last frame's.
                if let Some(id) = self.selected {
                    if self.world.cameras.get(id).is_some() {
                        // The preview shares the viewport's render origin, because
                        // module geometry is placed once per frame. It only loses
                        // precision if the previewed camera is very far (hundreds
                        // of kilometres) from the editor camera.
                        let preview_origin = view_origin;
                        if let Some(preview_view_proj) = camera_entity_view_proj(
                            &self.world,
                            id,
                            PREVIEW_WIDTH,
                            PREVIEW_HEIGHT,
                            preview_origin,
                        ) {
                            // Excludes the camera's own entity: otherwise its
                            // eye sits somewhere inside its own mesh (the
                            // main viewport draws it as an ordinary cube, so
                            // it can be selected and moved like anything
                            // else) and the preview is just the inside of
                            // that shape, whatever its scale. Built fresh
                            // rather than reusing `instances` above, which
                            // deliberately keeps every entity for the main
                            // viewport.
                            let (preview_instances, preview_group_counts) = build_instances(
                                &self.world,
                                selected,
                                &colliding,
                                &custom_names,
                                &anchors,
                                Some(id),
                                preview_origin,
                            );
                            if let Some(gpu) = &mut self.gpu {
                                gpu.sync_modules(&module_scenes, view_origin);
                                gpu.render_preview(
                                    &preview_instances,
                                    &preview_group_counts,
                                    preview_view_proj,
                                    &lights,
                                );
                            }
                        }
                    }
                }
                if let Some(gpu) = &mut self.gpu {
                    gpu.sync_modules(&module_scenes, view_origin);
                    gpu.render(
                        &instances,
                        &group_counts,
                        &text_instances,
                        view_proj,
                        &lights,
                        &egui_paint_jobs,
                        &egui_textures_delta,
                        egui_ppp,
                    );
                }
                let now = std::time::Instant::now();
                self.perf.end(now);
                if let Some(jump) = self.perf.take_jump() {
                    // Say what the editor was doing, to find what caused it.
                    let doing = match &self.redraw_reason {
                        Some(why) => why.clone(),
                        None => "idle".to_string(),
                    };
                    self.log(format!("{jump}, while {doing}"));
                }
                self.redraw_reason = self.continuous_reason(egui_delay);
                if self.redraw_reason.is_some() {
                    self.idle_wake = None;
                    if let Some(window) = &self.window {
                        window.request_redraw();
                    }
                } else {
                    // Nothing moving: sleep until something happens, the UI
                    // asks to draw again, or the idle refresh is due.
                    self.idle_wake = Some(now + egui_delay.min(IDLE_REFRESH));
                }
            }
            _ => {}
        }
    }
}
// The fallback scene used when there's no scene.ron on disk yet.
/// Path to the file that remembers recently opened projects (one folder path
/// per line), under the platform config directory.
fn recent_projects_file() -> Option<std::path::PathBuf> {
    dirs::config_dir().map(|d| d.join("frame-editor").join("recent-projects.txt"))
}

/// Editor preferences that persist across runs, stored as RON beside the
/// recent-projects list: whether the controls overlay shows, the flythrough
/// speed, and the mouse sensitivities. `#[serde(default)]` means an older
/// `editor.ron` with fewer fields still loads, filling the rest with defaults.
/// The two sensitivities are multipliers on the built-in base values, so 1.0
/// is the stock feel.
#[derive(serde::Serialize, serde::Deserialize, Clone, PartialEq)]
#[serde(default)]
struct EditorPrefs {
    show_help: bool,
    // Whether the performance overlay shows (View, Performance overlay, or F3).
    show_stats: bool,
    // Whether drawing waits for the screen's refresh.
    vsync: bool,
    fly_speed: f32,
    look_sensitivity: f32,
    orbit_sensitivity: f32,
    invert_look_x: bool,
    invert_look_y: bool,
    // Seconds a Scene-tree row must be held before it counts as a drag.
    drag_hold_seconds: f32,
    // Whether the directional light casts shadows.
    shadows: bool,
    // How far from the camera shadows reach, in world units.
    shadow_distance: f32,
    // Whether the sky and the distance fog are drawn.
    sky: bool,
    // How thick the fog is, as a multiplier on the default (0 is none).
    fog_amount: f32,
    // How much of the sky clouds cover, 0 (none) to 1 (overcast).
    clouds: f32,
    // How much grass modules draw: blades close in and how far it reaches.
    grass: grass::GrassQuality,
    // Whether the sun is placed by the time of day instead of the Light.
    sun_time: bool,
    // The hour (0 to 24) the sun is placed for.
    time_of_day: f32,
}

impl Default for EditorPrefs {
    fn default() -> Self {
        Self {
            show_help: true,
            show_stats: false,
            vsync: true,
            fly_speed: CAM_PAN_SPEED,
            look_sensitivity: 1.0,
            orbit_sensitivity: 1.0,
            invert_look_x: false,
            invert_look_y: false,
            drag_hold_seconds: SCENE_TREE_DRAG_HOLD_DEFAULT,
            shadows: true,
            shadow_distance: SHADOW_DISTANCE_DEFAULT,
            sky: true,
            fog_amount: sky::FOG_AMOUNT_DEFAULT,
            clouds: sky::CLOUDS_DEFAULT,
            grass: grass::GrassQuality::High,
            sun_time: false,
            time_of_day: sun::DEFAULT_HOUR,
        }
    }
}

impl EditorPrefs {
    /// Pull any hand-edited or corrupt value back into a usable range.
    fn sanitized(mut self) -> Self {
        let fix = |v: f32, default: f32, lo: f32, hi: f32| {
            if v.is_finite() {
                v.clamp(lo, hi)
            } else {
                default
            }
        };
        self.fly_speed = fix(self.fly_speed, CAM_PAN_SPEED, 0.1, 200.0);
        self.look_sensitivity = fix(self.look_sensitivity, 1.0, 0.1, 5.0);
        self.orbit_sensitivity = fix(self.orbit_sensitivity, 1.0, 0.1, 5.0);
        self.shadow_distance = fix(self.shadow_distance, SHADOW_DISTANCE_DEFAULT, 20.0, 2000.0);
        self.fog_amount = fix(
            self.fog_amount,
            sky::FOG_AMOUNT_DEFAULT,
            0.0,
            sky::FOG_AMOUNT_MAX,
        );
        self.clouds = fix(self.clouds, sky::CLOUDS_DEFAULT, 0.0, 1.0);
        self.time_of_day = fix(self.time_of_day, sun::DEFAULT_HOUR, 0.0, 24.0);
        self.drag_hold_seconds = fix(
            self.drag_hold_seconds,
            SCENE_TREE_DRAG_HOLD_DEFAULT,
            0.05,
            1.5,
        );
        self
    }
}

/// Hand the time-of-day preferences to the sun.
fn apply_sun_prefs(prefs: &EditorPrefs) {
    sun::set_time_of_day(prefs.sun_time, prefs.time_of_day);
}

fn prefs_file() -> Option<std::path::PathBuf> {
    dirs::config_dir().map(|d| d.join("frame-editor").join("editor.ron"))
}

fn load_prefs() -> EditorPrefs {
    prefs_file()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|t| ron::from_str::<EditorPrefs>(&t).ok())
        .map(EditorPrefs::sanitized)
        .unwrap_or_default()
}

fn save_prefs(prefs: &EditorPrefs) {
    let Some(path) = prefs_file() else { return };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(text) = ron::to_string(prefs) {
        let _ = std::fs::write(path, text);
    }
}

/// The remembered project folders as written, unfiltered and in file order.
fn read_recent_projects() -> Vec<std::path::PathBuf> {
    let Some(file) = recent_projects_file() else {
        return Vec::new();
    };
    let Ok(text) = std::fs::read_to_string(&file) else {
        return Vec::new();
    };
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(std::path::PathBuf::from)
        .collect()
}

/// Persist the remembered project folders.
fn write_recent_projects(roots: &[std::path::PathBuf]) {
    let Some(file) = recent_projects_file() else {
        return;
    };
    if let Some(parent) = file.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let text = roots
        .iter()
        .filter_map(|p| p.to_str())
        .collect::<Vec<_>>()
        .join("\n");
    let _ = std::fs::write(&file, text);
}

/// Remembered projects that still exist, most-recently-edited first (by each
/// project scene file's modification time).
/// Per-project metadata stored beside the scene in `project.ron`. The project's
/// name is the scene file's stem, so it isn't duplicated here.
#[derive(serde::Serialize, serde::Deserialize, Default, Clone)]
struct ProjectManifest {
    /// The project's name, shown on the launcher and in window titles. Empty
    /// in a project from before it was stored here; the editor then uses the
    /// start scene's name (or the folder's) and writes it in on conversion.
    #[serde(default)]
    name: String,
    /// The scene the game starts on (`World` for `scenes/World.ron`): the
    /// launcher's Play and game builds start here. See `project`.
    #[serde(default)]
    start_scene: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    version: String,
    /// The Frame Editor version that last saved this project, written by
    /// `write_manifest`. Not the game's own `version` above. An older editor
    /// opening the project compares it with its own and warns if it is
    /// newer. Empty in a project from before this existed.
    #[serde(default)]
    editor: String,
    /// File > Build Game's choices for this project.
    #[serde(default)]
    build: build::BuildSettings,
}

/// A warning if a plugin says it needs a newer engine than this one (its
/// `engine` field). The plugin still loads; it may use script variables or
/// features this editor doesn't have.
fn plugin_engine_warning(manifest: &frame_engine::world::PluginManifest) -> Option<String> {
    if !frame_engine::version::is_newer_than_this(&manifest.engine) {
        return None;
    }
    Some(format!(
        "plugin '{}' needs Frame Engine {} or newer, and this is {}. It is loaded, but parts of it may not work",
        manifest.name,
        manifest.engine,
        frame_engine::version::ENGINE
    ))
}

/// Load every .obj in a project's assets folder into a name -> mesh map.
/// Parse failures get logged by the caller through the returned errors list.
/// Load every plugin in a project's `plugins/` folder: each subfolder with a
/// `plugin.ron` manifest and a `scripts/` folder of `.rhai` files. Returns
/// the manifests found (for `World::installed_plugins`), each plugin's
/// scripts as name-to-source pairs ready to merge into `script_library`
/// (name-spaced `<plugin-name>/<script-stem>`, so two different plugins'
/// scripts can never collide, and it's visible at a glance in the script
/// picker which scripts came from a plugin versus which were written by
/// hand), and any errors encountered, the same three-part shape
/// `load_project_models` already uses.
///
/// A folder with no `plugin.ron` is skipped quietly rather than treated as
/// an error, it just isn't a plugin folder. Reload merges and updates,
/// deliberately: it doesn't currently remove a script whose plugin, or whose
/// specific script file, has since been deleted from disk, a real limitation
/// worth knowing rather than a silent gap.
fn load_project_plugins(
    root: &std::path::Path,
) -> (
    std::collections::BTreeMap<String, frame_engine::world::PluginManifest>,
    std::collections::BTreeMap<String, String>,
    Vec<String>,
) {
    // Creates the folder if it's missing (a brand new project, or an older
    // one that predates plugins existing at all), so it's sitting there
    // ready to drag a plugin into without anyone having to make it by hand.
    // A no-op if it already exists.
    let _ = std::fs::create_dir_all(root.join("plugins"));
    load_plugins_from(&ProjectFiles::Folder(root.to_path_buf()))
}

/// Load a project's plugins from its folder or a game build's pack: every
/// `plugins/<folder>/plugin.ron`, with the `.rhai` files in that folder's
/// `scripts/`. Shared by the editor and Play.
fn load_plugins_from(
    files: &ProjectFiles,
) -> (
    std::collections::BTreeMap<String, frame_engine::world::PluginManifest>,
    std::collections::BTreeMap<String, String>,
    Vec<String>,
) {
    let mut manifests = std::collections::BTreeMap::new();
    let mut scripts = std::collections::BTreeMap::new();
    let mut errors = Vec::new();
    let all = files.files_under("plugins");
    for manifest_path in &all {
        let parts: Vec<&str> = manifest_path.split('/').collect();
        let [_, folder, "plugin.ron"] = parts.as_slice() else {
            continue; // only a plugin.ron directly inside a plugin's folder
        };
        let manifest_text = match files.read_to_string(manifest_path) {
            Ok(text) => text,
            Err(e) => {
                errors.push(format!("{manifest_path}: {e}"));
                continue;
            }
        };
        let manifest: frame_engine::world::PluginManifest = match ron::from_str(&manifest_text) {
            Ok(manifest) => manifest,
            Err(e) => {
                errors.push(format!("plugins/{folder}: invalid plugin.ron: {e}"));
                continue;
            }
        };
        if let Some(warning) = plugin_engine_warning(&manifest) {
            errors.push(warning);
        }
        let scripts_dir = format!("plugins/{folder}/scripts/");
        for script_path in &all {
            let Some(file) = script_path.strip_prefix(&scripts_dir) else {
                continue;
            };
            let Some(stem) = file.strip_suffix(".rhai").filter(|s| !s.contains('/')) else {
                continue;
            };
            match files.read_to_string(script_path) {
                Ok(source) => {
                    scripts.insert(format!("{}/{stem}", manifest.name), source);
                }
                Err(e) => errors.push(format!("{script_path}: {e}")),
            }
        }
        manifests.insert(manifest.name.clone(), manifest);
    }
    (manifests, scripts, errors)
}

/// Load every prefab in a project's `prefabs/` folder: each `.ron` file
/// directly inside it, named after its file stem (no subfolder, unlike
/// plugins — a prefab is one file, not a manifest plus scripts). Simpler
/// than `load_project_plugins` for the same reason: nothing here needs
/// merging into `World` or an enabled/disabled state, it's just read back to
/// spawn from later. A file that fails to parse is skipped with an error
/// rather than aborting the whole scan, the same tolerance
/// `load_project_plugins` gives a broken `plugin.ron`.
fn load_project_prefabs(
    root: &std::path::Path,
) -> (
    std::collections::BTreeMap<String, frame_engine::world::Prefab>,
    Vec<String>,
) {
    let mut prefabs = std::collections::BTreeMap::new();
    let mut errors = Vec::new();
    let dir = root.join("prefabs");
    // Creates the folder if missing, the same "ready to use immediately"
    // treatment plugins/ and assets/ already get.
    let _ = std::fs::create_dir_all(&dir);
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return (prefabs, errors);
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("ron") {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        match std::fs::read_to_string(&path) {
            Ok(text) => match ron::from_str::<frame_engine::world::Prefab>(&text) {
                Ok(prefab) => {
                    if prefab.format > frame_engine::world::SCENE_FORMAT {
                        errors.push(format!(
                            "{}: saved by a newer Frame Editor (save format {}, this editor reads up to {}), so parts of it may be missing here",
                            path.display(),
                            prefab.format,
                            frame_engine::world::SCENE_FORMAT
                        ));
                    }
                    prefabs.insert(stem.to_string(), prefab);
                }
                Err(e) => {
                    if frame_engine::world::scene_format_of(&text)
                        > frame_engine::world::SCENE_FORMAT
                    {
                        errors.push(format!(
                            "{}: saved by a newer Frame Editor and can't be read here; update Frame Editor to use it",
                            path.display()
                        ));
                    } else {
                        errors.push(format!("{}: invalid prefab: {e}", path.display()));
                    }
                }
            },
            Err(e) => errors.push(format!("{}: {e}", path.display())),
        }
    }
    (prefabs, errors)
}

/// Merge freshly-scanned plugin manifests and scripts into `world`,
/// preserving whatever enabled/disabled state it already has for a plugin
/// it's seen before, and defaulting a newly discovered one to disabled, the
/// same "off until you turn it on" default a mod manager uses. Only an
/// enabled plugin's scripts actually enter script_library. Shared by
/// load_project_assets (editing a project) and start_play (a fresh load for
/// the Play window), since both face the same problem: reconcile what's
/// really on disk right now with a world that's already been loaded.
fn merge_plugins_into_world(
    world: &mut World,
    manifests: std::collections::BTreeMap<String, frame_engine::world::PluginManifest>,
    scripts: std::collections::BTreeMap<String, String>,
) {
    let mut installed = std::collections::BTreeMap::new();
    for (name, manifest) in manifests {
        let enabled = world
            .installed_plugins
            .get(&name)
            .map(|p| p.enabled)
            .unwrap_or(false);
        installed.insert(
            name,
            frame_engine::world::InstalledPlugin { manifest, enabled },
        );
    }
    world.installed_plugins = installed;
    for (name, source) in scripts {
        let owner = name.split('/').next().unwrap_or("");
        let enabled = world
            .installed_plugins
            .get(owner)
            .map(|p| p.enabled)
            .unwrap_or(false);
        if enabled {
            world.script_library.insert(name, source);
        }
    }
}

fn load_project_models(
    root: &std::path::Path,
) -> (
    std::collections::BTreeMap<String, frame_engine::assets::MeshData>,
    Vec<String>,
) {
    load_models_from(&ProjectFiles::Folder(root.to_path_buf()))
}

/// Parse every .obj file under a project's `assets/`, from its folder or a
/// game build's pack. Models are named by file stem wherever they sit, so
/// two files with the same stem in different folders collide; the later one
/// in sorted path order wins.
fn load_models_from(
    files: &ProjectFiles,
) -> (
    std::collections::BTreeMap<String, frame_engine::assets::MeshData>,
    Vec<String>,
) {
    let mut models = std::collections::BTreeMap::new();
    let mut errors = Vec::new();
    for path in files.files_under("assets") {
        let Some(name) = path
            .rsplit('/')
            .next()
            .and_then(|file| file.strip_suffix(".obj"))
            .filter(|stem| !stem.is_empty())
            .map(String::from)
        else {
            continue;
        };
        match files.read_to_string(&path) {
            Ok(text) => match frame_engine::assets::parse_obj(&text) {
                Ok(data) => {
                    models.insert(name, data);
                }
                Err(e) => errors.push(format!("{name}.obj: {e}")),
            },
            Err(e) => errors.push(format!("{name}.obj: {e}")),
        }
    }
    (models, errors)
}

/// Read a project's manifest, or a default one if it has none yet.
/// One changed file in the working tree, for the Source Control tab: its path
/// and a short label ("modified", "new", "deleted", ...), plus whether the change
/// is staged.
struct GitFileStatus {
    path: String,
    label: &'static str,
    staged: bool,
}

/// A read-only snapshot of the open project's git state: enough for "what state
/// is my repo in?" at a glance. Produced by `git_summary`, refreshed on a timer.
struct GitSummary {
    branch: String,
    // Commits ahead of / behind the upstream branch, if one is configured.
    ahead_behind: Option<(usize, usize)>,
    upstream: Option<String>,
    files: Vec<GitFileStatus>,
}

/// Read the git state of the repository containing `root`, or None if it isn't
/// inside one. Read-only: opens, reads, drops. No credentials are ever needed
/// because nothing here talks to the network — ahead/behind counts compare
/// against the remote-tracking ref as of the last fetch/pull done OUTSIDE the
/// editor.
fn git_summary(root: &std::path::Path) -> Option<GitSummary> {
    // discover() walks up parent directories, so a project folder nested inside
    // a repo (the common layout) is found too.
    let repo = git2::Repository::discover(root).ok()?;

    // Branch name (or a short description of a detached/unborn HEAD).
    let head = repo.head().ok();
    let branch = head
        .as_ref()
        .and_then(|h| h.shorthand().ok())
        .unwrap_or("(no branch)")
        .to_string();

    // Ahead/behind vs. the upstream, if the branch has one configured.
    let mut ahead_behind = None;
    let mut upstream_name = None;
    if let Some(h) = &head {
        if h.is_branch() {
            if let Ok(name) = h.shorthand() {
                if let Ok(local) = repo.find_branch(name, git2::BranchType::Local) {
                    if let Ok(up) = local.upstream() {
                        if let Ok(Some(up_name)) = up.name() {
                            upstream_name = Some(up_name.to_string());
                        }
                        if let (Some(local_oid), Some(up_oid)) = (h.target(), up.get().target()) {
                            ahead_behind = repo.graph_ahead_behind(local_oid, up_oid).ok();
                        }
                    }
                }
            }
        }
    }

    // Working-tree and index status, including untracked files.
    let mut opts = git2::StatusOptions::new();
    opts.include_untracked(true);
    let mut files = Vec::new();
    if let Ok(statuses) = repo.statuses(Some(&mut opts)) {
        for entry in statuses.iter() {
            let Ok(path) = entry.path() else { continue };
            let s = entry.status();
            // Index (staged) changes and worktree (unstaged) changes are separate
            // flags on the same entry; a file can carry both.
            let checks: [(git2::Status, &'static str, bool); 8] = [
                (git2::Status::INDEX_NEW, "new", true),
                (git2::Status::INDEX_MODIFIED, "modified", true),
                (git2::Status::INDEX_DELETED, "deleted", true),
                (git2::Status::WT_NEW, "new", false),
                (git2::Status::WT_MODIFIED, "modified", false),
                (git2::Status::WT_DELETED, "deleted", false),
                (git2::Status::WT_RENAMED, "renamed", false),
                (git2::Status::CONFLICTED, "conflicted", false),
            ];
            for (flag, label, staged) in checks {
                if s.contains(flag) {
                    files.push(GitFileStatus {
                        path: path.to_string(),
                        label,
                        staged,
                    });
                }
            }
        }
    }

    Some(GitSummary {
        branch,
        ahead_behind,
        upstream: upstream_name,
        files,
    })
}

fn read_manifest(root: &std::path::Path) -> ProjectManifest {
    std::fs::read_to_string(root.join("project.ron"))
        .ok()
        .and_then(|t| ron::from_str(&t).ok())
        .unwrap_or_default()
}

/// Write a project's manifest, stamped with this editor's version in
/// `editor`. A stamp from a newer editor already on disk is kept, so opening
/// a project in an older editor never hides that a newer one saved it.
fn write_manifest(root: &std::path::Path, manifest: &ProjectManifest) {
    let mut manifest = manifest.clone();
    manifest.editor = newest_editor(&read_manifest(root).editor);
    if let Ok(text) = ron::ser::to_string_pretty(&manifest, ron::ser::PrettyConfig::default()) {
        let _ = std::fs::write(root.join("project.ron"), text);
    }
}

/// The editor version to stamp: this one, unless `existing` is newer.
fn newest_editor(existing: &str) -> String {
    if frame_engine::version::is_newer_than_this(existing) {
        existing.to_string()
    } else {
        frame_engine::version::ENGINE.to_string()
    }
}

/// After saving a scene, stamp the project it belongs to (the folder it is
/// in, if that folder has a `project.ron`) with this editor's version.
fn stamp_project(root: &std::path::Path) {
    if !root.join("project.ron").exists() {
        return;
    }
    let manifest = read_manifest(root);
    if manifest.editor == newest_editor(&manifest.editor) {
        return; // already stamped
    }
    write_manifest(root, &manifest);
}

/// A window with a version warning and an OK button. True when OK is
/// clicked. Shared by the launcher and the editor.
fn version_notice_ui(ctx: &egui::Context, text: &str) -> bool {
    let mut ok = false;
    egui::Window::new("Newer version")
        .collapsible(false)
        .resizable(false)
        .default_width(420.0)
        .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
        .show(ctx, |ui| {
            ui.label(text);
            ui.add_space(8.0);
            if ui.button("OK").clicked() {
                ok = true;
            }
        });
    ok
}

/// A remembered project, resolved for display on the launcher: its folder, the
/// scene file inside it, the name (the scene file's stem), and when it was last
/// edited (the scene file's modification time).
#[derive(Clone)]
struct RecentProject {
    root: std::path::PathBuf,
    name: String,
    description: String,
    version: String,
    modified: Option<std::time::SystemTime>,
}

/// Find a project folder's scene file: the first `.ron` in it that isn't the
/// `project.ron` manifest. A project's name is this file's stem.
fn find_scene(folder: &std::path::Path) -> Option<std::path::PathBuf> {
    // The same rule a game build uses on its pack (`ProjectFiles::scene`).
    let scene = ProjectFiles::Folder(folder.to_path_buf()).scene()?;
    Some(folder.join(scene))
}

/// A project's name: the one in `project.ron`, else its start scene's (an
/// older project), else its folder's.
fn project_display_name(root: &std::path::Path) -> String {
    let manifest = read_manifest(root);
    if !manifest.name.trim().is_empty() {
        return manifest.name.trim().to_string();
    }
    ProjectFiles::Folder(root.to_path_buf())
        .scene()
        .and_then(|scene| {
            scene
                .rsplit('/')
                .next()
                .and_then(|file| file.strip_suffix(".ron"))
                .map(String::from)
        })
        .or_else(|| root.file_name().and_then(|n| n.to_str()).map(String::from))
        .unwrap_or_else(|| "Project".to_string())
}

/// When a project was last edited: its newest scene file's time.
fn last_edited(root: &std::path::Path) -> Option<std::time::SystemTime> {
    let files = ProjectFiles::Folder(root.to_path_buf());
    let mut paths: Vec<std::path::PathBuf> = project::list_scenes(&files)
        .iter()
        .map(|name| root.join(project::scene_file(name)))
        .collect();
    if let Some(scene) = files.scene() {
        paths.push(root.join(scene));
    }
    paths
        .iter()
        .filter_map(|p| std::fs::metadata(p).and_then(|m| m.modified()).ok())
        .max()
}

/// A scene's name if `path` is a `.ron` file directly in `scenes_dir`.
fn scene_name_in(scenes_dir: &std::path::Path, path: &std::path::Path) -> Option<String> {
    let parent = path.parent()?;
    let same =
        parent == scenes_dir || (parent.canonicalize().ok()? == scenes_dir.canonicalize().ok()?);
    if !same || path.extension().and_then(|e| e.to_str()) != Some("ron") {
        return None;
    }
    let name = path.file_stem()?.to_str()?.to_string();
    project::valid_scene_name(&name).then_some(name)
}

/// Remembered projects that still hold a scene, most-recently-edited first.
fn sorted_recent_projects() -> Vec<RecentProject> {
    let mut projects: Vec<RecentProject> = read_recent_projects()
        .into_iter()
        .filter_map(|root| {
            find_scene(&root)?;
            let name = project_display_name(&root);
            let modified = last_edited(&root);
            let manifest = read_manifest(&root);
            Some(RecentProject {
                root: root.clone(),
                name,
                description: manifest.description,
                version: manifest.version,
                modified,
            })
        })
        .collect();
    projects.sort_by_key(|p| p.modified);
    projects.reverse();
    projects
}

/// Format a scene's last-edited time as a local date and time for the launcher.
fn format_edited(modified: Option<std::time::SystemTime>) -> String {
    match modified {
        Some(t) => chrono::DateTime::<chrono::Local>::from(t)
            .format("%b %e, %Y at %H:%M")
            .to_string(),
        None => "unknown".to_string(),
    }
}

/// egui's record of a repaint request, shortened to the file inside its crate,
/// the line and the reason.
fn short_cause(cause: &egui::RepaintCause) -> String {
    let file = cause
        .file
        .rsplit_once("/src/")
        .map_or(cause.file, |(_, f)| f);
    let crate_name = cause
        .file
        .split('/')
        .find(|part| part.starts_with("egui") || part.starts_with("frame-"))
        .unwrap_or("");
    format!("{crate_name} {file}:{} {}", cause.line, cause.reason)
        .trim()
        .to_string()
}

/// Paint the performance overlay in the viewport's top-right corner: plain
/// monospace lines on a dark box, so it reads over any scene.
fn paint_stats(ui: &egui::Ui, lines: &[String]) {
    let painter = ui.painter();
    let font = egui::FontId::monospace(12.0);
    let galley = painter.layout_no_wrap(
        lines.join("\n"),
        font,
        egui::Color32::from_rgb(0xe8, 0xe8, 0xe8),
    );
    let margin = egui::vec2(8.0, 6.0);
    let area = ui.max_rect();
    let size = galley.size() + margin * 2.0;
    let rect = egui::Rect::from_min_size(
        egui::pos2(area.right() - size.x - 8.0, area.top() + 8.0),
        size,
    );
    painter.rect_filled(rect, 4.0, egui::Color32::from_black_alpha(170));
    painter.galley(rect.min + margin, galley, egui::Color32::WHITE);
}

/// Build per-primitive instance buckets from a world, in the engine's Mesh order
/// (Cube, Sphere, Plane). Colliding entities are tinted toward red (a debug
/// flag); the selected entity, if any, is highlighted. Shared by the editor
/// viewport and the play window.
fn build_instances(
    world: &World,
    selected: Option<usize>,
    colliding: &std::collections::HashSet<usize>,
    custom_names: &[String],
    // Extension component names whose entities are only anchors for a
    // module's own geometry (see `module_api`): they are not drawn here.
    anchors: &[&str],
    // An entity to leave out entirely, not drawn at all. Used when rendering
    // from a Camera entity's own point of view (the Inspector preview, or
    // Play mode with a Camera in the scene): without this, a first-person
    // camera renders its own mesh with its eye sitting somewhere inside that
    // mesh's own geometry, so the view is just the inside of a solid shape,
    // however small the mesh's scale — the same reason a first-person game
    // hides the player's own body from their own camera. `None` for every
    // other caller (the main orbit viewport, Play with no Camera), which
    // draws every entity as normal.
    exclude: Option<usize>,
    // The render origin (see `origin`): every position is drawn relative to it.
    origin: DVec3,
) -> (Vec<InstanceRaw>, Vec<u32>) {
    // One bucket per mesh: the three primitives, then the imported models in
    // the same sorted name order the mesh buffer uses.
    let mut buckets: Vec<Vec<InstanceRaw>> = vec![Vec::new(); 3 + custom_names.len()];
    for (id, slot) in world.positions.iter().enumerate() {
        if Some(id) == exclude {
            continue;
        }
        // A UI entity is drawn as a screen-space overlay, not as 3D
        // geometry; same reasoning as the camera-self exclusion above, just
        // for every such entity instead of one.
        if world.ui_texts.get(id).is_some() || world.ui_images.get(id).is_some() {
            continue;
        }
        if anchors.iter().any(|name| world.ext_has(name, id)) {
            continue;
        }
        let Some(p) = slot.as_ref() else { continue };
        let mesh = world.meshes.get(id).cloned().unwrap_or_default();
        let bucket = match &mesh {
            // Nothing to draw.
            Mesh::Empty => continue,
            Mesh::Cube => 0,
            Mesh::Sphere => 1,
            Mesh::Plane => 2,
            // An imported model draws with its own vertices. A name with no
            // loaded model falls back to the cube.
            Mesh::Custom(name) => match custom_names.iter().position(|n| n == name) {
                Some(i) => 3 + i,
                None => 0,
            },
        };
        let color = world.colors.get(id).copied().unwrap_or_default();
        let scale = world.scales.get(id).copied().unwrap_or_default();
        let material = world.materials.get(id).copied().unwrap_or_default();
        let rotation = world.rotations.get(id).copied().unwrap_or_default();
        let rgb = if colliding.contains(&id) {
            const T: f32 = 0.6; // how far toward red
            [
                color.r * (1.0 - T) + 1.0 * T,
                color.g * (1.0 - T) + 0.15 * T,
                color.b * (1.0 - T) + 0.15 * T,
            ]
        } else {
            [color.r, color.g, color.b]
        };
        let raw = InstanceRaw {
            position: origin::relative(DVec3::new(p.x, p.y, p.z), origin).to_array(),
            color: rgb,
            selected: if Some(id) == selected { 1.0 } else { 0.0 },
            scale: [scale.x, scale.y, scale.z],
            emissive: material.emissive,
            rotation: [rotation.yaw, rotation.pitch, rotation.roll],
        };
        buckets[bucket].push(raw);
    }
    let group_counts: Vec<u32> = buckets.iter().map(|b| b.len() as u32).collect();
    let instances: Vec<InstanceRaw> = buckets.into_iter().flatten().collect();
    (instances, group_counts)
}

/// Gather up to MAX_LIGHTS active Light entities from the world into the
/// fixed-size array the shader's uniform buffer expects, in entity id order.
/// A Light entity beyond the cap is silently not drawn, a real, named
/// limitation rather than an unbounded per-frame cost. Unused slots are
/// left zeroed (intensity 0.0), which the shader's loop reads as "nothing
/// here".
fn build_lights(world: &World, origin: DVec3) -> [LightRaw; MAX_LIGHTS] {
    let mut out = [LightRaw::zeroed(); MAX_LIGHTS];
    let mut count = 0usize;
    // Only the first directional light casts the scene's shadow.
    let mut shadow_caster_assigned = false;
    for (id, slot) in world.lights.iter().enumerate() {
        if count >= MAX_LIGHTS {
            break;
        }
        let Some(light) = slot.as_ref() else {
            continue;
        };
        let (position_or_direction, kind_flag, range) = match light.kind {
            LightKind::Directional { direction } => {
                ([direction[0], direction[1], direction[2], 0.0], 0.0, 0.0)
            }
            LightKind::Point { range } => {
                // Position has no Default (every entity is meant to be
                // spawned with an explicit one), so a Point light on a
                // slot with no position at all falls back to the world
                // origin rather than relying on one.
                let p = world.positions.get(id).copied().unwrap_or(Position {
                    x: 0.0,
                    y: 0.0,
                    z: 0.0,
                });
                let r = origin::relative(DVec3::new(p.x, p.y, p.z), origin);
                ([r.x, r.y, r.z, 0.0], 1.0, range)
            }
        };
        let casts_shadow = kind_flag < 0.5 && light.intensity > 0.0 && !shadow_caster_assigned;
        if casts_shadow {
            shadow_caster_assigned = true;
        }
        out[count] = LightRaw {
            position_or_direction,
            params: [
                kind_flag,
                range,
                light.intensity,
                if casts_shadow { 1.0 } else { 0.0 },
            ],
        };
        count += 1;
    }
    // With a time-of-day (or module) sun, it turns the scene's sun light.
    sun::apply_to_lights(&mut out, sun::current(), sun::current_moon());
    out
}

/// Turn each entity's pending `Sound.play` request into an actual sound,
/// clearing the flag once handled either way (a missing file or no audio
/// device still consumes the request, rather than retrying it every tick
/// forever). Returns log lines for the caller to print, since logging here
/// directly would need `&mut self` while `world` is already borrowed from
/// either `self.world` or `self.game_world`, the same reason this is a
/// free function taking `world` explicitly rather than an `App` method: a
/// method could only ever reach `self.world`, never the Play window's
/// separately-held `self.game_world`.
///
/// `assets_root` is the open project's `assets/` folder; a `Sound.name`
/// resolves under it, the same convention `Mesh::Custom` already uses for
/// imported models. No audio device (`audio_manager` is `None`) or no
/// project open (`assets_root` is `None`) both mean requests are quietly
/// dropped rather than erroring, since a Sound component is still valid
/// data either way, it just can't play right now.
/// Audio formats the editor can import and play, matching the features
/// `kira` is built with by default.
const AUDIO_EXTENSIONS: &[&str] = &["wav", "ogg", "mp3", "flac"];

/// Find a sound's file under `assets`. A Sound stores just a file name
/// ("footstep.wav"), so the file can be moved between asset folders without
/// breaking anything, the same way a model is found by name. An exact path
/// under assets/ is tried first, so an older scene that stored a relative
/// path ("audio/footstep.wav") still works. Otherwise every folder is
/// searched, in sorted order so the result is the same every time, for a
/// file with that name.
fn find_audio(assets: &std::path::Path, name: &str) -> Option<std::path::PathBuf> {
    find_in_assets(assets, name)
}

/// Find a file under an `assets/` folder by name, by the same rule Play uses
/// on a project's folder or pack (`ProjectFiles::find_by_name`).
fn find_in_assets(assets: &std::path::Path, name: &str) -> Option<std::path::PathBuf> {
    let found = ProjectFiles::Folder(assets.to_path_buf()).find_by_name("", name)?;
    Some(assets.join(found))
}

/// Every audio file name under `assets`, sorted and without duplicates, for
/// the Inspector's Sound picker.
fn list_audio(assets: &std::path::Path) -> Vec<String> {
    let mut names = Vec::new();
    walk_files(assets, &mut |path| {
        let is_audio = path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| AUDIO_EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
            .unwrap_or(false);
        if is_audio {
            if let Some(name) = path.file_name().and_then(|f| f.to_str()) {
                names.push(name.to_string());
            }
        }
    });
    names.sort();
    names.dedup();
    names
}

/// Image formats the editor can import and use as a material texture.
/// PNG-only for now, matching the `image` crate's enabled feature set (see
/// Cargo.toml) — no new dependency feature added just for this first pass.
const IMAGE_EXTENSIONS: &[&str] = &["png"];

/// Find a texture's file under `assets`, the same "by name, anywhere under
/// assets/" resolution `find_audio` gives Sound, so a texture can be moved
/// between asset folders without breaking a mesh's material.
fn find_image(assets: &std::path::Path, name: &str) -> Option<std::path::PathBuf> {
    find_in_assets(assets, name)
}

/// Every image file name under `assets`, sorted and without duplicates, for
/// the Assets tab's material texture picker.
fn list_images(assets: &std::path::Path) -> Vec<String> {
    let mut names = Vec::new();
    walk_files(assets, &mut |path| {
        let is_image = path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| IMAGE_EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
            .unwrap_or(false);
        if is_image {
            if let Some(name) = path.file_name().and_then(|f| f.to_str()) {
                names.push(name.to_string());
            }
        }
    });
    names.sort();
    names.dedup();
    names
}

/// Build GPU-ready material data for a set of custom meshes, in the same
/// name order `custom_names`/`mesh_meta` already share (see
/// `MaterialGpuData`'s own doc comment for why decoding is cached). A mesh
/// with no entry in `mesh_meta` yet (freshly imported, before its first
/// material edit) gets `MeshMaterial::default()` — untextured, matching how
/// it always looked before materials existed.
fn build_material_gpu_data(
    names: &[String],
    mesh_meta: &std::collections::BTreeMap<String, frame_engine::world::MeshMeta>,
    project: Option<&ProjectFiles>,
    cache: &mut std::collections::HashMap<String, (u32, u32, Vec<u8>)>,
) -> Vec<MaterialGpuData> {
    names
        .iter()
        .map(|name| {
            let material = mesh_meta
                .get(name)
                .map(|m| m.material.clone())
                .unwrap_or_default();
            let texture_rgba = material.texture.as_ref().and_then(|tex_name| {
                if let Some(cached) = cache.get(tex_name) {
                    return Some(cached.clone());
                }
                let project = project?;
                let path = project.find_by_name("assets", tex_name)?;
                let bytes = project.read(&path).ok()?;
                let img = image::load_from_memory(&bytes).ok()?.to_rgba8();
                let (width, height) = img.dimensions();
                let data = (width, height, img.into_raw());
                cache.insert(tex_name.clone(), data.clone());
                Some(data)
            });
            MaterialGpuData {
                texture_rgba,
                roughness: material.roughness,
                metalness: material.metalness,
            }
        })
        .collect()
}

/// Call `visit` for every file under `dir`, in every subfolder. Unreadable
/// folders are skipped rather than failing the whole walk.
fn walk_files(dir: &std::path::Path, visit: &mut dyn FnMut(&std::path::Path)) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        match entry.file_type() {
            Ok(t) if t.is_dir() => walk_files(&path, visit),
            Ok(t) if t.is_file() => visit(&path),
            _ => {}
        }
    }
}

fn update_sounds(
    world: &mut World,
    mut audio_manager: Option<&mut kira::AudioManager<kira::DefaultBackend>>,
    sound_cache: &mut std::collections::HashMap<String, kira::sound::static_sound::StaticSoundData>,
    project: Option<&ProjectFiles>,
) -> Vec<String> {
    let mut messages = Vec::new();
    let ids: Vec<usize> = world
        .sounds
        .iter()
        .enumerate()
        .filter_map(|(id, slot)| slot.as_ref().filter(|s| s.play).map(|_| id))
        .collect();
    for id in ids {
        // Read the name, then immediately clear the request flag: whatever
        // happens below (missing manager, missing file, decode error), this
        // one-shot request is spent, not retried next tick.
        let name = world
            .sounds
            .get(id)
            .map(|s| s.name.clone())
            .unwrap_or_default();
        if let Some(s) = world.sounds.get_mut(id) {
            s.play = false;
        }
        let Some(manager) = audio_manager.as_deref_mut() else {
            continue;
        };
        let Some(project) = project else {
            messages.push(format!(
                "Sound '{name}' requested on entity {id}, but no project is open."
            ));
            continue;
        };
        if !sound_cache.contains_key(&name) {
            let Some(path) = project.find_by_name("assets", &name) else {
                messages.push(format!(
                    "Couldn't find sound '{name}' anywhere under assets/"
                ));
                continue;
            };
            let bytes = match project.read(&path) {
                Ok(bytes) => bytes,
                Err(e) => {
                    messages.push(format!("Couldn't load sound '{name}': {e}"));
                    continue;
                }
            };
            // Decoded from memory, so a sound inside a pack plays the same
            // as one in a folder.
            match kira::sound::static_sound::StaticSoundData::from_cursor(std::io::Cursor::new(
                bytes,
            )) {
                Ok(data) => {
                    sound_cache.insert(name.clone(), data);
                }
                Err(e) => {
                    messages.push(format!("Couldn't load sound '{name}': {e:?}"));
                    continue;
                }
            }
        }
        // Cheap clone: StaticSoundData shares its decoded audio data through
        // an Arc, so this doesn't re-read or re-decode the file, it just
        // hands the manager another handle to the same samples.
        if let Some(data) = sound_cache.get(&name) {
            if let Err(e) = manager.play(data.clone()) {
                messages.push(format!("Couldn't play sound '{name}': {e:?}"));
            }
        }
    }
    messages
}

fn default_world() -> World {
    let mut world = World::new();
    world.spawn(
        Position {
            x: 0.0,
            y: 0.0,
            z: 40.0,
        },
        Velocity {
            dx: 0.0,
            dy: 0.0,
            dz: 0.0,
        },
    );
    world.spawn(
        Position {
            x: 40.0,
            y: 20.0,
            z: 0.0,
        },
        Velocity {
            dx: -0.3,
            dy: 0.3,
            dz: 0.0,
        },
    );
    world.spawn(
        Position {
            x: 0.0,
            y: 0.0,
            z: -50.0,
        },
        Velocity {
            dx: 0.0,
            dy: 0.0,
            dz: 0.6,
        },
    );
    world
}
static APP_IDENTITY: std::sync::OnceLock<(String, String)> = std::sync::OnceLock::new();

/// The name shown in window titles and the launcher heading.
fn app_name() -> &'static str {
    APP_IDENTITY
        .get()
        .map(|(name, _)| name.as_str())
        .unwrap_or("Frame Editor")
}

/// The Linux application id, which the desktop uses to group windows and find
/// the launcher entry and icon.
// Only Linux uses an application id (Wayland groups windows by it).
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn app_id() -> &'static str {
    APP_IDENTITY
        .get()
        .map(|(_, id)| id.as_str())
        .unwrap_or("frame-editor")
}

/// Start the editor. `modules` are features added by other crates (see
/// `module_api`); the public editor passes none.
pub fn run(modules: Vec<Box<dyn EditorModule>>) {
    run_as("Frame Editor", "frame-editor", modules);
}

/// Like `run`, with its own name (window titles, launcher heading) and Linux
/// application id, so a build with extra features can be told apart from the
/// plain editor in the window switcher.
pub fn run_as(name: &str, app_id: &str, modules: Vec<Box<dyn EditorModule>>) {
    run_app(name, app_id, modules, None, None);
}

/// Start straight into a game, with no editor: the project in `project` (a
/// folder holding its scene, or a pack made by `write_pack`, opened with
/// `cipher` if it is sealed) opens in one window, the same way the editor's
/// Play button runs it, and closing that window quits. `name` is the window
/// title and `app_id` the Linux application id. This is what a game build
/// runs.
pub fn run_player(
    name: &str,
    app_id: &str,
    project: std::path::PathBuf,
    cipher: Option<Arc<dyn PackCipher>>,
    modules: Vec<Box<dyn EditorModule>>,
) {
    start_player(name, app_id, project, cipher, modules, None);
}

/// Like `run_player`, opening on the game's own `menu` (see `GameMenu`)
/// with the world running behind it, instead of straight into play. Esc
/// twice during play brings the menu back.
pub fn run_player_with_menu(
    name: &str,
    app_id: &str,
    project: std::path::PathBuf,
    cipher: Option<Arc<dyn PackCipher>>,
    modules: Vec<Box<dyn EditorModule>>,
    menu: Box<dyn GameMenu>,
) {
    start_player(name, app_id, project, cipher, modules, Some(menu));
}

fn start_player(
    name: &str,
    app_id: &str,
    project: std::path::PathBuf,
    cipher: Option<Arc<dyn PackCipher>>,
    modules: Vec<Box<dyn EditorModule>>,
    menu: Option<Box<dyn GameMenu>>,
) {
    let files = match ProjectFiles::open(&project, cipher) {
        Ok(files) => files,
        Err(e) => {
            report_player_error(
                name,
                &format!(
                    "{name} could not open its game files ({}): {e}",
                    project.display()
                ),
            );
            std::process::exit(1);
        }
    };
    install_crash_log(name);
    run_app(name, app_id, modules, Some(files), menu);
}

/// Tell a player their game couldn't start: on standard error, and in a
/// message box on Windows, where a game has no console to print to (release
/// builds hide it).
pub fn report_player_error(name: &str, message: &str) {
    eprintln!("{message}");
    #[cfg(target_os = "windows")]
    {
        let _ = rfd::MessageDialog::new()
            .set_level(rfd::MessageLevel::Error)
            .set_title(name)
            .set_description(message)
            .set_buttons(rfd::MessageButtons::Ok)
            .show();
    }
    #[cfg(not(target_os = "windows"))]
    let _ = name;
}

/// Where a game writes `crash-log.txt`: next to the program, so a tester
/// finds it beside the game, or the temporary folder if that can't be
/// written to.
fn crash_log_path() -> std::path::PathBuf {
    let beside = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join("crash-log.txt")));
    match beside {
        Some(path)
            if std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
                .is_ok() =>
        {
            path
        }
        _ => std::env::temp_dir().join("crash-log.txt"),
    }
}

/// On a crash (a panic, on any thread), still print it as usual, but also
/// write what happened to `crash-log.txt` and tell the player where it is.
/// Only the first crash is reported: one fault can set off others.
fn install_crash_log(name: &str) {
    static REPORTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    let name = name.to_string();
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        previous(info);
        if REPORTED.swap(true, std::sync::atomic::Ordering::SeqCst) {
            return;
        }
        let text = format!(
            "{name} crashed.\n\n{info}\n\nFrame Engine {}, {} {}\n\n{}\n",
            frame_engine::version::ENGINE,
            std::env::consts::OS,
            std::env::consts::ARCH,
            std::backtrace::Backtrace::force_capture()
        );
        let path = crash_log_path();
        let message = match std::fs::write(&path, text) {
            Ok(()) => format!(
                "{name} crashed. What happened was saved to:\n{}\n\nPlease send that file to the developer.",
                path.display()
            ),
            Err(_) => format!("{name} crashed.\n\n{info}"),
        };
        report_player_error(&name, &message);
    }));
}

/// The editor, or with `player` set, a game on its own (see `run_player`).
fn run_app(
    name: &str,
    app_id: &str,
    modules: Vec<Box<dyn EditorModule>>,
    player: Option<ProjectFiles>,
    game_menu: Option<Box<dyn GameMenu>>,
) {
    let _ = APP_IDENTITY.set((name.to_string(), app_id.to_string()));
    let event_loop = EventLoop::new().unwrap();
    // The editor opens on the launcher screen with no project loaded, so it
    // starts from an empty world; creating or opening a project replaces it.
    let world = World::default();
    let prefs = load_prefs();
    set_scene_tree_drag_hold_seconds(prefs.drag_hold_seconds);
    set_shadows_enabled(prefs.shadows);
    set_shadow_distance(prefs.shadow_distance);
    set_sky_enabled(prefs.sky);
    set_fog_amount(prefs.fog_amount);
    set_cloud_amount(prefs.clouds);
    set_grass_quality(prefs.grass);
    apply_sun_prefs(&prefs);
    // A missing/unusable audio device (headless, no default output) is a
    // real, expected case, not a crash: the editor still runs, sounds just
    // silently don't play, the same graceful-degradation the game window
    // already gives a closed window. Everything heard goes through a
    // low-pass filter, dry until it is wanted (under water).
    let mut audio_settings = kira::AudioManagerSettings::<kira::DefaultBackend>::default();
    let muffle = audio_settings.main_track_builder.add_effect(
        kira::effect::filter::FilterBuilder::new()
            .cutoff(UNDERWATER_CUTOFF)
            .mix(kira::Mix::DRY),
    );
    let audio_manager = kira::AudioManager::<kira::DefaultBackend>::new(audio_settings)
        .map_err(|e| eprintln!("Audio device unavailable, sounds won't play: {e:?}"))
        .ok();
    let audio_muffle = audio_manager.is_some().then_some(muffle);
    let mut app = App {
        window: None,
        gpu: None,
        gpu_shared: None,
        world,
        // Same shared gravity as the Play window and the engine's own
        // standalone demo; see `physics::GRAVITY_Y`.
        physics: Physics::new(GRAVITY_Y),
        undo_stack: Vec::new(),
        redo_stack: Vec::new(),
        inspector_editing: false,
        ctrl_held: false,
        shift_held: false,
        mode: AppMode::Launcher,
        project_name: None,
        recent_projects: sorted_recent_projects(),
        new_project_name: String::new(),
        settings_open: false,
        pending_delete: None,
        settings_root: None,
        settings_orig_name: String::new(),
        settings_name: String::new(),
        settings_description: String::new(),
        settings_version: String::new(),
        // No scene target until a project is opened.
        current_scene_path: None,
        project_dir: None,
        custom_meshes: std::collections::BTreeMap::new(),
        texture_cache: std::collections::HashMap::new(),
        material_editing: None,
        prefabs: std::collections::BTreeMap::new(),
        new_prefab_name: String::new(),
        game_custom_names: Vec::new(),
        assets_subdir: std::path::PathBuf::new(),
        new_asset_folder: String::new(),
        thumbnails: std::collections::HashMap::new(),
        move_pending: None,
        git_summary: None,
        git_refresh_at: std::time::Instant::now(),
        paused: false,
        clock: Clock::new(TICK_RATE, MAX_CATCHUP_TICKS),
        cam_focus_x: 0.0,
        cam_focus_y: 0.0,
        cam_distance: 150.0,
        // Gentle default tilt so the cubes read as 3D on launch. Zero both for
        // the old straight-down-Z view.
        cam_yaw: 0.5,
        cam_pitch: 0.3,
        dragging: false,
        orbiting: false,
        panning: false,
        fly_speed: prefs.fly_speed,
        look_sensitivity: prefs.look_sensitivity,
        orbit_sensitivity: prefs.orbit_sensitivity,
        invert_look_x: prefs.invert_look_x,
        invert_look_y: prefs.invert_look_y,
        drag_hold_seconds: prefs.drag_hold_seconds,
        gizmo: None,
        gizmo_drag: None,
        gizmo_hover: None,
        last_cursor: (0.0, 0.0),
        selected: None,
        show_help: prefs.show_help,
        show_stats: prefs.show_stats,
        perf: perf::FrameStats::new(std::time::Instant::now()),
        show_play_stats: false,
        play_perf: perf::FrameStats::frames_only(std::time::Instant::now()),
        play_panels_open: false,
        redraw_reason: None,
        idle_wake: None,
        wave_clock: None,
        egui_cause: None,
        dirty: false,
        saved_scripts: std::collections::BTreeMap::new(),
        pending_unsaved: None,
        pending_scene_switch: None,
        scenes_tab: scenes::ScenesTabState::default(),
        version_notice: None,
        player_files: None,
        player_failed: false,
        window_title: String::new(),
        saved_prefs: prefs,
        egui_ctx: egui::Context::default(),
        egui_state: None,
        // Default layout mirrors the old editor: the Viewport and Script Editor
        // are tabs filling the centre (click Script Editor to edit full-width
        // over the viewport), with Scene and Inspector docked on the right. All
        // of it is draggable, tabbable, and splittable at runtime.
        dock_state: {
            let mut state = egui_dock::DockState::new(vec![Tab::Viewport, Tab::Scripts]);
            state.main_surface_mut().split_right(
                egui_dock::NodeIndex::root(),
                0.78,
                vec![Tab::Scene, Tab::Inspector, Tab::Source],
            );
            state
        },
        viewport_rect: None,
        fly_mode: false,
        cam_focus_z: 0.0,
        cam_eye: DVec3::ZERO,
        fly_picked: false,
        console_tab: ConsoleTab::Output,
        log_lines: vec!["Frame Editor started.".to_string()],
        input: InputState::new(),
        game_window: None,
        game_gpu: None,
        modules: Rc::new(RefCell::new(modules)),
        game_world: None,
        game_physics: None,
        game_egui_ctx: None,
        game_egui_state: None,
        game_ui_textures: std::collections::HashMap::new(),
        game_files: None,
        game_input: InputState::new(),
        game_clock: Clock::new(TICK_RATE, MAX_CATCHUP_TICKS),
        game_closing: false,
        game_mouse_held: false,
        menu_open: game_menu.is_some(),
        game_session: None,
        game_menu,
        menu_opened: std::time::Instant::now(),
        game_volume: 1.0,
        applied_volume: None,
        game_audio: game_audio::GameAudio::default(),
        game_audio_at: None,
        popped_out: std::collections::HashMap::new(),
        popped_out_closing: Vec::new(),
        new_script_name: String::new(),
        script_filter: String::new(),
        open_script: None,
        renaming: None,
        plugins_panel_open: false,
        editor_settings_open: false,
        build_window_open: false,
        build_settings: build::BuildSettings::default(),
        build_job: None,
        build_progress: Vec::new(),
        build_result: None,
        script_runtime: script::RhaiRuntime::new(),
        logo_texture: None,
        audio_manager,
        audio_muffle,
        muffled: false,
        sound_cache: std::collections::HashMap::new(),
    };
    match player {
        Some(files) => {
            app.mode = AppMode::Player;
            app.player_files = Some(files);
            println!("{name} starting.");
        }
        None => println!("Frame Editor started at the launcher."),
    }
    event_loop.run_app(&mut app).unwrap();
    if app.player_failed {
        std::process::exit(1);
    }
}

#[cfg(test)]
mod prefs_tests {
    use super::*;

    #[test]
    fn vsync_is_on_unless_turned_off_and_an_old_prefs_file_keeps_it_on() {
        assert!(EditorPrefs::default().vsync);
        let old: EditorPrefs = ron::from_str("(show_help: true)").unwrap();
        assert!(old.vsync);
        assert!(!old.show_stats);
        use wgpu::PresentMode as P;
        assert_eq!(present_mode_for(true), P::Fifo);
        assert_eq!(present_mode_for(false), P::AutoNoVsync);
        // Without vsync the fastest offered mode wins; with it, plain Fifo.
        let offered = [P::Fifo, P::Mailbox, P::Immediate];
        assert_eq!(
            resolved_present_mode(P::AutoNoVsync, &offered),
            P::Immediate
        );
        assert_eq!(
            resolved_present_mode(P::AutoNoVsync, &[P::Fifo, P::Mailbox]),
            P::Mailbox
        );
        assert_eq!(resolved_present_mode(P::Fifo, &offered), P::Fifo);
        assert_eq!(resolved_present_mode(P::AutoVsync, &[P::Fifo]), P::Fifo);
    }

    #[test]
    fn old_editor_ron_still_loads_with_defaults() {
        let p: EditorPrefs = ron::from_str("(show_help: false)").unwrap();
        assert!(!p.show_help);
        assert_eq!(p.fly_speed, CAM_PAN_SPEED);
        assert_eq!(p.look_sensitivity, 1.0);
        assert_eq!(p.drag_hold_seconds, SCENE_TREE_DRAG_HOLD_DEFAULT);
        assert!(p.shadows);
        assert_eq!(p.shadow_distance, SHADOW_DISTANCE_DEFAULT);
        assert!(p.sky);
        assert_eq!(p.fog_amount, sky::FOG_AMOUNT_DEFAULT);
    }

    #[test]
    fn sky_preferences_are_kept_in_range() {
        let p = EditorPrefs {
            fog_amount: 99.0,
            ..EditorPrefs::default()
        }
        .sanitized();
        assert_eq!(p.fog_amount, sky::FOG_AMOUNT_MAX);
        let p = EditorPrefs {
            fog_amount: f32::NAN,
            sky: false,
            ..EditorPrefs::default()
        }
        .sanitized();
        assert_eq!(p.fog_amount, sky::FOG_AMOUNT_DEFAULT);
        assert!(!p.sky);
        let p = EditorPrefs {
            clouds: 3.0,
            ..EditorPrefs::default()
        }
        .sanitized();
        assert_eq!(p.clouds, 1.0);
        let p = EditorPrefs {
            clouds: f32::NAN,
            ..EditorPrefs::default()
        }
        .sanitized();
        assert_eq!(p.clouds, sky::CLOUDS_DEFAULT);
    }

    #[test]
    fn the_sun_is_the_shadow_casting_light_then_any_directional_light() {
        let mut lights = [LightRaw::zeroed(); MAX_LIGHTS];
        assert_eq!(sun_direction(&lights), None, "no lights, no sun");
        // A point light is never the sun.
        lights[0] = LightRaw {
            position_or_direction: [5.0, 5.0, 5.0, 0.0],
            params: [1.0, 50.0, 1.0, 0.0],
        };
        assert_eq!(sun_direction(&lights), None);
        // A directional light that is not the shadow caster still counts...
        lights[1] = LightRaw {
            position_or_direction: [0.0, 1.0, 0.0, 0.0],
            params: [0.0, 0.0, 1.0, 0.0],
        };
        assert_eq!(sun_direction(&lights), Some([0.0, 1.0, 0.0]));
        // ...but the shadow caster wins.
        lights[2] = LightRaw {
            position_or_direction: [1.0, 0.5, 0.0, 0.0],
            params: [0.0, 0.0, 1.0, 1.0],
        };
        assert_eq!(sun_direction(&lights), Some([1.0, 0.5, 0.0]));
        // A switched-off light (intensity 0) is not a sun.
        let mut off = [LightRaw::zeroed(); MAX_LIGHTS];
        off[0] = LightRaw {
            position_or_direction: [0.0, 1.0, 0.0, 0.0],
            params: [0.0, 0.0, 0.0, 0.0],
        };
        assert_eq!(sun_direction(&off), None);
    }

    #[test]
    fn prefs_round_trip_and_sanitize() {
        let p = EditorPrefs {
            fly_speed: 12.5,
            invert_look_y: true,
            ..EditorPrefs::default()
        };
        let text = ron::to_string(&p).unwrap();
        let back: EditorPrefs = ron::from_str(&text).unwrap();
        assert!(back == p);
        let bad = EditorPrefs {
            fly_speed: f32::NAN,
            look_sensitivity: 99.0,
            ..EditorPrefs::default()
        }
        .sanitized();
        assert_eq!(bad.fly_speed, CAM_PAN_SPEED);
        assert_eq!(bad.look_sensitivity, 5.0);
    }
}

#[cfg(test)]
mod shader_tests {
    use wgpu::naga;

    /// Bindings of the scene group that only the vertex stage can see (see
    /// the scene bind group layout in `GpuState::new`).
    const VERTEX_ONLY: [(u32, u32); 1] = [(0, 0)];
    /// And those only the fragment stage can see.
    const FRAGMENT_ONLY: [(u32, u32); 4] = [(0, 1), (0, 2), (0, 3), (0, 4)];

    #[test]
    fn vertex_shaders_only_read_bindings_the_vertex_stage_can_see() {
        for source in [super::ENTITY_WGSL, include_str!("shadow.wgsl")] {
            let module = naga::front::wgsl::parse_str(source).unwrap();
            let info = naga::valid::Validator::new(
                naga::valid::ValidationFlags::all(),
                naga::valid::Capabilities::all(),
            )
            .validate(&module)
            .unwrap();
            for (i, entry) in module.entry_points.iter().enumerate() {
                if entry.stage != naga::ShaderStage::Vertex {
                    continue;
                }
                let uses = info.get_entry_point(i);
                for (handle, global) in module.global_variables.iter() {
                    let Some(binding) = &global.binding else {
                        continue;
                    };
                    let used = !uses[handle].is_empty();
                    assert!(
                        !(used && FRAGMENT_ONLY.contains(&(binding.group, binding.binding))),
                        "{} reads {:?} (group {}, binding {}), which only the fragment stage sees",
                        entry.name,
                        global.name,
                        binding.group,
                        binding.binding
                    );
                }
            }
        }
    }

    #[test]
    fn fragment_shaders_only_read_bindings_the_fragment_stage_can_see() {
        for source in [super::ENTITY_WGSL, include_str!("shadow.wgsl")] {
            let module = naga::front::wgsl::parse_str(source).unwrap();
            let info = naga::valid::Validator::new(
                naga::valid::ValidationFlags::all(),
                naga::valid::Capabilities::all(),
            )
            .validate(&module)
            .unwrap();
            for (i, entry) in module.entry_points.iter().enumerate() {
                if entry.stage != naga::ShaderStage::Fragment {
                    continue;
                }
                let uses = info.get_entry_point(i);
                for (handle, global) in module.global_variables.iter() {
                    let Some(binding) = &global.binding else {
                        continue;
                    };
                    let used = !uses[handle].is_empty();
                    assert!(
                        !(used && VERTEX_ONLY.contains(&(binding.group, binding.binding))),
                        "{} reads {:?} (group {}, binding {}), which only the vertex stage sees",
                        entry.name,
                        global.name,
                        binding.group,
                        binding.binding
                    );
                }
            }
        }
    }

    fn validate(source: &str) {
        let module = naga::front::wgsl::parse_str(source).expect("WGSL should parse");
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        )
        .validate(&module)
        .expect("WGSL should validate");
    }

    #[test]
    fn entity_shader_is_valid() {
        validate(super::ENTITY_WGSL);
    }

    #[test]
    fn entity_shader_has_the_entry_points_the_pipelines_name() {
        let module = naga::front::wgsl::parse_str(super::ENTITY_WGSL).unwrap();
        for name in ["vs_main", "fs_main", "fs_transparent", "vs_sky", "fs_sky"] {
            assert!(
                module.entry_points.iter().any(|e| e.name == name),
                "missing entry point {name}"
            );
        }
    }

    #[test]
    fn shadow_shader_is_valid() {
        validate(include_str!("shadow.wgsl"));
    }

    /// The size the shader gives a uniform, by its variable name.
    fn uniform_size(source: &str, name: &str) -> u32 {
        let module = naga::front::wgsl::parse_str(source).unwrap();
        let mut layouter = naga::proc::Layouter::default();
        layouter.update(module.to_ctx()).unwrap();
        let (_, var) = module
            .global_variables
            .iter()
            .find(|(_, v)| v.name.as_deref() == Some(name))
            .expect("uniform declared");
        layouter[var.ty].size
    }

    #[test]
    fn the_uniforms_are_the_same_size_on_both_sides() {
        let main = super::ENTITY_WGSL;
        let shadow = include_str!("shadow.wgsl");
        let camera = std::mem::size_of::<super::CameraUniform>() as u32;
        let shadow_uniform = std::mem::size_of::<super::ShadowUniform>() as u32;
        assert_eq!(uniform_size(main, "camera"), camera);
        assert_eq!(uniform_size(main, "shadow"), shadow_uniform);
        assert_eq!(uniform_size(shadow, "shadow_cam"), shadow_uniform);
    }

    #[test]
    fn the_vertex_layout_matches_the_shader_inputs() {
        let module = naga::front::wgsl::parse_str(super::ENTITY_WGSL).unwrap();
        let entry = module
            .entry_points
            .iter()
            .find(|e| e.name == "vs_main")
            .unwrap();
        let vertex_ty = entry.function.arguments[0].ty;
        let naga::TypeInner::Struct { members, .. } = &module.types[vertex_ty].inner else {
            panic!("vertex input is a struct");
        };
        let locations: Vec<u32> = members
            .iter()
            .filter_map(|m| match m.binding {
                Some(naga::Binding::Location { location, .. }) => Some(location),
                _ => None,
            })
            .collect();
        let ours: Vec<u32> = super::MeshVertex::ATTRIBS
            .iter()
            .map(|a| a.shader_location)
            .collect();
        assert_eq!(locations, ours);
        let last = super::MeshVertex::ATTRIBS.last().unwrap();
        assert_eq!(
            (last.offset + last.format.size()) as usize,
            std::mem::size_of::<super::MeshVertex>()
        );
    }
}

#[cfg(test)]
mod shadow_fit_tests {
    use super::*;

    #[test]
    fn the_projection_has_no_far_plane_and_keeps_far_depths_apart() {
        let p = camera_projection(16.0 / 9.0);
        // Depth of a point straight ahead, `d` units out.
        let depth = |d: f32| {
            let c = p * Vec4::new(0.0, 0.0, -d, 1.0);
            c.z / c.w
        };
        assert!((depth(NEAR) - 1.0).abs() < 1e-6, "the near plane is 1");
        assert!(depth(10.0) > depth(1_000.0), "nearer is greater");
        // 100 km out is still in front of the camera and drawn.
        let far = depth(100_000.0);
        assert!(far > DEPTH_CLEAR && far < 1e-5, "{far}");
        // Two surfaces a metre apart 50 km away still get different depths.
        assert!(depth(50_000.0) > depth(50_001.0));
    }

    fn instance(position: [f32; 3], scale: f32) -> InstanceRaw {
        InstanceRaw {
            position,
            color: [1.0; 3],
            selected: 0.0,
            scale: [scale; 3],
            emissive: 0.0,
            rotation: [0.0; 3],
        }
    }

    fn sun() -> [LightRaw; MAX_LIGHTS] {
        let mut lights = [LightRaw::zeroed(); MAX_LIGHTS];
        lights[0] = LightRaw {
            position_or_direction: [-1.0, 1.0, 0.1, 0.0],
            params: [0.0, 0.0, 1.0, 1.0],
        };
        lights
    }

    fn camera() -> [[f32; 4]; 4] {
        let view = Mat4::look_at_rh(
            Vec3::new(0.0, 20.0, 40.0),
            Vec3::new(0.0, 0.0, -20.0),
            Vec3::Y,
        );
        let proj = camera_projection(16.0 / 9.0);
        (proj * view).to_cols_array_2d()
    }

    #[test]
    fn map_fits_the_view_not_the_huge_ground() {
        // A ground plane 1600 units wide and a cube near the camera.
        let instances = [
            instance([0.0, 0.0, 0.0], 200.0),
            instance([0.0, 5.0, -20.0], 1.0),
        ];
        let u = compute_shadow(&instances, &sun(), camera(), None);
        assert_eq!(u.params[0], 1.0);
        // Whole-scene fit would have texels of ~0.6 units; the view fit is sharper.
        let scene_fit_texel = 2.0 * 600.0 / SHADOW_MAP_SIZE as f32;
        assert!(u.params[2] / 1.5 < scene_fit_texel);
        // The cube is inside the map, with a valid depth.
        let clip = Mat4::from_cols_array_2d(&u.light_view_proj) * Vec4::new(0.0, 5.0, -20.0, 1.0);
        assert!(clip.x.abs() <= 1.0 && clip.y.abs() <= 1.0, "{clip:?}");
        assert!(clip.z >= 0.0 && clip.z <= 1.0, "{clip:?}");
    }

    #[test]
    fn small_scene_uses_the_whole_scene() {
        let instances = [
            instance([0.0, 0.0, 0.0], 1.0),
            instance([10.0, 0.0, 0.0], 1.0),
        ];
        let u = compute_shadow(&instances, &sun(), camera(), None);
        assert_eq!(u.params[0], 1.0);
    }

    #[test]
    fn no_shadow_caster_light_means_shadows_off() {
        let instances = [instance([0.0, 0.0, 0.0], 1.0)];
        let mut lights = sun();
        lights[0].params[3] = 0.0;
        assert_eq!(
            compute_shadow(&instances, &lights, camera(), None).params[0],
            0.0
        );
    }
}

#[cfg(test)]
mod rotation_shader_tests {
    use frame_engine::world::Rotation;

    // A line-for-line copy of `rotate_by` in shader.wgsl and shadow.wgsl, so
    // the hand-derived shader steps are checked against the engine's own
    // `Rotation::apply` (the shaders can't be run without a GPU).
    fn rotate_by(v: [f32; 3], angles: [f32; 3]) -> [f32; 3] {
        let (sr, cr) = angles[2].sin_cos();
        let rolled = [v[0] * cr + v[1] * sr, -v[0] * sr + v[1] * cr, v[2]];
        let (sp, cp) = angles[1].sin_cos();
        let pitched = [
            rolled[0],
            rolled[1] * cp - rolled[2] * sp,
            rolled[1] * sp + rolled[2] * cp,
        ];
        let (sy, cy) = angles[0].sin_cos();
        [
            pitched[0] * cy - pitched[2] * sy,
            pitched[1],
            pitched[0] * sy + pitched[2] * cy,
        ]
    }

    #[test]
    fn shader_rotation_steps_match_the_engines() {
        for yaw in [-2.0f32, 0.0, 0.9, 3.0] {
            for pitch in [-1.0f32, 0.0, 0.8] {
                for roll in [-1.5f32, 0.0, 2.2] {
                    let r = Rotation { yaw, pitch, roll };
                    for v in [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.3, -0.7, 2.0]] {
                        let a = rotate_by(v, [yaw, pitch, roll]);
                        let b = r.apply(v);
                        assert!(
                            a.iter().zip(b.iter()).all(|(p, q)| (p - q).abs() < 1e-5),
                            "{r:?} {v:?}: {a:?} vs {b:?}"
                        );
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod far_from_the_origin_tests {
    use super::*;
    use frame_engine::world::{Camera, Velocity};

    fn spawn_at(world: &mut World, x: f64, y: f64, z: f64) -> usize {
        world.spawn(
            Position { x, y, z },
            Velocity {
                dx: 0.0,
                dy: 0.0,
                dz: 0.0,
            },
        )
    }

    #[test]
    fn a_meshless_entity_is_not_drawn_and_saves_and_loads() {
        let mut world = World::new();
        let shown = spawn_at(&mut world, 0.0, 0.0, 0.0);
        let hidden = spawn_at(&mut world, 3.0, 0.0, 0.0);
        world.meshes.insert(hidden, Mesh::Empty);
        let (instances, counts) = build_instances(
            &world,
            None,
            &std::collections::HashSet::new(),
            &[],
            &[],
            None,
            DVec3::ZERO,
        );
        assert_eq!(instances.len(), 1, "entity {shown} only");
        assert_eq!(counts.iter().sum::<u32>(), 1);
        let text = ron::to_string(&world).expect("saves");
        let back: World = ron::from_str(&text).expect("loads");
        assert!(back.meshes.get(hidden) == Some(&Mesh::Empty));
        assert!(back.meshes.get(shown) == Some(&Mesh::Cube));
    }

    #[test]
    fn an_entity_twenty_thousand_km_out_is_drawn_exactly_where_it_is() {
        let eye = DVec3::new(2.0e7, 0.0, 0.0);
        let mut world = World::new();
        let id = spawn_at(&mut world, 2.0e7 + 5.25, 1.5, -3.125);
        let origin = origin::snap(eye);
        let (instances, _) = build_instances(
            &world,
            None,
            &std::collections::HashSet::new(),
            &[],
            &[],
            None,
            origin,
        );
        assert_eq!(instances.len(), 1);
        let drawn = instances[0].position;
        let back = [
            f64::from(drawn[0]) + origin.x,
            f64::from(drawn[1]) + origin.y,
            f64::from(drawn[2]) + origin.z,
        ];
        assert_eq!(back, [2.0e7 + 5.25, 1.5, -3.125], "entity {id}");
        assert!(
            drawn.iter().all(|v| v.abs() < 200.0),
            "small numbers: {drawn:?}"
        );
    }

    #[test]
    fn two_entities_keep_their_exact_spacing_far_from_the_origin() {
        let mut world = World::new();
        spawn_at(&mut world, 1.0e9 + 0.5, 0.0, 1.0e9);
        spawn_at(&mut world, 1.0e9 + 1.75, 0.0, 1.0e9);
        let origin = origin::snap(DVec3::new(1.0e9, 0.0, 1.0e9));
        let (instances, _) = build_instances(
            &world,
            None,
            &std::collections::HashSet::new(),
            &[],
            &[],
            None,
            origin,
        );
        let gap = instances[1].position[0] - instances[0].position[0];
        assert_eq!(
            gap, 1.25,
            "an f32 position at a billion units could not tell these apart"
        );
    }

    #[test]
    fn a_point_light_far_out_is_placed_relative_to_the_origin() {
        use frame_engine::world::{Light, LightKind};
        let mut world = World::new();
        let id = spawn_at(&mut world, 2.0e7 + 2.0, 4.0, 2.0e7 - 6.0);
        world.lights.insert(
            id,
            Light {
                kind: LightKind::Point { range: 30.0 },
                intensity: 1.0,
            },
        );
        let origin = origin::snap(DVec3::new(2.0e7, 0.0, 2.0e7));
        let lights = build_lights(&world, origin);
        assert_eq!(lights[0].position_or_direction[..3], [2.0, 4.0, -6.0]);
    }

    #[test]
    fn a_camera_far_out_looks_at_what_is_in_front_of_it() {
        // The camera and a target 10 units ahead of it (-Z at yaw 0), 20,000
        // km out: the target projects to the middle of the screen.
        let mut world = World::new();
        let cam = spawn_at(&mut world, 2.0e7 + 0.3, 2.0, 2.0e7 + 0.7);
        world.cameras.insert(cam, Camera);
        let origin = origin::snap(DVec3::new(2.0e7, 0.0, 2.0e7));
        let vp = Mat4::from_cols_array_2d(
            &camera_entity_view_proj(&world, cam, 800, 600, origin).unwrap(),
        );
        let target = origin::relative(DVec3::new(2.0e7 + 0.3, 2.0, 2.0e7 + 0.7 - 10.0), origin);
        let (x, y) = project(vp, target.x, target.y, target.z, 800.0, 600.0).unwrap();
        assert!(
            (x - 400.0).abs() < 0.01 && (y - 300.0).abs() < 0.01,
            "{x} {y}"
        );
    }

    #[test]
    fn the_orbit_camera_far_out_frames_its_focus() {
        // `view_matrix` for the orbit camera is the focus, relative to the
        // origin, through `camera_matrix`; the focus lands mid-screen.
        let focus = DVec3::new(2.0e7 + 12.5, 3.0, -2.0e7 + 0.125);
        let origin = origin::snap(focus);
        let f = origin::relative(focus, origin);
        let vp = camera_matrix(f.x, f.y, f.z, 50.0, 0.7, 0.4, 800, 600);
        let (x, y) = project(vp, f.x, f.y, f.z, 800.0, 600.0).unwrap();
        assert!(
            (x - 400.0).abs() < 0.01 && (y - 300.0).abs() < 0.01,
            "{x} {y}"
        );
    }
}

#[cfg(test)]
mod sun_prefs_tests {
    use super::*;

    #[test]
    fn the_time_of_day_sun_is_off_by_default_and_an_old_prefs_file_still_loads() {
        let p = EditorPrefs::default();
        assert!(!p.sun_time);
        let old: EditorPrefs = ron::from_str("(fly_speed: 30.0)").expect("old file loads");
        assert!(!old.sun_time);
        // A file written by the earlier clock build still loads.
        let older: EditorPrefs =
            ron::from_str("(clock: true, day_hours: 13.0, year_days: 120.0)").expect("loads");
        assert!(!older.sun_time);
    }

    #[test]
    fn the_time_of_day_is_kept_in_range() {
        let p = EditorPrefs {
            time_of_day: 99.0,
            ..EditorPrefs::default()
        }
        .sanitized();
        assert_eq!(p.time_of_day, 24.0);
        let p = EditorPrefs {
            time_of_day: f32::NAN,
            ..EditorPrefs::default()
        }
        .sanitized();
        assert_eq!(p.time_of_day, sun::DEFAULT_HOUR);
    }
}

#[cfg(test)]
mod ambient_tests {
    use super::*;

    #[test]
    fn the_ambient_light_is_untouched_while_the_cycle_is_off() {
        let lights = [LightRaw::zeroed(); MAX_LIGHTS];
        let uniform = sky_uniform_for_frame(
            Mat4::IDENTITY.to_cols_array_2d(),
            &lights,
            true,
            0.0,
            [0.0; 2],
            [0.0; 2],
        );
        assert_eq!(uniform.ground[3], 1.0);
    }

    #[test]
    fn under_a_liquid_everything_is_its_colour_in_a_thick_fog() {
        let lights = [LightRaw::zeroed(); MAX_LIGHTS];
        let above = sky_uniform_for_frame(
            Mat4::IDENTITY.to_cols_array_2d(),
            &lights,
            true,
            0.0,
            [0.0; 2],
            [0.0; 2],
        );
        let view = module_api::Underwater {
            color: [0.1, 0.3, 0.35],
            visibility: 12.0,
        };
        let under = under_liquid(above, view, 0.0);
        for field in [under.zenith, under.horizon, under.ground, under.sun_colour] {
            assert_eq!(&field[..3], &view.color[..], "lit by full daylight here");
        }
        assert!((under.horizon[3] - UNDERWATER_FOG / 12.0).abs() < 1e-6);
        assert!(
            under.horizon[3] > 100.0 * above.horizon[3],
            "far thicker than air"
        );
        assert_eq!(
            (under.sun_direction[3], under.clouds[0], under.motion[3]),
            (0.0, 0.0, 0.0)
        );
        // Higher up the fog would thin; it is thickened back to match.
        let lake = under_liquid(above, view, 1200.0);
        assert!((lake.horizon[3] / under.horizon[3] - std::f32::consts::E).abs() < 1e-3);
        // At night the water is dark.
        let mut night = above;
        night.ground[3] = 0.1;
        let dark = under_liquid(night, view, 0.0);
        assert!(dark.horizon[0] < 0.02);
    }
}

#[cfg(test)]
mod version_tests {
    use super::*;

    fn temp_project(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("frame-stamp-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn build_choices_survive_saving_project_settings() {
        let root = temp_project("build");
        // An older project.ron with no build section gets the defaults.
        std::fs::write(root.join("project.ron"), "(version: \"0.1.0\")").unwrap();
        let build = read_manifest(&root).build;
        assert!(build.release && build.encrypt);
        let mut manifest = read_manifest(&root);
        manifest.build.release = false;
        manifest.build.output = "/tmp/builds".to_string();
        write_manifest(&root, &manifest);
        // What Project Settings does when saved: new description and
        // version, everything else kept.
        write_manifest(
            &root,
            &ProjectManifest {
                description: "New".to_string(),
                version: "0.2.0".to_string(),
                ..read_manifest(&root)
            },
        );
        let back = read_manifest(&root);
        assert_eq!(back.version, "0.2.0");
        assert!(!back.build.release);
        assert_eq!(back.build.output, "/tmp/builds");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_project_is_stamped_with_this_editor() {
        let root = temp_project("this");
        write_manifest(
            &root,
            &ProjectManifest {
                description: "A game".to_string(),
                version: "2.3.0".to_string(),
                ..Default::default()
            },
        );
        let back = read_manifest(&root);
        assert_eq!(back.editor, frame_engine::version::ENGINE);
        // The game's own version is left alone.
        assert_eq!(back.version, "2.3.0");
        // A project from before the stamp reads with it empty.
        std::fs::write(
            root.join("project.ron"),
            "(description: \"old\", version: \"0.1.0\")",
        )
        .unwrap();
        assert_eq!(read_manifest(&root).editor, "");
        // Saving a scene in the project stamps it.
        stamp_project(&root);
        assert_eq!(read_manifest(&root).editor, frame_engine::version::ENGINE);
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_newer_editors_stamp_is_kept() {
        let root = temp_project("newer");
        std::fs::write(
            root.join("project.ron"),
            "(description: \"\", version: \"0.1.0\", editor: \"99.0.0\")",
        )
        .unwrap();
        stamp_project(&root);
        write_manifest(&root, &read_manifest(&root));
        let back = read_manifest(&root);
        assert_eq!(back.editor, "99.0.0");
        assert!(frame_engine::version::is_newer_than_this(&back.editor));
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_folder_without_a_project_is_not_stamped() {
        let root = temp_project("bare");
        stamp_project(&root);
        assert!(!root.join("project.ron").exists());
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_plugin_needing_a_newer_engine_warns() {
        let mut manifest: frame_engine::world::PluginManifest =
            ron::from_str("(name: \"weather\", version: \"1.0.0\", author: \"me\")").unwrap();
        assert_eq!(manifest.engine, "");
        assert!(plugin_engine_warning(&manifest).is_none());
        manifest.engine = frame_engine::version::ENGINE.to_string();
        assert!(plugin_engine_warning(&manifest).is_none());
        manifest.engine = "99.0".to_string();
        let warning = plugin_engine_warning(&manifest).unwrap();
        assert!(
            warning.contains("weather") && warning.contains("99.0"),
            "{warning}"
        );
    }
}

#[cfg(test)]
mod pack_play_tests {
    use super::*;

    /// A project with a plugin, a model in a subfolder and a texture, as a
    /// folder and as a pack: Play must find the same things in both.
    #[test]
    fn play_loads_the_same_project_from_a_folder_or_a_pack() {
        let root = std::env::temp_dir().join(format!("frame-editor-pack-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let png = include_bytes!("../assets/frame-editor.png").to_vec();
        for (name, bytes) in [
            (
                "Game.ron",
                ron::to_string(&World::new()).unwrap().into_bytes(),
            ),
            ("project.ron", b"()".to_vec()),
            (
                "assets/models/rock.obj",
                b"v 0 0 0\nv 1 0 0\nv 0 1 0\nf 1 2 3\n".to_vec(),
            ),
            ("assets/textures/stone.png", png),
            (
                "plugins/weather/plugin.ron",
                b"(name: \"weather\", version: \"1.0\", author: \"me\")".to_vec(),
            ),
            ("plugins/weather/scripts/rain.rhai", b"let x = 1;".to_vec()),
            (
                "plugins/weather/scripts/notes.txt",
                b"not a script".to_vec(),
            ),
        ] {
            let path = root.join(name);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, bytes).unwrap();
        }
        let out = root.with_extension("pack");
        frame_engine::pack::write_pack(&root, &out, None).unwrap();
        let folder = ProjectFiles::open(&root, None).unwrap();
        let pack = ProjectFiles::open(&out, None).unwrap();

        for files in [&folder, &pack] {
            let scene = files.scene().unwrap();
            assert_eq!(scene, "Game.ron");
            World::load_from_str(&files.read_to_string(&scene).unwrap()).unwrap();

            let (manifests, scripts, errors) = load_plugins_from(files);
            assert!(errors.is_empty(), "{errors:?}");
            assert_eq!(manifests.keys().collect::<Vec<_>>(), ["weather"]);
            assert_eq!(scripts.keys().collect::<Vec<_>>(), ["weather/rain"]);

            let (models, errors) = load_models_from(files);
            assert!(errors.is_empty(), "{errors:?}");
            assert_eq!(models.keys().collect::<Vec<_>>(), ["rock"]);

            let mut meta = std::collections::BTreeMap::new();
            let material = frame_engine::world::MeshMaterial {
                texture: Some("stone.png".to_string()),
                ..Default::default()
            };
            meta.insert(
                "rock".to_string(),
                frame_engine::world::MeshMeta {
                    half_extents: [0.5; 3],
                    material,
                },
            );
            let mut cache = std::collections::HashMap::new();
            let gpu =
                build_material_gpu_data(&["rock".to_string()], &meta, Some(files), &mut cache);
            let (w, h, rgba) = gpu[0].texture_rgba.clone().expect("texture found");
            assert_eq!(rgba.len(), (w * h * 4) as usize);
        }
        std::fs::remove_dir_all(&root).ok();
        std::fs::remove_file(&out).ok();
    }
}
