// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! How a crate outside the editor adds a feature to it.
//!
//! The editor knows nothing about any particular feature. A crate that wants
//! to add one (a game-specific feature, say) implements [`EditorModule`] and hands
//! it to [`crate::run`]. The editor then asks each module, at the right
//! moments, to:
//!
//! - show an Inspector section for the selected entity ([`EditorModule::inspect`]),
//! - build whatever it will draw from the world ([`EditorModule::build_scene`]),
//! - make a per-window GPU copy of it ([`EditorModule::new_gpu`], [`ModuleGpu`]),
//!   which can bring its own textures ([`GpuContext`], [`WorldMaterial`]),
//! - leave anchor entities out of the ordinary shape pass
//!   ([`EditorModule::anchor_components`]),
//! - say where the viewers are, so it can build detail around them
//!   ([`EditorModule::set_viewers`]),
//! - and place the sun, if it wants to ([`EditorModule::sun`]).
//!
//! A module's own data lives in the world as named extension components
//! (`World::ext_set` / `ext_get`), so it saves and loads with the scene.

use super::*;
use std::collections::{BTreeMap, BTreeSet};

/// The scene part of the editor's shader (WGSL): bind group 0 (the camera,
/// lights, shadow map and sky) and the helpers that read it (`apply_fog`,
/// `shadow_factor`, `morph_amount`, ...). A module drawing with its own shader
/// (see [`ModuleGpu::draw_custom_in_view`]) puts this first in its source, so
/// it is lit, shadowed and fogged exactly like everything else. The camera
/// can be read only in vertex shaders, the lights and shadow map only in
/// fragment shaders, and the sky in both.
pub const SCENE_WGSL: &str = include_str!("scene.wgsl");

pub use crate::grass::{
    GRASS_GRID_MAX, GRASS_PATCHES_MAX, GrassFactory, GrassField, GrassPatch, GrassQuality,
    GrassSample,
};
/// Bytes as megabytes, worded the way the performance overlay shows them.
pub use crate::perf::megabytes;
pub use frame_engine::assets::MeshVertexData;
pub use frame_engine::physics::{StaticMesh, StaticMeshSet};

/// Whatever a module builds from the world, shared between every window that
/// draws it. The module downcasts it back to its own type in `ModuleGpu::sync`.
pub type ModuleScene = Arc<dyn std::any::Any + Send + Sync>;

/// A feature added to the editor by another crate.
pub trait EditorModule {
    /// Short name, for logs.
    fn name(&self) -> &str;

    /// Names of extension components whose entities are only anchors. An
    /// entity with any of these is not drawn as a cube; the module draws its
    /// own geometry instead.
    fn anchor_components(&self) -> &'static [&'static str] {
        &[]
    }

    /// Add this module's section to the Inspector for the selected entity.
    /// Read and write the entity's components through `entity`.
    fn inspect(&mut self, _ui: &mut egui::Ui, _entity: &mut ExtEdit) {}

    /// Where the viewers are, in world space, just before `build_scene`. The
    /// first is the camera about to be drawn from (the editor camera in the
    /// viewport, the active Camera entity in the Play window); while Play
    /// runs the other window's camera follows, so a module can build for both
    /// and neither window waits on the other. A module that builds more
    /// detail near the viewer than far away (a streamed landscape) keeps
    /// these; a module that doesn't care ignores them. The list is never
    /// empty: with no Camera entity the viewer is the editor's focus point.
    fn set_viewers(&mut self, _viewers: &[[f64; 3]]) {}

    /// Build what should be drawn for `world`, or None for nothing. Called
    /// every frame for each world being drawn, so reuse the last result when
    /// nothing changed. `hold` is true while a mouse button is down (dragging
    /// an Inspector slider, say): keep showing the previous result instead of
    /// rebuilding on every frame.
    fn build_scene(&mut self, _world: &World, _hold: bool) -> Option<ModuleScene> {
        None
    }

    /// Fixed collision geometry this module wants the simulation to have, as
    /// named sets (see `StaticMeshSet`). Called every physics tick, so return
    /// the same `key` while nothing changed and the physics side keeps what it
    /// already built. Return an empty list for none.
    fn collision_meshes(&mut self, _world: &World) -> Vec<(String, StaticMeshSet)> {
        Vec::new()
    }

    /// Whether this module has a change to make to the world from the editor
    /// (a button pressed in its Inspector section that spawns entities,
    /// say). Asked once per editor frame, after the Inspector; when it says
    /// yes the editor saves an undo step and then calls `edit_world`. The
    /// default is never.
    fn wants_world_edit(&self) -> bool {
        false
    }

    /// Make the change `wants_world_edit` asked for, with write access to the
    /// world. Called in the editor only, paused or running, never in Play.
    fn edit_world(&mut self, _world: &mut World) {}

    /// Advance whatever this module simulates by `dt` seconds, with write
    /// access to the world (including `world.resources`). Called once per
    /// simulation tick, before collision is synced, in the editor viewport
    /// and in Play alike. Do nothing when there is nothing to advance.
    fn tick(&mut self, _world: &mut World, _dt: f32) {}

    /// Where this module wants the sun, if it does. Called once per frame
    /// for each window drawn, with the world being drawn, before the lights
    /// are built. Return `Some` to take over the sun from the scene's Light
    /// (and from the editor's own time-of-day setting): lights, shadows, sky
    /// and fog all follow it. The scene itself is never changed. Return
    /// `None` to leave the sun alone. If several modules answer, the first
    /// wins.
    fn sun(&mut self, _world: &World) -> Option<SunOverride> {
        None
    }

    /// How far from the viewer this module's scene is sure to reach, in world
    /// units, if it draws out to a distance (a landscape, say). Called once
    /// per frame for each window drawn, like `sun`. The editor thins its fog
    /// so that it is nearly full at the farthest reach any module gives,
    /// and the scene's far edge fades into the sky instead of ending in a
    /// line. The default is None: the fog keeps its usual thickness.
    fn view_distance(&self, _world: &World) -> Option<f64> {
        None
    }

    /// How far the sea stands above its still level right now (below if
    /// negative), in world units, if this module keeps the tides. Called once
    /// per frame for each window drawn, like `sun`. Water built with a tide
    /// rule (see [`WaterFlow`]) follows it on the GPU: the sea rises and
    /// falls, and water the tide can reach (a river's mouth) is lifted where
    /// the tide stands higher. If several modules answer, the first wins.
    /// The default is None: no tide.
    fn tide(&mut self, _world: &World) -> Option<f32> {
        None
    }

    /// Where this module wants the moon and how full it is, if it keeps one.
    /// Called once per frame for each window drawn, after `sun`. The sky
    /// draws the moon with its lit part, and at night it lights the scene in
    /// place of the sun: a dim, cool light that still casts shadows, brighter
    /// the fuller and higher it is. The default is None: with a day-night sun
    /// the editor puts a full moon opposite the sun. If several modules
    /// answer, the first wins.
    fn moon(&mut self, _world: &World) -> Option<MoonOverride> {
        None
    }

    /// The wind, if this module keeps the weather. Called once per frame for
    /// each window drawn, after `sun`. The clouds drift with it. The default
    /// is None: a light breeze that moves while the world moves.
    fn wind(&mut self, _world: &World) -> Option<Wind> {
        None
    }

    /// Whether this module is still working in the background (building
    /// something it will want to show). While any module is, the editor keeps
    /// drawing every frame so the results appear as they arrive; otherwise an
    /// idle editor only draws when something changes. The default is never.
    fn busy(&self) -> bool {
        false
    }

    /// Lines for the performance overlay (View, Performance overlay): how
    /// much this module holds and is doing, in plain words ("cache 120 MB",
    /// say). Called once per frame only while the overlay is shown. The
    /// default shows nothing.
    fn stats(&self) -> Vec<String> {
        Vec::new()
    }

    /// Make this module's GPU state for one window.
    fn new_gpu(&self, gpu: &GpuContext) -> Box<dyn ModuleGpu>;
}

/// A sun supplied by a module (see [`EditorModule::sun`]).
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct SunOverride {
    /// Unit vector pointing toward the sun: X east, Y up, Z south. A negative
    /// Y puts the sun below the horizon, which is night.
    pub direction: [f32; 3],
}

/// A moon supplied by a module (see [`EditorModule::moon`]).
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct MoonOverride {
    /// Unit vector pointing toward the moon: X east, Y up, Z south. Below the
    /// horizon when Y is negative.
    pub direction: [f32; 3],
    /// Where it is in its month: 0 new (dark), 0.25 first quarter (lit on
    /// the right), 0.5 full, 0.75 last quarter, back to new at 1.
    pub phase: f32,
}

/// The wind a module supplies (see [`EditorModule::wind`]).
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Wind {
    /// Where the air is going and how fast, in metres a second: x east, y
    /// south (the world's x and z).
    pub velocity: [f32; 2],
    /// How far the air has moved so far, in metres (x and z). The clouds sit
    /// this far downwind of where they started, so a module with its own
    /// clock (a sped-up day) moves them at its own pace.
    pub drift: [f64; 2],
}

/// What a module gets when it makes its per-window GPU state.
pub struct GpuContext<'a> {
    pub device: &'a wgpu::Device,
    pub queue: &'a wgpu::Queue,
    pub(crate) material_layout: &'a wgpu::BindGroupLayout,
    pub(crate) material_sampler: &'a wgpu::Sampler,
    pub(crate) grass: &'a crate::grass::GrassFactory,
    pub(crate) scene_layout: &'a wgpu::BindGroupLayout,
    pub(crate) color_format: wgpu::TextureFormat,
}

