mod storage;
use serde::{Deserialize, Serialize};
pub use storage::ComponentStorage;

/// Type-erased storage for one runtime-registered component type, so `World`
/// can hold arbitrary component types it was never compiled knowing about.
/// Each concrete `ComponentStorage<T>` implements this by delegating to
/// itself; the trait is what lets `World` hold a mix of them behind one
/// `HashMap`.
trait ErasedStorage: std::any::Any {
    /// Remove this entity's value, if it has one, without needing to know T.
    /// This is what `despawn` calls: it can't know every registered type, so
    /// it can't downcast, but it can still ask every storage to forget an id.
    fn remove_erased(&mut self, id: usize);
    fn as_any(&self) -> &dyn std::any::Any;
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any;
    fn clone_box(&self) -> Box<dyn ErasedStorage>;
}

impl<T: Clone + 'static> ErasedStorage for ComponentStorage<T> {
    fn remove_erased(&mut self, id: usize) {
        self.remove(id);
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
    fn clone_box(&self) -> Box<dyn ErasedStorage> {
        Box::new(self.clone())
    }
}

impl Clone for Box<dyn ErasedStorage> {
    fn clone(&self) -> Self {
        self.clone_box()
    }
}

/// One runtime-registered component slot, keyed by name in `World::dynamic`.
/// `type_id` is checked on every access, so a name can never silently be read
/// back as the wrong type; `type_name` exists purely so that mismatch can be
/// reported in a way a person can actually read.
struct DynamicEntry {
    type_id: std::any::TypeId,
    type_name: &'static str,
    storage: Box<dyn ErasedStorage>,
}

impl Clone for DynamicEntry {
    fn clone(&self) -> Self {
        DynamicEntry {
            type_id: self.type_id,
            type_name: self.type_name,
            storage: self.storage.clone_box(),
        }
    }
}

/// The world-space size of an entity at scale 1, in world units. This is a
/// simulation fact (it's what collision boxes are built from), so it lives in
/// the engine. The editor's rendering and picking must use the same value:
/// `QUAD_SIZE` in the editor references this, and `MESH_SIZE` in shader.wgsl
/// (which can't import Rust) must be kept equal to it by hand.
pub const ENTITY_SIZE: f32 = 8.0;

/// Downward (−Y) acceleration added to a falling entity's velocity each tick.
/// A tunable knob: larger falls faster. Only entities with the `Gravity` marker
/// are affected.
pub const GRAVITY: f32 = 0.3;

#[derive(Serialize, Deserialize, Clone)]
pub struct Script {
    /// The name of a script in the world's `script_library`. The behaviour
    /// itself lives once in the library; entities reference it by name, so
    /// editing a library script updates every entity that uses it.
    pub uses: String,
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq)]
pub struct Controlled;

/// Marks an entity as immovable: collision response never pushes it, so other
/// entities rest against it (a floor, a wall) instead of shoving it aside.
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq)]
pub struct Static;

/// Marks an entity as affected by gravity: it accelerates downward (−Y) each
/// tick. Opt-in, so entities without it don't fall. A `Static` entity ignores
/// gravity even if marked.
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq)]
pub struct Gravity;

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq)]
pub struct Position {
    pub x: f32,
    pub y: f32,
    pub z: f32,
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq)]
pub struct Velocity {
    pub dx: f32,
    pub dy: f32,
    pub dz: f32,
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq)]
pub struct Color {
    pub r: f32,
    pub g: f32,
    pub b: f32,
}

/// The primitive shape an entity is drawn as. Per-entity appearance data, on the
/// same footing as `Color` and `Scale`: the engine stores and serializes it but
/// never draws — the editor turns it into geometry. Defaults to `Cube`, so
/// scenes saved before meshes existed load and look exactly as they did.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Default)]
pub enum Mesh {
    #[default]
    Cube,
    Sphere,
    Plane,
    /// An imported model, named after its file in the project's assets
    /// folder ("tree" for assets/tree.obj). Its collision half extents live
    /// in World::mesh_meta under the same name.
    Custom(String),
}

