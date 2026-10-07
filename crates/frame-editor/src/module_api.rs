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
//! - and leave anchor entities out of the ordinary shape pass
//!   ([`EditorModule::anchor_components`]).
//!
//! A module's own data lives in the world as named extension components
//! (`World::ext_set` / `ext_get`), so it saves and loads with the scene.

use super::*;
use std::collections::{BTreeMap, BTreeSet};

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

    /// Advance whatever this module simulates by `dt` seconds, with write
    /// access to the world (including `world.resources`). Called once per
    /// simulation tick, before collision is synced, in the editor viewport
    /// and in Play alike. Do nothing when there is nothing to advance.
    fn tick(&mut self, _world: &mut World, _dt: f32) {}

    /// Make this module's GPU state for one window.
    fn new_gpu(&self, gpu: &GpuContext) -> Box<dyn ModuleGpu>;
}

/// What a module gets when it makes its per-window GPU state.
pub struct GpuContext<'a> {
    pub device: &'a wgpu::Device,
    pub queue: &'a wgpu::Queue,
    pub(crate) material_layout: &'a wgpu::BindGroupLayout,
    pub(crate) material_sampler: &'a wgpu::Sampler,
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
}

/// Makes [`WorldMaterial`]s for one window.
#[derive(Clone)]
pub struct MaterialFactory {
    layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
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
        if !pixels_fit(width, height, rgba.len()) {
            return None;
        }
        let view = create_material_texture(device, queue, width, height, rgba);
        let bind_group = create_material_bind_group(
            device,
            &self.layout,
            &self.sampler,
            &view,
            roughness.clamp(0.0, 1.0),
            metalness.clamp(0.0, 1.0),
            true,
        );
        Some(WorldMaterial { bind_group })
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

    /// World-space box (min, max) of what this draws, so shadows can cover it.
    fn shadow_bounds(&self) -> Option<([f32; 3], [f32; 3])> {
        None
    }

    /// Whether there is anything to draw.
    fn has_geometry(&self) -> bool;

    /// Draw it. The editor has already set the pipeline and bind groups, for
    /// both the shadow pass and the colour pass.
    fn draw(&self, pass: &mut wgpu::RenderPass<'_>);

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
}

/// An editable copy of one entity's extension components, handed to
/// [`EditorModule::inspect`]. Changes are written back to the world by the
/// editor, and only for names that were actually changed.
#[derive(Clone, PartialEq, Default)]
pub struct ExtEdit {
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

/// A triangle mesh on the GPU, in world units, for a module to draw.
pub struct WorldMesh {
    buffer: wgpu::Buffer,
    count: u32,
}

impl WorldMesh {
    /// Upload `vertices` (a flat triangle list). None when there are none.
    pub fn new(device: &wgpu::Device, vertices: &[MeshVertexData]) -> Option<Self> {
        if vertices.is_empty() {
            return None;
        }
        let verts: Vec<MeshVertex> = vertices
            .iter()
            .map(|v| MeshVertex {
                position: v.position,
                normal: v.normal,
                uv: v.uv,
            })
            .collect();
        let buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("module mesh buffer"),
            contents: bytemuck::cast_slice(&verts),
            usage: wgpu::BufferUsages::VERTEX,
        });
        Some(WorldMesh {
            buffer,
            count: verts.len() as u32,
        })
    }

    /// Draw it. Call `WorldInstance::bind` first.
    pub fn draw(&self, pass: &mut wgpu::RenderPass<'_>) {
        pass.set_vertex_buffer(0, self.buffer.slice(..));
        pass.draw(0..self.count, 0..1);
    }
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
) -> Vec<Option<ModuleScene>> {
    modules
        .borrow_mut()
        .iter_mut()
        .map(|m| m.build_scene(world, hold))
        .collect()
}

/// Let every module advance its own simulation by `dt`. Called once per
/// simulation tick, before `sync_collision`.
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
            y > 1.0 && y < 12.0,
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
        let (with, _) = build_instances(&world, None, &none, &[], &anchors, None);
        assert_eq!(with.len(), 1, "only the ordinary entity is drawn");
        let (without, _) = build_instances(&world, None, &none, &[], &[], None);
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
        let proj = Mat4::perspective_rh(FOV_DEGREES.to_radians(), 16.0 / 9.0, 0.1, 10000.0);
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
        assert_eq!(QUAD_SIZE, 8.0, "shader.wgsl's MESH_SIZE must match");
    }
}