impl GpuContext<'_> {
    /// A maker of [`WorldMaterial`]s that the module can keep, to build or
    /// rebuild its textures whenever it syncs.
    pub fn material_factory(&self) -> MaterialFactory {
        MaterialFactory {
            layout: self.material_layout.clone(),
            sampler: self.material_sampler.clone(),
        }
    }

    /// What a module needs to make [`GrassField`]s for this window.
    pub fn grass_factory(&self) -> GrassFactory {
        self.grass.clone()
    }

    /// What a module needs to build its own render pipeline for
    /// [`ModuleGpu::draw_custom_in_view`].
    pub fn custom_targets(&self) -> CustomTargets {
        CustomTargets {
            scene_layout: self.scene_layout.clone(),
            color_format: self.color_format,
            depth_format: crate::DEPTH_FORMAT,
            depth_compare: wgpu::CompareFunction::Greater,
        }
    }
}

/// How a module's own pipeline must be set up to draw into a window (see
/// [`ModuleGpu::draw_custom_in_view`]): bind group 0 is always the scene
/// (`scene_layout`, the bindings in [`SCENE_WGSL`]); the colour target's
/// format; and the depth buffer's format and test. Depth is reversed (nearer
/// is greater), so use `depth_compare` as given.
#[derive(Clone, Debug)]
pub struct CustomTargets {
    pub scene_layout: wgpu::BindGroupLayout,
    pub color_format: wgpu::TextureFormat,
    pub depth_format: wgpu::TextureFormat,
    pub depth_compare: wgpu::CompareFunction,
}

/// The Grass setting in Editor Settings, for a module that draws grass of
/// its own (see [`GrassQuality`]).
pub fn grass_setting() -> GrassQuality {
    crate::grass_quality()
}

/// Seconds of moving water and wind: runs while the world moves (the editor
/// playing or the Play window open), wrapped every hour. The same clock the
/// editor's waves and grass use, for a module's own shader.
pub fn motion_seconds() -> f32 {
    crate::wave_seconds()
}

/// Makes [`WorldMaterial`]s for one window.
#[derive(Clone)]
pub struct MaterialFactory {
    layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
}

/// Whether `indices` are whole triangles that all point at one of `vertices`
/// vertices.
fn indices_fit(indices: &[u32], vertices: usize) -> bool {
    !indices.is_empty()
        && indices.len() % 3 == 0
        && indices.iter().all(|&i| (i as usize) < vertices)
}

/// Whether `len` bytes are exactly a `width` by `height` RGBA image.
fn pixels_fit(width: u32, height: u32, len: usize) -> bool {
    width > 0 && height > 0 && len == width as usize * height as usize * 4
}

impl MaterialFactory {
    /// A material from an RGBA8 image (`width * height * 4` bytes, colours
    /// in sRGB). The sampler filters linearly and repeats, so keep texture
    /// coordinates a half texel inside the edges if blending across them
    /// would be wrong. `roughness` and `metalness` are 0 to 1, as for a
    /// mesh. Returns None if the pixel data is the wrong size.
    pub fn create(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        width: u32,
        height: u32,
        rgba: &[u8],
        roughness: f32,
        metalness: f32,
    ) -> Option<WorldMaterial> {
        self.create_with_look(
            device,
            queue,
            width,
            height,
            rgba,
            MaterialLook {
                roughness,
                metalness,
                ..MaterialLook::default()
            },
        )
    }

    /// Like `create`, with the whole [`MaterialLook`]. With more than one
    /// layer, `height` must divide into that many layers of equal height.
    pub fn create_with_look(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        width: u32,
        height: u32,
        rgba: &[u8],
        look: MaterialLook,
    ) -> Option<WorldMaterial> {
        let layers = look.layers.max(1);
        if !pixels_fit(width, height, rgba.len()) || !height.is_multiple_of(layers) {
            return None;
        }
        let view = create_material_texture(device, queue, width, height, rgba);
        let bind_group = create_material_bind_group_with(
            device,
            &self.layout,
            &self.sampler,
            &view,
            [
                look.roughness.clamp(0.0, 1.0),
                look.metalness.clamp(0.0, 1.0),
                1.0,
                0.0,
            ],
            [look.detail.clamp(0.0, 1.0), layers as f32, 0.0, 0.0],
        );
        Some(WorldMaterial { bind_group })
    }

    /// A material for a see-through liquid surface drawn in the transparent
    /// pass (water). It is smooth and untextured, and the shader gives it
    /// moving waves, `ripples` (0 to 1) choppy, that fade out with distance;
    /// the sky reflected more strongly the lower the view; and a glint of the
    /// sun, small up close and a broad glittering path far out. The waves
    /// move only while the world does. The vertex `uv` keeps its meaning for
    /// the transparent pass (opacity and thickness).
    pub fn create_liquid(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        ripples: f32,
    ) -> WorldMaterial {
        let view = create_default_white_texture(device, queue);
        let bind_group = create_material_bind_group_with(
            device,
            &self.layout,
            &self.sampler,
            &view,
            [0.05, 0.0, 0.0, ripples.clamp(0.0, 1.0).max(1e-3)],
            [0.0; 4],
        );
        WorldMaterial { bind_group }
    }
}

/// How a material's surface looks, beyond its texture.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct MaterialLook {
    /// 0 smooth to 1 rough, as for a mesh.
    pub roughness: f32,
    /// 0 to 1, as for a mesh.
    pub metalness: f32,
    /// 0 to 1: soft light and dark patches laid over the texture at a few
    /// sizes (from about 4 to 256 world units), fixed to the world, so large
    /// areas of one colour (ground seen up close) are not flat. Each size
    /// fades out with distance before it could shimmer. 0 is none.
    pub detail: f32,
    /// How many images the texture holds, stacked top to bottom, each the
    /// same size (1 for an ordinary texture). A mesh built with
    /// [`WorldMesh::new_indexed_layered`] picks a point between them for
    /// each vertex and the colour blends between the two nearest, so one
    /// mesh can shade from one set of colours into another (lush ground into
    /// dry, say). The vertex `uv` addresses one image, as if it were the
    /// whole texture.
    pub layers: u32,
}

impl Default for MaterialLook {
    /// Fully rough, not metal, no detail, one layer.
    fn default() -> Self {
        MaterialLook {
            roughness: 1.0,
            metalness: 0.0,
            detail: 0.0,
            layers: 1,
        }
    }
}

/// A texture and surface look a module's meshes can be drawn with. The
/// mesh's `uv` picks the pixel, and the result multiplies the instance
/// colour. The editor puts the default material back before every module's
/// draw, so bind this at the start of your own.
pub struct WorldMaterial {
    bind_group: wgpu::BindGroup,
}

impl WorldMaterial {
    pub fn bind(&self, pass: &mut wgpu::RenderPass<'_>) {
        pass.set_bind_group(1, &self.bind_group, &[]);
    }
}

/// One window's GPU side of a module.
pub trait ModuleGpu {
    /// Make the GPU copy match `scene`: upload when it changed, drop it when
    /// there is none. Called every frame, so do nothing when already current.
    fn sync(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, scene: Option<&ModuleScene>);

    /// Where the editor is drawing from this frame. The world's positions are
    /// `f64`, but the graphics card works in `f32`, so everything is drawn
    /// relative to a point near the camera (`origin`, in world units; it
    /// changes only in large steps). Called every frame after `sync`, before
    /// anything is drawn. A module that places geometry in the world keeps
    /// its own `f64` position and draws at `position - origin`, cast to `f32`
    /// only after the subtraction. A module whose geometry is placed some
    /// other way can ignore this.
    fn set_render_origin(&mut self, _queue: &wgpu::Queue, _origin: [f64; 3]) {}

    /// Box (min, max) of what this draws, so shadows can cover it. In the same
    /// space as everything is drawn in: relative to the origin given to
    /// `set_render_origin`.
    fn shadow_bounds(&self) -> Option<([f32; 3], [f32; 3])> {
        None
    }

    /// Whether there is anything to draw.
    fn has_geometry(&self) -> bool;