/// Metadata for one imported model, keyed by name in World::mesh_meta. Holds
/// the unit space half extents (each 0.5 or less, same unit sizing as the
/// primitives) that collision fits its box to. Lives in the scene rather than
/// the model file so a scene knows its collision shapes before assets load.
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq)]
pub struct MeshMeta {
    pub half_extents: [f32; 3],
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq)]
pub struct Scale {
    pub x: f32,
    pub y: f32,
    pub z: f32,
}

// NEW: the Material component itself. Same shape as Scale/Color — plain,
// Copy, per-entity data.
/// Per-entity emissive strength: 0.0 is fully lit by the directional light
/// (normal shading), 1.0 ignores it entirely and renders at full color, as
/// if glowing. A tunable knob per entity, same footing as Color and Scale.
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq)]
pub struct Material {
    pub emissive: f32,
}

impl Default for Material {
    fn default() -> Self {
        // 0.0 = normal shading, matches every entity's appearance before
        // Material existed.
        Material { emissive: 0.0 }
    }
}

/// Per-entity yaw rotation, in radians, around the world's vertical (Y) axis.
/// Starts at a single angle rather than a full 3D orientation (pitch and roll
/// too), the same "start minimal, grow later" path Material took with just
/// emissive. 0.0 is unrotated, matching every entity's appearance before
/// Rotation existed. Collision boxes stay axis-aligned and unrotated; a
/// rotated entity's hitbox is a known, already-documented limitation, not a
/// new one this introduces.
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq)]
pub struct Rotation {
    pub yaw: f32,
}

impl Default for Rotation {
    fn default() -> Self {
        Rotation { yaw: 0.0 }
    }
}

/// Per-entity health: how much damage this entity can take before dying.
/// Starts as a single tracked number, the same "start minimal, grow later"
/// path Material took with just emissive: no damage system, no death or
/// despawn behaviour, and no maximum tracked yet. Those are deliberately
/// separate, later steps once something actually needs them; for now this is
/// just data a host (a system, a script, an editor control) can read and
/// change.
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq)]
pub struct Health {
    pub current: f32,
}

impl Default for Health {
    fn default() -> Self {
        // 100.0 is a common, arbitrary starting point, not a rule the engine
        // enforces. Nothing currently reads this as "full" versus "damaged"
        // without a maximum to compare it against.
        Health { current: 100.0 }
    }
}

#[derive(Serialize, Deserialize, Default, Clone)]
pub struct World {
    pub positions: ComponentStorage<Position>,
    pub velocities: ComponentStorage<Velocity>,
    #[serde(default)]
    pub colors: ComponentStorage<Color>,
    #[serde(default)]
    pub controlled: ComponentStorage<Controlled>,
    #[serde(default)]
    pub statics: ComponentStorage<Static>,
    #[serde(default)]
    pub gravities: ComponentStorage<Gravity>,
    /// Metadata for imported meshes, keyed by mesh name. Works like
    /// script_library does for scripts.
    #[serde(default)]
    pub mesh_meta: std::collections::BTreeMap<String, MeshMeta>,
    #[serde(default)]
    pub scales: ComponentStorage<Scale>,
    #[serde(default)]
    pub meshes: ComponentStorage<Mesh>,
    // NEW: materials storage, right next to scales/meshes.
    #[serde(default)]
    pub materials: ComponentStorage<Material>,
    #[serde(default)]
    pub rotations: ComponentStorage<Rotation>,
    #[serde(default)]
    pub healths: ComponentStorage<Health>,
    #[serde(default)]
    pub scripts: ComponentStorage<Script>,
    /// Named, reusable scripts shared across entities. Entities reference these
    /// by name (Step 2). Plain data — name -> Rhai source — so the engine stays
    /// VM-free and the library saves and loads with the scene.
    #[serde(default)]
    pub script_library: std::collections::BTreeMap<String, String>,
    /// Pairs of entity ids whose boxes overlap, as of the last time the
    /// collision system ran, each with the contact point: the centre of the
    /// region where the two boxes overlap, in world space. Transient, derived
    /// state — recomputed each run and never saved with the scene, so it's
    /// skipped by serde and defaults empty.
    #[serde(skip)]
    pub collisions: Vec<(usize, usize, [f32; 3])>,
    /// Runtime-registered component storage, keyed by name, for a component
    /// type the engine was never compiled knowing about. This is the seam a
    /// host (a private game layer, an editor plugin) uses to attach its own
    /// per-entity data without needing to add a field to `World` itself, the
    /// same problem `Material` and `Rotation` would otherwise have to solve
    /// by becoming public engine code just to exist.
    ///
    /// Deliberately does not serialize with the scene yet. `Box<dyn Any>`
    /// can't be serialized generically without either a real dependency just
    /// for this (breaking the engine's zero-new-dependencies rule) or a
    /// hand-rolled per-type registry of serialize functions, a real future
    /// step, not an oversight here. A registered component resets on reload,
    /// the same way `collisions` above is transient by design.
    #[serde(skip)]
    dynamic: std::collections::HashMap<String, DynamicEntry>,
}