    /// Draw it. The editor has already set the pipeline and bind groups, for
    /// both the shadow pass and the colour pass.
    fn draw(&self, pass: &mut wgpu::RenderPass<'_>);

    /// Like `draw`, for one view: `view` tells what this draw can see, so a
    /// module with many meshes can skip those that would not show (see
    /// [`DrawView::sees_box`]). The editor calls this, not `draw`, for the
    /// colour pass, the shadow pass and the Camera preview. The default
    /// draws everything with `draw`.
    fn draw_in_view(&self, pass: &mut wgpu::RenderPass<'_>, _view: &DrawView) {
        self.draw(pass);
    }

    /// Like `draw_transparent`, for one view (see `draw_in_view`). The
    /// default draws everything with `draw_transparent`.
    fn draw_transparent_in_view(&self, pass: &mut wgpu::RenderPass<'_>, _view: &DrawView) {
        self.draw_transparent(pass);
    }

    /// Whether there is anything to draw with `draw_transparent`.
    fn has_transparent(&self) -> bool {
        false
    }

    /// Draw see-through geometry. Called after everything opaque (the
    /// scene's entities and every module's `draw`) has been drawn, in the
    /// colour pass only, so it casts no shadow. The pipeline blends by the
    /// vertex's `uv.x` (0 clear, 1 solid), tests depth and does not write it.
    /// `uv.y` is a 0 to 1 thickness: thin geometry lightens and fades, so set
    /// it to 1 for an ordinary uniform mesh.
    /// Meshes are not sorted: draw the farthest first if several overlap.
    fn draw_transparent(&self, _pass: &mut wgpu::RenderPass<'_>) {}

    /// Whether there is anything to draw with `draw_custom_in_view`.
    fn has_custom(&self) -> bool {
        false
    }

    /// Whether this module draws the clouds itself (with
    /// `draw_custom_in_view`). If any module in a window does, the sky
    /// leaves out its own clouds there and `clouds.w` in the sky uniform is
    /// 1. The cover and the wind's drift still ride in `clouds.xyz` for the
    /// module's shader. Liquids still reflect the sky's own clouds. The
    /// default is false.
    fn draws_clouds(&self) -> bool {
        false
    }

    /// Draw with the module's own pipeline (built with
    /// [`GpuContext::custom_targets`] and a shader that starts with
    /// [`SCENE_WGSL`]). Called after every module's solid geometry and grass
    /// and before anything see-through, in the colour pass and the Camera
    /// preview, with the scene bound as group 0. Set your own pipeline and
    /// any other groups; whoever draws next sets theirs.
    fn draw_custom_in_view(&self, _pass: &mut wgpu::RenderPass<'_>, _view: &DrawView) {}

    /// This window's grass, if the module has any (see [`GrassField`]). The
    /// editor draws it after every module's solid geometry, as much of it as
    /// the Grass setting asks for. The default is none.
    fn grass(&self) -> Option<&GrassField> {
        None
    }

    /// Lines for the performance overlay about this window's GPU copy (how
    /// many meshes, how much memory). Called once per frame only while the
    /// overlay is shown. The default shows nothing.
    fn stats(&self) -> Vec<String> {
        Vec::new()
    }
}

/// What one draw can see: the six sides of its view, in the same space as
/// everything is drawn in (relative to the render origin). Handed to
/// [`ModuleGpu::draw_in_view`] so a module can skip meshes outside it. For
/// the colour pass and the Camera preview it is the camera's view; for the
/// shadow pass it is the box the sun's shadow map covers, which is only the
/// ground near the camera.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct DrawView {
    /// Each side as `[a, b, c, d]`: a point `p` is on the inside when
    /// `a * p.x + b * p.y + c * p.z + d >= 0`. None sees everything.
    planes: Option<[[f32; 4]; 6]>,
    /// True for the shadow pass, which draws depth only, seen from the sun.
    pub shadow: bool,
    /// Where the camera is (relative to the render origin), for a camera's
    /// view; None for the shadow pass and a view that sees everything.
    eye: Option<[f32; 3]>,
}

impl DrawView {
    /// The view of a view-projection matrix (a camera's, or the shadow
    /// map's). A broken matrix gives a view that sees everything.
    pub fn from_view_proj(view_proj: [[f32; 4]; 4], shadow: bool) -> DrawView {
        let m = Mat4::from_cols_array_2d(&view_proj);
        let (r0, r1, r2, r3) = (m.row(0), m.row(1), m.row(2), m.row(3));
        // Clip space keeps -w <= x, y <= w and 0 <= z <= w; each side is one
        // of those, written in terms of the point before projection.
        let sides = [r3 + r0, r3 - r0, r3 + r1, r3 - r1, r2, r3 - r2];
        let planes = sides
            .iter()
            .all(|p| p.is_finite())
            .then(|| sides.map(|p| p.to_array()));
        let eye = (!shadow)
            .then(|| crate::sky::eye_from_view_proj(&m))
            .filter(|e| e.is_finite())
            .map(|e| e.to_array());
        DrawView {
            planes,
            shadow,
            eye,
        }
    }

    /// Where the camera is, relative to the render origin, for a camera's
    /// view (None for the shadow pass): for drawing more detail near it.
    pub fn eye(&self) -> Option<[f32; 3]> {
        self.eye
    }

    /// A view that sees everything.
    pub fn everything() -> DrawView {
        DrawView {
            planes: None,
            shadow: false,
            eye: None,
        }
    }

    /// Whether any part of the box from `min` to `max` may be seen. Never
    /// says no to something that shows; it can say yes to a box that only
    /// just misses a corner of the view.
    pub fn sees_box(&self, min: [f32; 3], max: [f32; 3]) -> bool {
        let Some(planes) = &self.planes else {
            return true;
        };
        planes.iter().all(|[a, b, c, d]| {
            // The box's corner furthest along this side's inward direction.
            let x = if *a >= 0.0 { max[0] } else { min[0] };
            let y = if *b >= 0.0 { max[1] } else { min[1] };
            let z = if *c >= 0.0 { max[2] } else { min[2] };
            a * x + b * y + c * z + d >= 0.0
        })
    }
}

/// An editable copy of one entity's extension components, handed to
/// [`EditorModule::inspect`]. Changes are written back to the world by the
/// editor, and only for names that were actually changed.
#[derive(Clone, PartialEq, Default)]
pub struct ExtEdit {
    id: usize,
    values: BTreeMap<String, ron::Value>,
    touched: BTreeSet<String>,
}

impl ExtEdit {
    pub(crate) fn from_world(world: &World, id: usize) -> Self {
        let mut values = BTreeMap::new();
        for (name, storage) in &world.extensions {
            if let Some(value) = storage.get(id) {
                values.insert(name.clone(), value.clone());
            }
        }
        ExtEdit {
            id,
            values,
            touched: BTreeSet::new(),
        }
    }

    pub(crate) fn apply(&self, world: &mut World, id: usize) {
        for name in &self.touched {
            match self.values.get(name) {
                Some(value) => {
                    world
                        .extensions
                        .entry(name.clone())
                        .or_default()
                        .insert(id, value.clone());
                }
                None => world.ext_remove(name, id),
            }
        }
    }

    /// The entity's id, for a module that wants to act on it from
    /// [`EditorModule::edit_world`].
    pub fn id(&self) -> usize {
        self.id
    }

    /// Whether the entity has a component named `name`.
    pub fn has(&self, name: &str) -> bool {
        self.values.contains_key(name)
    }

    /// The component named `name`, if the entity has it and it reads as `T`.
    pub fn get<T: serde::de::DeserializeOwned>(&self, name: &str) -> Option<T> {
        self.values.get(name)?.clone().into_rust::<T>().ok()
    }

    /// Set the component named `name`. Setting the value it already has is
    /// not a change.
    pub fn set<T: serde::Serialize>(&mut self, name: &str, value: &T) {
        let Ok(text) = ron::to_string(value) else {
            return;
        };
        let Ok(new) = ron::from_str::<ron::Value>(&text) else {
            return;
        };
        if self.values.get(name) != Some(&new) {
            self.values.insert(name.to_string(), new);
            self.touched.insert(name.to_string());
        }
    }

    /// Remove the component named `name`.
    pub fn remove(&mut self, name: &str) {
        if self.values.remove(name).is_some() {
            self.touched.insert(name.to_string());
        }
    }
}

/// How one vertex of a level-of-detail mesh slides toward a coarser shape as
/// the camera gets further away, so that switching to the coarser mesh shows
/// no jump. Distance is measured across the ground only, as the larger of the
/// x and z distances from the camera (a square, matching square rings of
/// detail). Nearer than `start` the vertex is where its mesh puts it; past
/// `end` it is fully moved; in between it blends. `end` at or below `start`
/// means no morph.
#[derive(Copy, Clone, Debug, Default, PartialEq)]
pub struct VertexMorph {
    /// How far the vertex moves up (down if negative) when fully morphed, in
    /// the mesh's own units.
    pub offset_y: f32,
    /// The normal it has when fully morphed.
    pub normal: [f32; 3],
    pub start: f32,
    pub end: f32,
    /// The texture coordinates it has when fully morphed, so a colour read
    /// from the mesh's texture slides too. None keeps its own.
    pub uv: Option<[f32; 2]>,
}

/// How a vertex of a liquid surface moves: its flow, which carries the
/// ripples along and breaks into white water when fast, and how it follows
/// the tide (see [`EditorModule::tide`]). For [`WorldMesh::new_flowing`].
#[derive(Copy, Clone, Debug, Default, PartialEq)]
pub struct WaterFlow {
    /// The water's velocity across the ground (x, z), in the mesh's own units
    /// a second, up to `FLOW_RANGE` either way.
    pub velocity: [f32; 2],
    pub tide: Tide,
}

/// How a vertex of a liquid surface follows the tide.
#[derive(Copy, Clone, Debug, Default, PartialEq)]
pub enum Tide {
    /// Not at all: a lake, or water that is not a liquid.
    #[default]
    Still,
    /// The sea: rises and falls with the tide.
    Sea,
    /// Water the tide can reach, such as a river near its mouth: lifted to
    /// the tide's level where that is higher than its own. `sea` is the
    /// still sea level in the mesh's own units.
    Floods { sea: f32 },
}

/// The fastest water a [`WaterFlow`] can carry, in the mesh's units a
/// second. MUST match FLOW_RANGE in shader.wgsl.
pub const FLOW_RANGE: f32 = 16.0;
/// How far below the still sea a flooding vertex can sit and still be
/// reached, in the mesh's units. MUST match TIDE_REACH in shader.wgsl.
const TIDE_REACH: f32 = 512.0;

/// A value from -1 to 1 as the renderer stores it.
fn snorm(v: f32) -> i16 {
    (v.clamp(-1.0, 1.0) * 32767.0).round() as i16
}

/// `flow` as the renderer stores it, for a vertex at height `y`.
fn water_bits(flow: &WaterFlow, y: f32) -> [i16; 4] {
    let (mode, below) = match flow.tide {
        Tide::Still => (0.0, 0.0),
        Tide::Sea => (1.0, 0.0),
        Tide::Floods { sea } => (-1.0, (sea - y) / TIDE_REACH),
    };
    [
        snorm(flow.velocity[0] / FLOW_RANGE),
        snorm(flow.velocity[1] / FLOW_RANGE),
        snorm(mode),
        snorm(below),
    ]
}

/// The vertices as the renderer stores them, each with its `morphs` entry
/// (none past the end of `morphs`).
fn morphing_vertices(vertices: &[MeshVertexData], morphs: &[VertexMorph]) -> Vec<MeshVertex> {
    vertices
        .iter()
        .enumerate()
        .map(|(i, v)| {
            let mut out = MeshVertex::plain(v.position, v.normal, v.uv);
            if let Some(m) = morphs.get(i) {
                out.morph = [m.offset_y, m.start, m.end];
                out.morph_normal = m.normal;
                out.morph_uv = m.uv.unwrap_or(v.uv);
            }
            out
        })
        .collect()
}

/// A layer choice (0 to 1, see `MaterialLook::layers`) as the renderer stores
/// it: in the last of the four values liquids use for their flow, which a
/// solid surface leaves unused (its tide rule stays 0, so nothing moves).
fn layer_bits(layer: f32) -> [i16; 4] {
    [0, 0, 0, snorm(layer.clamp(0.0, 1.0))]
}

/// The vertices as the renderer stores them, with their `morphs` and
/// `layers` entries (none past the end of either).
fn layered_vertices(
    vertices: &[MeshVertexData],
    morphs: &[VertexMorph],
    layers: &[f32],
) -> Vec<MeshVertex> {
    let mut verts = morphing_vertices(vertices, morphs);
    for (v, &layer) in verts.iter_mut().zip(layers) {
        v.water = layer_bits(layer);
    }
    verts
}

/// The vertices as the renderer stores them, with their `morphs` and `flows`
/// entries (none past the end of either).
fn flowing_vertices(
    vertices: &[MeshVertexData],
    morphs: &[VertexMorph],
    flows: &[WaterFlow],
) -> Vec<MeshVertex> {
    let mut verts = morphing_vertices(vertices, morphs);
    for (v, flow) in verts.iter_mut().zip(flows) {
        v.water = water_bits(flow, v.position[1]);
    }
    verts
}

/// A triangle mesh on the GPU, in world units, for a module to draw.
///
/// Every window shares one graphics device, so a `WorldMesh` made while one
/// window syncs can be kept (in an `Arc`) and drawn by any other window too,
/// instead of each window uploading its own copy.
pub struct WorldMesh {
    buffer: wgpu::Buffer,
    /// Vertices in the buffer.
    count: u32,
    /// For an indexed mesh: the triangle corners, as positions in `buffer`,
    /// and how many there are.
    indices: Option<(wgpu::Buffer, u32)>,
}

impl WorldMesh {
    /// Upload `vertices` (a flat triangle list). None when there are none.
    pub fn new(device: &wgpu::Device, vertices: &[MeshVertexData]) -> Option<Self> {
        if vertices.is_empty() {
            return None;
        }
        let verts: Vec<MeshVertex> = vertices
            .iter()
            .map(|v| MeshVertex::plain(v.position, v.normal, v.uv))
            .collect();
        Some(Self::upload(device, &verts))
    }

    /// Like `new`, with each vertex morphing as its `morphs` entry says (see
    /// [`VertexMorph`]). `morphs` holds one entry per vertex; a missing entry
    /// means that vertex does not morph.
    pub fn new_morphing(
        device: &wgpu::Device,
        vertices: &[MeshVertexData],
        morphs: &[VertexMorph],
    ) -> Option<Self> {
        if vertices.is_empty() {
            return None;
        }
        let verts = morphing_vertices(vertices, morphs);
        Some(Self::upload(device, &verts))
    }

    /// Like `new_morphing`, for a liquid surface whose vertices move as their
    /// `flows` entries say (see [`WaterFlow`]); a missing entry is still
    /// water. Drawn with a liquid material, the ripples run with the flow.
    pub fn new_flowing(
        device: &wgpu::Device,
        vertices: &[MeshVertexData],
        morphs: &[VertexMorph],
        flows: &[WaterFlow],
    ) -> Option<Self> {
        if vertices.is_empty() {
            return None;
        }
        let verts = flowing_vertices(vertices, morphs, flows);
        Some(Self::upload(device, &verts))
    }

    /// An indexed mesh: each vertex stored once, and `indices` listing the
    /// three corners of every triangle as positions in `vertices`. A smooth
    /// surface shares each vertex between about six triangles, so this takes
    /// a fraction of the memory of a flat triangle list. Each vertex morphs as
    /// its `morphs` entry says (none when `morphs` is shorter). None when there
    /// are no triangles or an index points past the vertices.
    pub fn new_indexed(
        device: &wgpu::Device,
        vertices: &[MeshVertexData],
        morphs: &[VertexMorph],
        indices: &[u32],
    ) -> Option<Self> {
        if !indices_fit(indices, vertices.len()) {
            return None;
        }
        let verts = morphing_vertices(vertices, morphs);
        let mut mesh = Self::upload(device, &verts);
        let index_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("module mesh indices"),
            contents: bytemuck::cast_slice(indices),
            usage: wgpu::BufferUsages::INDEX,
        });
        mesh.indices = Some((index_buffer, indices.len() as u32));
        Some(mesh)
    }

    /// Like `new_indexed`, for a liquid surface whose vertices move as their
    /// `flows` entries say (see `new_flowing`). A surface that changes every
    /// frame (water that flows and rises) is made once with this and then
    /// rewritten in place with [`WorldMesh::update_flowing`].
    pub fn new_indexed_flowing(
        device: &wgpu::Device,
        vertices: &[MeshVertexData],
        morphs: &[VertexMorph],
        flows: &[WaterFlow],
        indices: &[u32],
    ) -> Option<Self> {
        if !indices_fit(indices, vertices.len()) {
            return None;
        }
        let verts = flowing_vertices(vertices, morphs, flows);
        let mut mesh = Self::upload(device, &verts);
        let index_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("module mesh indices"),
            contents: bytemuck::cast_slice(indices),
            usage: wgpu::BufferUsages::INDEX,
        });
        mesh.indices = Some((index_buffer, indices.len() as u32));
        Some(mesh)
    }

    /// Rewrite every vertex of a flowing mesh in place, keeping its indices:
    /// `vertices` must be exactly as many as it was made with. Far cheaper
    /// than a new mesh for a surface that changes every frame. Returns false,
    /// changing nothing, when the count differs.
    pub fn update_flowing(
        &self,
        queue: &wgpu::Queue,
        vertices: &[MeshVertexData],
        morphs: &[VertexMorph],
        flows: &[WaterFlow],
    ) -> bool {
        if vertices.len() as u32 != self.count {
            return false;
        }
        let verts = flowing_vertices(vertices, morphs, flows);
        queue.write_buffer(&self.buffer, 0, bytemuck::cast_slice(&verts));
        true
    }

    /// Like `new_indexed`, drawn with a layered material (see
    /// [`MaterialLook::layers`]): `layers` gives each vertex a point from 0
    /// (the first image) to 1 (the last), and the colour blends between the
    /// two images nearest it. A missing entry is 0. For solid surfaces only:
    /// liquids use the same slot for their flow.
    pub fn new_indexed_layered(
        device: &wgpu::Device,
        vertices: &[MeshVertexData],
        morphs: &[VertexMorph],
        layers: &[f32],
        indices: &[u32],
    ) -> Option<Self> {
        if !indices_fit(indices, vertices.len()) {
            return None;
        }
        let verts = layered_vertices(vertices, morphs, layers);
        let mut mesh = Self::upload(device, &verts);
        let index_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("module mesh indices"),
            contents: bytemuck::cast_slice(indices),
            usage: wgpu::BufferUsages::INDEX,
        });
        mesh.indices = Some((index_buffer, indices.len() as u32));
        Some(mesh)
    }

    fn upload(device: &wgpu::Device, verts: &[MeshVertex]) -> Self {
        let buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("module mesh buffer"),
            contents: bytemuck::cast_slice(verts),
            // Written to again by `update_flowing`.
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        });
        WorldMesh {
            buffer,
            count: verts.len() as u32,
            indices: None,
        }
    }

    /// How many vertices it holds.
    pub fn vertex_count(&self) -> u32 {
        self.count
    }

    /// How many triangles it draws.
    pub fn triangle_count(&self) -> u32 {
        match &self.indices {
            Some((_, n)) => n / 3,
            None => self.count / 3,
        }
    }

    /// How much GPU memory its buffers take, in bytes.
    pub fn bytes(&self) -> u64 {
        self.buffer.size() + self.indices.as_ref().map_or(0, |(b, _)| b.size())
    }

    /// Draw it. Call `WorldInstance::bind` first.
    pub fn draw(&self, pass: &mut wgpu::RenderPass<'_>) {
        self.draw_copies(pass, 1);
    }

    /// Draw a copy of it at every placement in `instances`, all in one draw
    /// call (see [`WorldInstances`]). Binds the placements itself.
    pub fn draw_instanced(&self, pass: &mut wgpu::RenderPass<'_>, instances: &WorldInstances) {
        if instances.count == 0 {
            return;
        }
        pass.set_vertex_buffer(1, instances.buffer.slice(..));
        self.draw_copies(pass, instances.count);
    }

    /// Draw `copies` copies, one per placement in the bound instance buffer.
    fn draw_copies(&self, pass: &mut wgpu::RenderPass<'_>, copies: u32) {
        pass.set_vertex_buffer(0, self.buffer.slice(..));
        match &self.indices {
            Some((indices, n)) => {
                pass.set_index_buffer(indices.slice(..), wgpu::IndexFormat::Uint32);
                pass.draw_indexed(0..*n, 0, 0..copies);
            }
            None => pass.draw(0..self.count, 0..copies),
        }
    }
}