impl Default for Scale {
    fn default() -> Self {
        // 1.0 on every axis = unscaled. Deriving Default would give 0.0 — a
        // zero-size, invisible entity.
        Scale {
            x: 1.0,
            y: 1.0,
            z: 1.0,
        }
    }
}

impl Default for Color {
    fn default() -> Self {
        // The same warm yellow the renderer used before colors existed, so
        // entities look unchanged until you deliberately recolor them.
        Color {
            r: 0.95,
            g: 0.85,
            b: 0.35,
        }
    }
}

impl World {
    /// A fresh, empty world. Because `World` derives `Default`, adding a new
    /// component field updates construction in exactly one place: the derive.
    pub fn new() -> Self {
        Self::default()
    }

    pub fn spawn(&mut self, position: Position, velocity: Velocity) -> usize {
        let free_slot = self.positions.iter().position(|slot| slot.is_none());
        let id = free_slot.unwrap_or_else(|| self.positions.len());

        self.positions.insert(id, position);
        self.velocities.insert(id, velocity);
        self.colors.insert(id, Color::default());
        self.scales.insert(id, Scale::default());
        self.meshes.insert(id, Mesh::default());
        // NEW: give every spawned entity a Material, same as Color/Scale/Mesh.
        self.materials.insert(id, Material::default());
        self.rotations.insert(id, Rotation::default());
        self.healths.insert(id, Health::default());
        id
    }

    // removes an entity by clearing its slots, will make the index stay valid now Empty
    pub fn despawn(&mut self, id: usize) {
        if id < self.positions.len() {
            self.positions.remove(id);
            self.velocities.remove(id);
            self.colors.remove(id);
            self.controlled.remove(id);
            self.scales.remove(id);
            self.meshes.remove(id);
            self.materials.remove(id);
            self.rotations.remove(id);
            self.healths.remove(id);
            self.scripts.remove(id);
            //Static and Gravity are markers, easy to forget  here since they
            // have no value to look at.Left out, a freed slot could keep an
            // old marker and hand it to whatever spawns into that slot next
            self.statics.remove(id);
            self.gravities.remove(id);
            // Every registered dynamic component too, without needing to know
            // any of their types: remove_erased is exactly what that's for.
            for entry in self.dynamic.values_mut() {
                entry.storage.remove_erased(id);
            }
        }
    }