/// Where one copy of a mesh goes, for [`WorldInstances`].
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Placement {
    /// Where the mesh's origin goes, in world units, relative to the set's
    /// anchor (see `WorldInstances::new`).
    pub position: [f32; 3],
    /// Yaw, pitch and roll in radians, applied roll, then pitch, then yaw,
    /// as for an entity's `Rotation`.
    pub rotation: [f32; 3],
    /// How much bigger than the mesh's own size, along each axis (1 is the
    /// mesh as built).
    pub scale: [f32; 3],
    /// Multiplies the mesh's colour (its material's texture), so copies of
    /// one mesh can differ a little. White leaves it as it is.
    pub color: [f32; 3],
}

impl Placement {
    /// At `position` relative to the anchor, unturned, at the mesh's own
    /// size and colour.
    pub fn at(position: [f32; 3]) -> Placement {
        Placement {
            position,
            rotation: [0.0; 3],
            scale: [1.0; 3],
            color: [1.0; 3],
        }
    }
}

/// Many copies of a mesh: one placement each, drawn in a single draw call
/// by `WorldMesh::draw_instanced`. A forest is one tree mesh and one set of
/// placements, not a mesh per tree, so the graphics card does the
/// repetition and the CPU sends one command.
///
/// Placements are given relative to an `anchor` in full `f64` world space
/// (the corner of a chunk of ground, say), so they stay small numbers. The
/// set keeps them and, whenever the render origin moves, works out each
/// copy's position relative to it in `f64` before it drops to `f32`, so
/// copies are as precise far from the world's centre as near it.
pub struct WorldInstances {
    buffer: wgpu::Buffer,
    count: u32,
    anchor: [f64; 3],
    placements: Vec<Placement>,
    origin: Option<[f64; 3]>,
}

impl WorldInstances {
    /// Upload `placements` around `anchor`. Call `set_render_origin` before
    /// the first draw. None when there are no placements.
    pub fn new(device: &wgpu::Device, anchor: [f64; 3], placements: &[Placement]) -> Option<Self> {
        if placements.is_empty() {
            return None;
        }
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("module instances"),
            size: (placements.len() * std::mem::size_of::<InstanceRaw>()) as wgpu::BufferAddress,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        Some(WorldInstances {
            buffer,
            count: placements.len() as u32,
            anchor,
            placements: placements.to_vec(),
            origin: None,
        })
    }

    /// Where the editor draws from this frame (see
    /// `ModuleGpu::set_render_origin`). Rewrites the copies' positions only
    /// when the origin has moved, which is rare.
    pub fn set_render_origin(&mut self, queue: &wgpu::Queue, origin: [f64; 3]) {
        if self.origin == Some(origin) {
            return;
        }
        self.origin = Some(origin);
        let raw = raw_instances(self.anchor, origin, &self.placements);
        queue.write_buffer(&self.buffer, 0, bytemuck::cast_slice(&raw));
    }

    /// How many copies there are.
    pub fn len(&self) -> u32 {
        self.count
    }

    /// Never true: an empty set is never made.
    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    /// A box (min, max) around every copy, relative to the render origin,
    /// for a mesh that reaches at most `radius` world units from its own
    /// origin at scale 1, so the whole set can be skipped when a view cannot
    /// see it (`DrawView::sees_box`). None before `set_render_origin`.
    pub fn bounds(&self, radius: f32) -> Option<([f32; 3], [f32; 3])> {
        let origin = self.origin?;
        Some(placement_bounds(
            self.anchor,
            origin,
            &self.placements,
            radius,
        ))
    }

    /// GPU memory the placements take, in bytes.
    pub fn bytes(&self) -> u64 {
        self.buffer.size()
    }
}

/// The instance records for `placements` around `anchor`, drawn relative to
/// `origin`: the subtraction is done in `f64`, then each position drops to
/// `f32`. The shader blows every mesh up by the entity size, so the scale is
/// divided by it to leave the mesh in world units.
fn raw_instances(anchor: [f64; 3], origin: [f64; 3], placements: &[Placement]) -> Vec<InstanceRaw> {
    let base = [
        anchor[0] - origin[0],
        anchor[1] - origin[1],
        anchor[2] - origin[2],
    ];
    placements
        .iter()
        .map(|p| InstanceRaw {
            position: [
                (base[0] + f64::from(p.position[0])) as f32,
                (base[1] + f64::from(p.position[1])) as f32,
                (base[2] + f64::from(p.position[2])) as f32,
            ],
            color: p.color,
            selected: 0.0,
            scale: p.scale.map(|s| s / QUAD_SIZE),
            emissive: 0.0,
            rotation: p.rotation,
        })
        .collect()
}

/// The box around every copy of a mesh reaching `radius` (see
/// `WorldInstances::bounds`).
fn placement_bounds(
    anchor: [f64; 3],
    origin: [f64; 3],
    placements: &[Placement],
    radius: f32,
) -> ([f32; 3], [f32; 3]) {
    let mut min = [f32::MAX; 3];
    let mut max = [f32::MIN; 3];
    for raw in raw_instances(anchor, origin, placements)
        .iter()
        .zip(placements)
        .map(|(r, p)| (r.position, p.scale))
    {
        let (position, scale) = raw;
        let reach = radius.max(0.0) * scale.iter().fold(0.0f32, |a, s| a.max(s.abs()));
        for a in 0..3 {
            min[a] = min[a].min(position[a] - reach);
            max[a] = max[a].max(position[a] + reach);
        }
    }
    (min, max)
}

/// The single placement a module's meshes are drawn with: a world position
/// and a colour. Vertices are used as plain world units.
pub struct WorldInstance {
    buffer: wgpu::Buffer,
}

impl WorldInstance {
    pub fn new(device: &wgpu::Device) -> Self {
        WorldInstance {
            buffer: device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("module instance buffer"),
                size: std::mem::size_of::<InstanceRaw>() as wgpu::BufferAddress,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }),
        }
    }

    pub fn set(&self, queue: &wgpu::Queue, position: [f32; 3], color: [f32; 3]) {
        // The shader multiplies every mesh by the entity size, so scale by
        // its inverse to leave the vertices as world units.
        let instance = InstanceRaw {
            position,
            color,
            selected: 0.0,
            scale: [1.0 / QUAD_SIZE; 3],
            emissive: 0.0,
            rotation: [0.0; 3],
        };
        queue.write_buffer(&self.buffer, 0, bytemuck::cast_slice(&[instance]));
    }

    pub fn bind(&self, pass: &mut wgpu::RenderPass<'_>) {
        pass.set_vertex_buffer(1, self.buffer.slice(..));
    }
}

/// The editor's own widgets, so a module's Inspector section looks the same.
pub mod widgets {
    pub use crate::{outerface_checkbox, section_label};
}

pub(crate) fn module_scenes(
    modules: &std::cell::RefCell<Vec<Box<dyn EditorModule>>>,
    world: &World,
    hold: bool,
    viewers: &[[f64; 3]],
) -> Vec<Option<ModuleScene>> {
    modules
        .borrow_mut()
        .iter_mut()
        .map(|m| {
            m.set_viewers(viewers);
            m.build_scene(world, hold)
        })
        .collect()
}

/// The name of the first module still working in the background, if any.
pub(crate) fn busy_module(
    modules: &std::cell::RefCell<Vec<Box<dyn EditorModule>>>,
) -> Option<String> {
    modules
        .borrow()
        .iter()
        .find(|m| m.busy())
        .map(|m| m.name().to_string())
}

/// Every module's overlay lines, each headed by the module's name.
pub(crate) fn module_stats(
    modules: &std::cell::RefCell<Vec<Box<dyn EditorModule>>>,
) -> Vec<String> {
    let mut out = Vec::new();
    for m in modules.borrow().iter() {
        let lines = m.stats();
        if !lines.is_empty() {
            out.push(format!("{}:", m.name()));
            out.extend(lines.into_iter().map(|l| format!("  {l}")));
        }
    }
    out
}

/// The first sun any module asks for this frame, if any.
pub(crate) fn module_sun(
    modules: &std::cell::RefCell<Vec<Box<dyn EditorModule>>>,
    world: &World,
) -> Option<SunOverride> {
    modules
        .borrow_mut()
        .iter_mut()
        .find_map(|module| module.sun(world))
}

/// The farthest any module's scene reaches (see
/// [`EditorModule::view_distance`]), or None when none draws to a distance.
pub(crate) fn module_view_distance(
    modules: &std::cell::RefCell<Vec<Box<dyn EditorModule>>>,
    world: &World,
) -> Option<f64> {
    modules
        .borrow()
        .iter()
        .filter_map(|m| m.view_distance(world))
        .filter(|r| r.is_finite() && *r > 0.0)
        .reduce(f64::max)
}

/// The tide, from the first module that keeps one (see
/// [`EditorModule::tide`]).
pub(crate) fn module_tide(
    modules: &std::cell::RefCell<Vec<Box<dyn EditorModule>>>,
    world: &World,
) -> Option<f32> {
    modules
        .borrow_mut()
        .iter_mut()
        .find_map(|module| module.tide(world))
        .filter(|t| t.is_finite())
}

/// The first module's moon (see [`EditorModule::moon`]), if any gives one
/// that makes sense.
pub(crate) fn module_moon(
    modules: &std::cell::RefCell<Vec<Box<dyn EditorModule>>>,
    world: &World,
) -> Option<MoonOverride> {
    modules
        .borrow_mut()
        .iter_mut()
        .find_map(|module| module.moon(world))
        .filter(|m| m.direction.iter().all(|v| v.is_finite()) && m.phase.is_finite())
}

/// The first module's wind (see [`EditorModule::wind`]), if any.
pub(crate) fn module_wind(
    modules: &std::cell::RefCell<Vec<Box<dyn EditorModule>>>,
    world: &World,
) -> Option<Wind> {
    modules
        .borrow_mut()
        .iter_mut()
        .find_map(|module| module.wind(world))
        .filter(|w| {
            w.velocity.iter().all(|v| v.is_finite()) && w.drift.iter().all(|v| v.is_finite())
        })
}

/// Let every module advance its own simulation by `dt`. Called once per
/// simulation tick, before `sync_collision`.
/// Whether any module has a change to make to the world (see
/// [`EditorModule::wants_world_edit`]).
pub(crate) fn modules_want_world_edit(
    modules: &std::cell::RefCell<Vec<Box<dyn EditorModule>>>,
) -> bool {
    modules.borrow().iter().any(|m| m.wants_world_edit())
}

/// Let every module that asked make its change to the world.
pub(crate) fn edit_world_modules(
    modules: &std::cell::RefCell<Vec<Box<dyn EditorModule>>>,
    world: &mut World,
) {
    for module in modules.borrow_mut().iter_mut() {
        if module.wants_world_edit() {
            module.edit_world(world);
        }
    }
}

pub(crate) fn tick_modules(
    modules: &std::cell::RefCell<Vec<Box<dyn EditorModule>>>,
    world: &mut World,
    dt: f32,
) {
    for module in modules.borrow_mut().iter_mut() {
        module.tick(world, dt);
    }
}

/// Hand every module's collision geometry to `physics`. Called just before
/// each physics step. A name given by two modules is the later module's.
pub(crate) fn sync_collision(
    modules: &std::cell::RefCell<Vec<Box<dyn EditorModule>>>,
    world: &World,
    physics: &mut frame_engine::physics::Physics,
) {
    let mut sets: Vec<(String, StaticMeshSet)> = Vec::new();
    for module in modules.borrow_mut().iter_mut() {
        for (name, set) in module.collision_meshes(world) {
            sets.retain(|(n, _)| *n != name);
            sets.push((name, set));
        }
    }
    physics.sync_static_meshes(&sets);
}