    /// Attach a value for a runtime-registered component, keyed by name, to an
    /// entity. The first insert under a given name fixes that name's type for
    /// the rest of the world's life; a later insert under the same name with a
    /// different `T` is refused (returns `false`) rather than corrupting the
    /// slot. Returns `true` on success.
    pub fn insert_dynamic<T: Clone + 'static>(&mut self, name: &str, id: usize, value: T) -> bool {
        let entry = self
            .dynamic
            .entry(name.to_string())
            .or_insert_with(|| DynamicEntry {
                type_id: std::any::TypeId::of::<T>(),
                type_name: std::any::type_name::<T>(),
                storage: Box::new(ComponentStorage::<T>::default()),
            });
        if entry.type_id != std::any::TypeId::of::<T>() {
            return false;
        }
        match entry
            .storage
            .as_any_mut()
            .downcast_mut::<ComponentStorage<T>>()
        {
            Some(storage) => {
                storage.insert(id, value);
                true
            }
            None => false,
        }
    }

    /// Read a runtime-registered component's value for an entity. `None` if
    /// the name isn't registered yet, the entity doesn't have a value under
    /// it, or `T` doesn't match the type that name was first registered with.
    pub fn get_dynamic<T: Clone + 'static>(&self, name: &str, id: usize) -> Option<&T> {
        let entry = self.dynamic.get(name)?;
        if entry.type_id != std::any::TypeId::of::<T>() {
            return None;
        }
        entry
            .storage
            .as_any()
            .downcast_ref::<ComponentStorage<T>>()?
            .get(id)
    }

    /// The mutable counterpart to `get_dynamic`.
    pub fn get_dynamic_mut<T: Clone + 'static>(&mut self, name: &str, id: usize) -> Option<&mut T> {
        let entry = self.dynamic.get_mut(name)?;
        if entry.type_id != std::any::TypeId::of::<T>() {
            return None;
        }
        entry
            .storage
            .as_any_mut()
            .downcast_mut::<ComponentStorage<T>>()?
            .get_mut(id)
    }

    /// Remove a runtime-registered component's value for an entity, if it has
    /// one, from the given name's storage specifically. Does nothing if the
    /// name isn't registered or `T` doesn't match its type; `despawn` uses
    /// `remove_erased` instead, since it needs to clear every registered
    /// storage without knowing any of their types.
    pub fn remove_dynamic<T: Clone + 'static>(&mut self, name: &str, id: usize) {
        if let Some(entry) = self.dynamic.get_mut(name) {
            if entry.type_id == std::any::TypeId::of::<T>() {
                if let Some(storage) = entry
                    .storage
                    .as_any_mut()
                    .downcast_mut::<ComponentStorage<T>>()
                {
                    storage.remove(id);
                }
            }
        }
    }

    /// The type name a registered slot was first created with, for a friendly
    /// error message if a host ever mismatches its own type against a name it
    /// registered earlier (with a different type, in a different build, say).
    pub fn dynamic_type_name(&self, name: &str) -> Option<&'static str> {
        self.dynamic.get(name).map(|entry| entry.type_name)
    }

    /// Every name currently registered as a runtime component, regardless of
    /// type or which entities have a value under it. What a script host scans
    /// to know which dynamic component names might exist to expose, since it
    /// has no other way to enumerate a private plugin's own component types.
    pub fn dynamic_names(&self) -> Vec<String> {
        self.dynamic.keys().cloned().collect()
    }

    /// Serialize the whole world to a RON file.
    pub fn save_to_file(
        &self,
        path: impl AsRef<std::path::Path>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let ron = ron::ser::to_string_pretty(self, ron::ser::PrettyConfig::default())?;
        std::fs::write(path, ron)?;
        Ok(())
    }

    /// Load a world back from a RON file, replacing whatever was there.
    pub fn load_from_file(
        path: impl AsRef<std::path::Path>,
    ) -> Result<World, Box<dyn std::error::Error>> {
        let text = std::fs::read_to_string(path)?;
        let world = ron::from_str(&text)?;
        Ok(world)
    }
}

pub trait ScriptRuntime {
    /// Called once per tick, before any entity's script runs. Lets a runtime
    /// advance shared per-tick state (such as a clock exposed to scripts), and
    /// hands it the currently-held input, since that's also shared context, the
    /// same for every entity, rather than something specific to one. Optional —
    /// the default does nothing.
    fn begin_tick(&mut self, input: &crate::input::InputState) {
        let _ = input;
    }

    /// Run one entity's script for this tick, applying its effects to `world`.
    fn run(&mut self, world: &mut World, entity: usize);
}