pub(crate) fn anchor_names(
    modules: &std::cell::RefCell<Vec<Box<dyn EditorModule>>>,
) -> Vec<&'static str> {
    modules
        .borrow()
        .iter()
        .flat_map(|m| m.anchor_components().iter().copied())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn world_with_entity() -> (World, usize) {
        let mut world = World::default();
        let id = world.spawn(
            Position {
                x: 0.0,
                y: 0.0,
                z: 0.0,
            },
            Velocity {
                dx: 0.0,
                dy: 0.0,
                dz: 0.0,
            },
        );
        (world, id)
    }

    #[test]
    fn an_edit_copies_the_entitys_components_and_writes_changes_back() {
        let (mut world, id) = world_with_entity();
        world.ext_set("a", id, &1u32).unwrap();
        world.ext_set("b", id, &2u32).unwrap();
        let mut edit = ExtEdit::from_world(&world, id);
        assert_eq!(edit.get::<u32>("a"), Some(1));
        edit.set("a", &10u32);
        edit.remove("b");
        edit.set("c", &3u32);
        // Nothing reaches the world until the editor applies the edit.
        assert_eq!(world.ext_get::<u32>("a", id), Some(1));
        edit.apply(&mut world, id);
        assert_eq!(world.ext_get::<u32>("a", id), Some(10));
        assert!(!world.ext_has("b", id));
        assert_eq!(world.ext_get::<u32>("c", id), Some(3));
    }

    #[test]
    fn setting_the_same_value_is_not_a_change() {
        let (mut world, id) = world_with_entity();
        world.ext_set("a", id, &1u32).unwrap();
        let before = ExtEdit::from_world(&world, id);
        let mut edit = before.clone();
        edit.set("a", &1u32);
        edit.remove("missing");
        assert!(edit == before, "the Inspector would mark the scene dirty");
    }

    #[test]
    fn an_edit_only_touches_the_names_it_changed() {
        let (mut world, id) = world_with_entity();
        world.ext_set("a", id, &1u32).unwrap();
        world.ext_set("b", id, &2u32).unwrap();
        let mut edit = ExtEdit::from_world(&world, id);
        edit.set("a", &5u32);
        // Another system changes "b" after the snapshot was taken.
        world.ext_set("b", id, &99u32).unwrap();
        edit.apply(&mut world, id);
        assert_eq!(world.ext_get::<u32>("a", id), Some(5));
        assert_eq!(world.ext_get::<u32>("b", id), Some(99));
    }

    /// A module that only remembers where the viewers were.
    struct Watcher {
        seen: std::rc::Rc<std::cell::RefCell<Vec<[f64; 3]>>>,
    }
    impl EditorModule for Watcher {
        fn name(&self) -> &str {
            "watcher"
        }
        fn set_viewers(&mut self, viewers: &[[f64; 3]]) {
            *self.seen.borrow_mut() = viewers.to_vec();
        }
        fn new_gpu(&self, _gpu: &GpuContext) -> Box<dyn ModuleGpu> {
            unreachable!("no GPU in tests")
        }
    }

    #[test]
    fn modules_are_told_where_the_viewers_are_before_they_build() {
        let (world, _) = world_with_entity();
        let seen = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let modules = std::cell::RefCell::new(vec![
            Box::new(Watcher { seen: seen.clone() }) as Box<dyn EditorModule>
        ]);
        let far = [2.0e7, 120.0, -3.5e6];
        let _ = module_scenes(&modules, &world, false, &[far]);
        // Full precision: a viewer 20,000 km out is not rounded to f32.
        assert_eq!(*seen.borrow(), vec![far]);
    }

    #[test]
    fn only_whole_triangles_inside_the_vertices_make_an_indexed_mesh() {
        assert!(indices_fit(&[0, 1, 2, 2, 1, 3], 4));
        assert!(!indices_fit(&[], 4));
        assert!(!indices_fit(&[0, 1], 4));
        assert!(!indices_fit(&[0, 1, 4], 4));
    }

    /// A module that is busy or not, and says so in the overlay.
    struct Worker {
        busy: bool,
    }
    impl EditorModule for Worker {
        fn name(&self) -> &str {
            "worker"
        }
        fn busy(&self) -> bool {
            self.busy
        }
        fn stats(&self) -> Vec<String> {
            vec!["jobs 3".to_string()]
        }
        fn new_gpu(&self, _gpu: &GpuContext) -> Box<dyn ModuleGpu> {
            unreachable!("no GPU in tests")
        }
    }

    #[test]
    fn a_busy_module_keeps_the_editor_drawing_and_reports_its_stats() {
        let modules = RefCell::new(vec![
            Box::new(Watcher {
                seen: std::rc::Rc::new(RefCell::new(Vec::new())),
            }) as Box<dyn EditorModule>,
            Box::new(Worker { busy: false }),
        ]);
        assert_eq!(busy_module(&modules), None);
        // Only modules with something to say get lines, under their name.
        assert_eq!(module_stats(&modules), vec!["worker:", "  jobs 3"]);
        modules.borrow_mut()[1] = Box::new(Worker { busy: true });
        assert_eq!(busy_module(&modules), Some("worker".to_string()));
    }

    struct Floor {
        key: u64,
    }
    impl EditorModule for Floor {
        fn name(&self) -> &str {
            "floor"
        }
        fn collision_meshes(&mut self, _world: &World) -> Vec<(String, StaticMeshSet)> {
            let triangles = vec![
                [-60.0, 0.0, -60.0],
                [-60.0, 0.0, 60.0],
                [60.0, 0.0, 60.0],
                [-60.0, 0.0, -60.0],
                [60.0, 0.0, 60.0],
                [60.0, 0.0, -60.0],
            ];
            vec![(
                "floor".to_string(),
                StaticMeshSet {
                    key: self.key,
                    origin: [0.0; 3],
                    meshes: Arc::new(vec![StaticMesh { triangles }]),
                },
            )]
        }
        fn new_gpu(&self, _gpu: &GpuContext) -> Box<dyn ModuleGpu> {
            unreachable!("no GPU in tests")
        }
    }

    #[test]
    fn module_collision_reaches_the_physics_step() {
        let (mut world, id) = world_with_entity();
        world.positions.get_mut(id).unwrap().y = 12.0;
        world.gravities.insert(id, frame_engine::world::Gravity);
        world
            .rigid_bodies
            .insert(id, frame_engine::world::RigidBody);
        let modules = RefCell::new(vec![Box::new(Floor { key: 1 }) as Box<dyn EditorModule>]);
        let mut physics = Physics::new(GRAVITY_Y);
        for _ in 0..120 {
            sync_collision(&modules, &world, &mut physics);
            physics.step(&mut world, 1.0 / 30.0);
        }
        let y = world.positions.get(id).unwrap().y;
        assert!(
            y > 0.4 && y < 12.0,
            "it should rest on the module's floor: {y}"
        );
        assert_eq!(physics.static_mesh_collider_count(), 1);
        // Removing the module removes the floor.
        modules.borrow_mut().clear();
        sync_collision(&modules, &world, &mut physics);
        assert_eq!(physics.static_mesh_collider_count(), 0);
    }

    #[test]
    fn a_material_needs_pixel_data_of_the_right_size() {
        // `create` runs this check before any GPU call.
        assert!(!pixels_fit(0, 4, 0));
        assert!(!pixels_fit(4, 0, 0));
        assert!(!pixels_fit(2, 2, 15));
        assert!(pixels_fit(2, 2, 16));
    }

    #[test]
    fn modules_are_ticked_with_write_access_to_the_world() {
        struct Counter;
        impl EditorModule for Counter {
            fn name(&self) -> &str {
                "counter"
            }
            fn tick(&mut self, world: &mut World, dt: f32) {
                let n = world.resources.get::<f32>("t").copied().unwrap_or(0.0);
                world.resources.insert("t", n + dt);
            }
            fn new_gpu(&self, _gpu: &GpuContext) -> Box<dyn ModuleGpu> {
                unreachable!()
            }
        }
        let modules = RefCell::new(vec![Box::new(Counter) as Box<dyn EditorModule>]);
        let mut world = World::new();
        tick_modules(&modules, &mut world, 0.5);
        tick_modules(&modules, &mut world, 0.5);
        assert_eq!(world.resources.get::<f32>("t").copied(), Some(1.0));
    }

    #[test]
    fn a_module_edits_the_world_only_when_it_asks() {
        struct Spawner {
            asked: bool,
        }
        impl EditorModule for Spawner {
            fn name(&self) -> &str {
                "spawner"
            }
            fn wants_world_edit(&self) -> bool {
                self.asked
            }
            fn edit_world(&mut self, world: &mut World) {
                world.spawn(
                    Position {
                        x: 0.0,
                        y: 0.0,
                        z: 0.0,
                    },
                    Velocity {
                        dx: 0.0,
                        dy: 0.0,
                        dz: 0.0,
                    },
                );
                self.asked = false;
            }
            fn new_gpu(&self, _gpu: &GpuContext) -> Box<dyn ModuleGpu> {
                unreachable!()
            }
        }
        let modules = RefCell::new(vec![
            Box::new(Spawner { asked: false }) as Box<dyn EditorModule>
        ]);
        let mut world = World::new();
        assert!(!modules_want_world_edit(&modules));
        modules.borrow_mut()[0] = Box::new(Spawner { asked: true });
        assert!(modules_want_world_edit(&modules));
        edit_world_modules(&modules, &mut world);
        assert_eq!(world.positions.iter().flatten().count(), 1);
        assert!(!modules_want_world_edit(&modules));
    }

    struct Anchor;
    impl EditorModule for Anchor {
        fn name(&self) -> &str {
            "anchor"
        }
        fn anchor_components(&self) -> &'static [&'static str] {
            &["anchor_thing"]
        }
        fn new_gpu(&self, _gpu: &GpuContext) -> Box<dyn ModuleGpu> {
            unreachable!("no GPU in tests")
        }
    }

    #[test]
    fn anchor_entities_are_left_out_of_the_shape_pass() {
        let (mut world, anchor) = world_with_entity();
        world.ext_set("anchor_thing", anchor, &1u32).unwrap();
        world.spawn(
            Position {
                x: 1.0,
                y: 0.0,
                z: 0.0,
            },
            Velocity {
                dx: 0.0,
                dy: 0.0,
                dz: 0.0,
            },
        );
        let modules = RefCell::new(vec![Box::new(Anchor) as Box<dyn EditorModule>]);
        let anchors = anchor_names(&modules);
        assert_eq!(anchors, vec!["anchor_thing"]);
        let none = std::collections::HashSet::new();
        let (with, _) =
            build_instances(&world, None, &none, &[], &anchors, None, glam::DVec3::ZERO);
        assert_eq!(with.len(), 1, "only the ordinary entity is drawn");
        let (without, _) = build_instances(&world, None, &none, &[], &[], None, glam::DVec3::ZERO);
        assert_eq!(without.len(), 2, "no module: both drawn");
    }

    #[test]
    fn extra_geometry_alone_still_casts_a_shadow_map() {
        let mut lights = [LightRaw::zeroed(); MAX_LIGHTS];
        lights[0] = LightRaw {
            position_or_direction: [-1.0, 1.0, 0.1, 0.0],
            params: [0.0, 0.0, 1.0, 1.0],
        };
        let view = Mat4::look_at_rh(Vec3::new(0.0, 30.0, 60.0), Vec3::ZERO, Vec3::Y);
        let proj = camera_projection(16.0 / 9.0);
        let camera = (proj * view).to_cols_array_2d();
        let bounds = Some((
            Vec3::new(-200.0, -30.0, -200.0),
            Vec3::new(200.0, 30.0, 200.0),
        ));
        assert_eq!(compute_shadow(&[], &lights, camera, bounds).params[0], 1.0);
        assert_eq!(compute_shadow(&[], &lights, camera, None).params[0], 0.0);
    }

    #[test]
    fn a_world_instance_scale_cancels_the_shaders_mesh_size() {
        // Module meshes are in world units; the shader multiplies by the
        // entity size, so the instance scale must be its inverse.
        assert_eq!(QUAD_SIZE * (1.0 / QUAD_SIZE), 1.0);
        for source in [crate::ENTITY_WGSL, include_str!("shadow.wgsl")] {
            let line = source
                .lines()
                .find(|l| l.starts_with("const MESH_SIZE"))
                .expect("a MESH_SIZE in each shader");
            let value: f32 = line
                .trim_start_matches("const MESH_SIZE: f32 =")
                .trim()
                .trim_end_matches(';')
                .parse()
                .unwrap();
            assert_eq!(value, QUAD_SIZE, "MESH_SIZE must match ENTITY_SIZE: {line}");
        }
    }

    #[test]
    fn a_morph_carries_its_texture_coordinates_and_plain_vertices_keep_their_own() {
        let v = |uv: [f32; 2]| MeshVertexData {
            position: [0.0; 3],
            normal: [0.0, 1.0, 0.0],
            uv,
        };
        let morph = |uv: Option<[f32; 2]>| VertexMorph {
            offset_y: 2.0,
            normal: [0.0, 1.0, 0.0],
            start: 10.0,
            end: 20.0,
            uv,
        };
        let out = morphing_vertices(
            &[v([0.1, 0.2]), v([0.3, 0.4]), v([0.5, 0.6])],
            &[morph(Some([0.9, 0.8])), morph(None)],
        );
        // Slides to the given coordinates.
        assert_eq!(out[0].morph_uv, [0.9, 0.8]);
        assert_eq!(out[0].morph, [2.0, 10.0, 20.0]);
        // No coordinates given, or no morph at all: stays on its own.
        assert_eq!(out[1].morph_uv, [0.3, 0.4]);
        assert_eq!(out[2].morph_uv, [0.5, 0.6]);
        assert_eq!(out[2].morph, [0.0; 3]);
    }

    #[test]
    fn flowing_vertices_carry_their_flow_and_tide_rule() {
        let v = |y: f32| MeshVertexData {
            position: [0.0, y, 0.0],
            normal: [0.0, 1.0, 0.0],
            uv: [0.5, 0.5],
        };
        let flows = [
            WaterFlow {
                velocity: [8.0, -4.0],
                tide: Tide::Sea,
            },
            WaterFlow {
                velocity: [0.0, 100.0],
                tide: Tide::Floods { sea: 2.0 },
            },
        ];
        let out = flowing_vertices(&[v(2.0), v(258.0), v(5.0)], &[], &flows);
        let back = |b: i16| f32::from(b) / 32767.0;
        // Velocity over FLOW_RANGE; the sea follows the tide.
        assert!((back(out[0].water[0]) * FLOW_RANGE - 8.0).abs() < 0.01);
        assert!((back(out[0].water[1]) * FLOW_RANGE + 4.0).abs() < 0.01);
        assert_eq!(out[0].water[2], 32767);
        assert_eq!(out[0].water[3], 0);
        // Too fast is held to the range; flooding water knows how far the
        // still sea is below it.
        assert_eq!(out[1].water[1], 32767);
        assert_eq!(out[1].water[2], -32767);
        assert!((back(out[1].water[3]) * TIDE_REACH + 256.0).abs() < 0.05);
        // No entry: still, and no flow.
        assert_eq!(out[2].water, [0; 4]);
        // Nothing else carries any.
        assert_eq!(morphing_vertices(&[v(1.0)], &[])[0].water, [0; 4]);
    }

    #[test]
    fn layered_vertices_keep_their_layer_where_no_tide_rule_reads_it() {
        let v = MeshVertexData {
            position: [0.0, 3.0, 0.0],
            normal: [0.0, 1.0, 0.0],
            uv: [0.5, 0.5],
        };
        let out = layered_vertices(&[v, v, v, v], &[], &[0.0, 0.5, 2.0]);
        // The tide rule stays 0, so the vertex shader moves nothing.
        assert!(out.iter().all(|o| o.water[..3] == [0, 0, 0]));
        assert_eq!(out[0].water[3], 0);
        assert!((f32::from(out[1].water[3]) / 32767.0 - 0.5).abs() < 1e-4);
        // Past the last layer is the last layer; a missing entry the first.
        assert_eq!(out[2].water[3], 32767);
        assert_eq!(out[3].water[3], 0);
    }

    #[test]
    fn a_camera_view_knows_where_the_eye_is_and_the_shadow_view_does_not() {
        let eye = glam::Vec3::new(12.0, 34.0, -56.0);
        let view = glam::Mat4::look_at_rh(eye, glam::Vec3::new(0.0, 30.0, 0.0), glam::Vec3::Y);
        let proj = glam::Mat4::perspective_rh(1.0, 1.5, 0.1, 1000.0);
        let vp = (proj * view).to_cols_array_2d();
        let got = DrawView::from_view_proj(vp, false).eye().unwrap();
        for a in 0..3 {
            assert!((got[a] - eye[a]).abs() < 0.01, "{got:?}");
        }
        assert!(DrawView::from_view_proj(vp, true).eye().is_none());
        assert!(DrawView::everything().eye().is_none());
    }

    #[test]
    fn a_module_shader_built_on_the_scene_part_is_valid_wgsl() {
        let source = format!(
            "{SCENE_WGSL}
@vertex
fn vs_custom(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {{
    let p = vec3<f32>(f32(i), 0.0, 0.0);
    let t = morph_amount(p, camera.eye.xyz, vec2<f32>(0.0, 1.0));
    return camera.view_proj * vec4<f32>(p + vec3<f32>(0.0, t, 0.0), 1.0);
}}
@fragment
fn fs_custom(@builtin(position) at: vec4<f32>) -> @location(0) vec4<f32> {{
    let lit = vec3<f32>(0.5) * shadow_factor(at.xyz, vec3<f32>(0.0, 1.0, 0.0));
    return vec4<f32>(apply_fog(lit, at.xyz), 1.0);
}}
"
        );
        let module = wgpu::naga::front::wgsl::parse_str(&source).expect("parses");
        wgpu::naga::valid::Validator::new(
            wgpu::naga::valid::ValidationFlags::all(),
            wgpu::naga::valid::Capabilities::all(),
        )
        .validate(&module)
        .expect("validates");
    }

    #[test]
    fn a_plain_material_look_has_one_layer() {
        let look = MaterialLook::default();
        assert_eq!(look.layers, 1);
        assert_eq!(look.detail, 0.0);
    }

    /// A module that keeps a tide (or not).
    struct Sea {
        tide: Option<f32>,
    }
    impl EditorModule for Sea {
        fn name(&self) -> &str {
            "sea"
        }
        fn tide(&mut self, _world: &World) -> Option<f32> {
            self.tide
        }
        fn new_gpu(&self, _gpu: &GpuContext) -> Box<dyn ModuleGpu> {
            unreachable!("no GPU in tests")
        }
    }

    #[test]
    fn the_first_module_with_a_tide_sets_it() {
        let (world, _) = world_with_entity();
        let modules: std::cell::RefCell<Vec<Box<dyn EditorModule>>> =
            std::cell::RefCell::new(vec![
                Box::new(Sea { tide: None }),
                Box::new(Sea { tide: Some(1.25) }),
                Box::new(Sea { tide: Some(-3.0) }),
            ]);
        assert_eq!(module_tide(&modules, &world), Some(1.25));
        let none: std::cell::RefCell<Vec<Box<dyn EditorModule>>> =
            std::cell::RefCell::new(vec![Box::new(Sea {
                tide: Some(f32::NAN),
            })]);
        assert_eq!(module_tide(&none, &world), None);
    }

    /// A module that draws out to a fixed distance (or not at all).
    struct Landscape {
        reach: Option<f64>,
    }
    impl EditorModule for Landscape {
        fn name(&self) -> &str {
            "landscape"
        }
        fn view_distance(&self, _world: &World) -> Option<f64> {
            self.reach
        }
        fn new_gpu(&self, _gpu: &GpuContext) -> Box<dyn ModuleGpu> {
            unreachable!("no GPU in tests")
        }
    }

    #[test]
    fn the_farthest_module_reach_sets_the_view_distance() {
        let (world, _) = world_with_entity();
        let modules = |reaches: &[Option<f64>]| {
            std::cell::RefCell::new(
                reaches
                    .iter()
                    .map(|&reach| Box::new(Landscape { reach }) as Box<dyn EditorModule>)
                    .collect::<Vec<_>>(),
            )
        };
        assert_eq!(module_view_distance(&modules(&[]), &world), None);
        assert_eq!(module_view_distance(&modules(&[None]), &world), None);
        let mixed = modules(&[Some(12_000.0), None, Some(48_000.0)]);
        assert_eq!(module_view_distance(&mixed, &world), Some(48_000.0));
        // Nonsense answers are ignored.
        let odd = modules(&[Some(f64::NAN), Some(-5.0), Some(f64::INFINITY)]);
        assert_eq!(module_view_distance(&odd, &world), None);
    }

    #[test]
    fn a_draw_view_keeps_what_the_camera_sees_and_drops_the_rest() {
        let eye = Vec3::new(0.0, 50.0, 0.0);
        // Looking along -Z, with the editor's own projection.
        let vp = crate::camera_projection(16.0 / 9.0)
            * Mat4::look_at_rh(eye, eye + Vec3::new(0.0, 0.0, -1.0), Vec3::Y);
        let view = DrawView::from_view_proj(vp.to_cols_array_2d(), false);
        let around =
            |x: f32, z: f32, r: f32| view.sees_box([x - r, 40.0, z - r], [x + r, 60.0, z + r]);
        // Ahead, near and very far (there is no far plane).
        assert!(around(0.0, -100.0, 10.0));
        assert!(around(0.0, -90_000.0, 1000.0));
        // Behind, and far off to the side.
        assert!(!around(0.0, 100.0, 10.0));
        assert!(!around(5_000.0, -100.0, 10.0));
        // A big box the camera stands in is seen.
        assert!(around(0.0, 0.0, 500.0));
        // Below the view, straight down out of sight.
        assert!(!view.sees_box([-10.0, -9_000.0, -110.0], [10.0, -8_000.0, -90.0]));
        // A broken matrix sees everything, as does `everything`.
        let broken = DrawView::from_view_proj([[f32::NAN; 4]; 4], true);
        assert!(broken.sees_box([1e9; 3], [1e9; 3]));
        assert!(DrawView::everything().sees_box([0.0; 3], [0.0; 3]));
    }

    #[test]
    fn a_shadow_view_sees_only_its_box() {
        let vp = Mat4::orthographic_rh(-100.0, 100.0, -100.0, 100.0, 0.1, 500.0)
            * Mat4::look_at_rh(Vec3::new(0.0, 300.0, 0.0), Vec3::ZERO, Vec3::Z);
        let view = DrawView::from_view_proj(vp.to_cols_array_2d(), true);
        assert!(view.shadow);
        assert!(view.sees_box([-5.0, 0.0, -5.0], [5.0, 10.0, 5.0]));
        assert!(!view.sees_box([400.0, 0.0, 400.0], [410.0, 10.0, 410.0]));
    }

    #[test]
    fn placements_become_instance_records_relative_to_the_render_origin() {
        // An anchor 20,000 km out, drawn from an origin near it.
        let anchor = [2.0e7, 100.0, -3.0e6];
        let origin = [2.0e7 + 256.0, 0.0, -3.0e6 - 512.0];
        let placements = [
            Placement::at([10.5, 2.0, -3.25]),
            Placement {
                position: [0.0; 3],
                rotation: [1.0, 0.0, 0.5],
                scale: [2.0, 3.0, 2.0],
                color: [0.5, 0.8, 0.5],
            },
        ];
        let raw = raw_instances(anchor, origin, &placements);
        assert_eq!(raw.len(), 2);
        // Exact, though the anchor alone would round badly as f32.
        assert_eq!(raw[0].position, [10.5 - 256.0, 102.0, -3.25 + 512.0]);
        assert_eq!(raw[1].position, [-256.0, 100.0, 512.0]);
        // Scale is in mesh units: one entity size per world unit.
        assert_eq!(raw[0].scale, [1.0 / QUAD_SIZE; 3]);
        assert_eq!(
            raw[1].scale,
            [2.0 / QUAD_SIZE, 3.0 / QUAD_SIZE, 2.0 / QUAD_SIZE]
        );
        assert_eq!(raw[1].rotation, [1.0, 0.0, 0.5]);
        assert_eq!(raw[1].color, [0.5, 0.8, 0.5]);
        assert_eq!(raw[0].selected, 0.0);
    }

    #[test]
    fn the_bounds_hold_every_copy_at_its_size() {
        let placements = [
            Placement::at([0.0, 0.0, 0.0]),
            Placement {
                scale: [1.0, 4.0, 1.0],
                ..Placement::at([100.0, 10.0, -50.0])
            },
        ];
        let (min, max) = placement_bounds([1000.0, 0.0, 0.0], [1000.0, 0.0, 0.0], &placements, 2.0);
        // The tall copy reaches 4 times the radius.
        assert_eq!(min, [-2.0, -2.0, -58.0]);
        assert_eq!(max, [108.0, 18.0, 2.0]);
    }
}
