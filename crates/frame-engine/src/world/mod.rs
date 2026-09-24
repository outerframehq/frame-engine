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
/// never draws. The editor turns it into geometry. Defaults to `Cube`, so
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

/// A plugin's manifest (its `plugin.ron`), one per folder under a project's
/// `plugins/` directory. Purely descriptive: the plugin's actual content is
/// its Rhai scripts, merged straight into `script_library` under a
/// `<plugin-name>/` prefix when the plugin is enabled, not held here.
#[derive(Serialize, Deserialize, Clone, Default, PartialEq)]
pub struct PluginManifest {
    pub name: String,
    pub version: String,
    pub author: String,
    #[serde(default)]
    pub description: String,
    /// Custom Inspector fields this plugin declares. Each names a dynamic
    /// component (the same key `insert_dynamic`/`get_dynamic` use) and a
    /// label to show next to it. `#[serde(default)]` so a plugin.ron written
    /// before this existed still parses as an empty list.
    ///
    /// This is deliberately data, not code: a plugin can't run arbitrary UI,
    /// it can only describe a field for the editor itself to draw. Rhai has
    /// no bindings to egui, and giving it any would be a much bigger trust
    /// boundary than anything else a script can do, reading and writing
    /// typed World data through a defined, safe surface. Declaring a field
    /// this way, and the editor being the only thing that ever actually
    /// renders anything, keeps a bad plugin's worst case the same as a bad
    /// script's: it does nothing useful, it doesn't get to act like part of
    /// the editor's own chrome.
    #[serde(default)]
    pub fields: Vec<PluginField>,
    /// Menu actions this plugin declares. Same reasoning as `fields`: a
    /// button the editor draws and the editor alone decides what running it
    /// means, never arbitrary plugin code triggered by a click.
    #[serde(default)]
    pub actions: Vec<PluginAction>,
    /// Panels this plugin declares: titled, global sections shown in the
    /// Plugins panel regardless of what entity (if any) is selected, since
    /// none of it is tied to one. Not a new widget type, a panel just
    /// groups fields and actions the editor already knows how to draw, the
    /// same "data, not code" reasoning as everything else here.
    #[serde(default)]
    pub panels: Vec<PluginPanel>,
}

/// One custom Inspector field a plugin declares. `name` must match a dynamic
/// component name the plugin (or its scripts) already uses with
/// `insert_dynamic`/`get_dynamic`; the field only appears in the Inspector
/// for an entity that already has a value under that name; there's no way
/// for the field itself to originate one; the same rule the `custom_<name>`
/// script variables already follow, for the same reason. Values are always
/// `f64`, matching the type the script bridge already uses for dynamic
/// values, so a value a script sets and a value the Inspector edits are the
/// same underlying data, not two incompatible types registered under one
/// name.
#[derive(Serialize, Deserialize, Clone, Default, PartialEq)]
pub struct PluginField {
    pub name: String,
    pub label: String,
    /// An inclusive range for the Inspector's slider. `None` on either side
    /// falls back to a plain drag box with no limit on that side.
    #[serde(default)]
    pub min: Option<f64>,
    #[serde(default)]
    pub max: Option<f64>,
}

/// One menu action a plugin declares: a label, and one of a small, fixed set
/// of things the editor knows how to do with it. Not a callback, not a
/// script the editor runs blind: `PluginActionKind` is a closed enum, so
/// every action a plugin can ever cause is one this codebase has explicitly
/// implemented and reviewed, the same "data, not code" reasoning `fields`
/// already uses.
#[derive(Serialize, Deserialize, Clone, PartialEq)]
pub struct PluginAction {
    pub label: String,
    pub kind: PluginActionKind,
}

/// What a `PluginAction` actually does when clicked. Both variants act on
/// every entity that already qualifies, never a single arbitrary target a
/// plugin gets to pick at click time.
#[derive(Serialize, Deserialize, Clone, PartialEq)]
pub enum PluginActionKind {
    /// Run one of this plugin's own scripts (by its full name, including the
    /// `<plugin-name>/` prefix) once, immediately, for every entity that
    /// currently has it assigned via `Script.uses`. The same execution a
    /// normal tick already does for that entity, just triggered once, right
    /// now, instead of waiting for the next tick.
    RunScript { script: String },
    /// Toggle a named dynamic value between 0.0 and 1.0 (above 0.5 becomes
    /// 0.0, otherwise 1.0) for every entity that currently has a value under
    /// that name. Like `fields`, this can't originate a value on an entity
    /// that doesn't already have one.
    ToggleValue { name: String },
    /// Like `ToggleValue`, but for a global value (`World.globals`) rather
    /// than a per-entity one. Natural for a panel, which isn't tied to any
    /// one entity to begin with.
    ToggleGlobal { name: String },
}

/// One global value's field in a panel: the same shape `PluginField` uses
/// for a per-entity field (a name, a label, an optional min/max range), but
/// `name` here is a key into `World.globals` instead of a dynamic component,
/// and the field isn't tied to any entity. The field only appears if that
/// name already has a value in `globals`, the same "can't originate a
/// value" rule every other declared field and action here follows.
#[derive(Serialize, Deserialize, Clone, Default, PartialEq)]
pub struct PluginPanelField {
    pub name: String,
    pub label: String,
    #[serde(default)]
    pub min: Option<f64>,
    #[serde(default)]
    pub max: Option<f64>,
}

/// One panel a plugin declares: a titled, global section in the Plugins
/// panel. Self-contained rather than referencing the plugin's top-level
/// `fields`/`actions` by index or name, so there's nothing fragile to keep
/// in sync if either list changes; a panel simply lists its own fields and
/// actions directly, reusing the exact same types (`PluginPanelField`,
/// `PluginAction`) the rest of this system already draws.
#[derive(Serialize, Deserialize, Clone, Default, PartialEq)]
pub struct PluginPanel {
    pub title: String,
    #[serde(default)]
    pub fields: Vec<PluginPanelField>,
    #[serde(default)]
    pub actions: Vec<PluginAction>,
}

/// One installed plugin, keyed by name in `World::installed_plugins`: its
/// manifest, re-read fresh from disk on every scan, and whether the project
/// currently has it turned on. `enabled` is deliberately kept separate from
/// the manifest, since it's the project's own choice, not the plugin's own
/// data, and re-scanning must preserve it rather than reset it. Defaults
/// off: a newly discovered plugin, like a freshly dropped-in mod, stays off
/// until switched on, never auto-activated.
///
/// This is scoped deliberately small for now: a plugin only supplies
/// scripts, it has no way to add its own Inspector fields, menu items, or
/// panels to the editor itself. That's a real, intended next step (giving a
/// plugin an actual editor-extension surface, not just entity behaviour),
/// not something this shape rules out, just not built yet.
#[derive(Serialize, Deserialize, Clone, Default, PartialEq)]
pub struct InstalledPlugin {
    pub manifest: PluginManifest,
    pub enabled: bool,
}

/// The physical properties of one kind of substance (a wood, a stone, a
/// metal), keyed by name in `World::substances`, the same shared-registry
/// shape `mesh_meta` and `script_library` already use: many entities can be
/// made of the same substance, so it's a name they reference, not data each
/// entity carries its own copy of.
///
/// This is deliberately just the shape, not a materials list. No named
/// substances (a copper, a granite) are defined here; those are game content,
/// meant to live in a project's own data rather than the engine, the same
/// reasoning that keeps the actual quirks, gods, and material names of any
/// specific game out of this repo.
///
/// Every field is `Option<f32>`, and `None` means the substance genuinely
/// doesn't have that property (a liquid has no yield strength; flint doesn't
/// melt, it shatters), not a gap to be filled in later. A default `Substance`
/// (every field `None`) is a real, valid, empty substance, not a placeholder.
///
/// No blending/mixing logic yet: combining substances into a new one (the
/// freeform metal-mixing the design calls for) is real math deserving its own
/// pass, not folded into the schema itself.
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Default)]
pub struct Substance {
    pub melting_point_c: Option<f32>,
    pub boiling_point_c: Option<f32>,
    /// For a combustible that burns rather than melts (wood), not both.
    pub ignition_point_c: Option<f32>,
    /// A relative, unitless hardness/wear-resistance scale. Not calibrated
    /// against anything yet; comparing two substances' numbers is meaningful,
    /// the numbers alone aren't.
    pub durability: Option<f32>,
    /// Resistance to deformation under load, distinct from durability (a
    /// gear can resist scratching but still bend under repeated stress).
    pub yield_strength: Option<f32>,
    /// How easily it can be shaped once heated, independent of hardness.
    pub malleability: Option<f32>,
    pub specific_heat: Option<f32>,
    pub thermal_conductivity: Option<f32>,
    pub electrical_conductivity: Option<f32>,
    /// Magnetic strength; `None` means not magnetic at all, rather than a
    /// separate bool plus a strength value.
    pub magnetism: Option<f32>,
    pub corrosion_resistance: Option<f32>,
    pub density: Option<f32>,
    pub flammability: Option<f32>,
    pub explosive_potential: Option<f32>,
    pub toxicity: Option<f32>,
}

impl Substance {
    /// Blend two substances into a new one, freeform: no fixed recipes, any
    /// two substances in any ratio. `ratio` is `self`'s share, clamped to
    /// 0.0 to 1.0; `other` gets the rest.
    ///
    /// For each property: if both have a value, they're weighted by `ratio`.
    /// If only one has a value, the missing side is treated as 0.0 rather
    /// than dropping the property, so mixing in a trace of something
    /// magnetic gives a small amount of magnetism, not none. If neither has
    /// a value, the result doesn't either; a blend can't invent a property
    /// neither input had.
    ///
    /// This is deliberately one simple linear blend for every property,
    /// including melting point, which in reality often doesn't blend
    /// linearly at all (a real alloy's melting point can sit well below
    /// either component, a eutectic point). Real-world accuracy isn't the
    /// goal here, the values in this system are already deliberately
    /// different from reality elsewhere in the design, so a uniform, honest
    /// rule beats a "more realistic" one that would only be realistic for
    /// some properties and not others.
    pub fn blend(&self, other: &Substance, ratio: f32) -> Substance {
        let ratio = ratio.clamp(0.0, 1.0);
        Substance {
            melting_point_c: blend_field(self.melting_point_c, other.melting_point_c, ratio),
            boiling_point_c: blend_field(self.boiling_point_c, other.boiling_point_c, ratio),
            ignition_point_c: blend_field(self.ignition_point_c, other.ignition_point_c, ratio),
            durability: blend_field(self.durability, other.durability, ratio),
            yield_strength: blend_field(self.yield_strength, other.yield_strength, ratio),
            malleability: blend_field(self.malleability, other.malleability, ratio),
            specific_heat: blend_field(self.specific_heat, other.specific_heat, ratio),
            thermal_conductivity: blend_field(
                self.thermal_conductivity,
                other.thermal_conductivity,
                ratio,
            ),
            electrical_conductivity: blend_field(
                self.electrical_conductivity,
                other.electrical_conductivity,
                ratio,
            ),
            magnetism: blend_field(self.magnetism, other.magnetism, ratio),
            corrosion_resistance: blend_field(
                self.corrosion_resistance,
                other.corrosion_resistance,
                ratio,
            ),
            density: blend_field(self.density, other.density, ratio),
            flammability: blend_field(self.flammability, other.flammability, ratio),
            explosive_potential: blend_field(
                self.explosive_potential,
                other.explosive_potential,
                ratio,
            ),
            toxicity: blend_field(self.toxicity, other.toxicity, ratio),
        }
    }
}

/// Blend one property between two substances. Both present blends by ratio;
/// one present treats the missing side as 0.0 rather than dropping the
/// property; neither present stays `None`.
fn blend_field(a: Option<f32>, b: Option<f32>, ratio: f32) -> Option<f32> {
    match (a, b) {
        (None, None) => None,
        (a, b) => Some(a.unwrap_or(0.0) * ratio + b.unwrap_or(0.0) * (1.0 - ratio)),
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq)]
pub struct Scale {
    pub x: f32,
    pub y: f32,
    pub z: f32,
}

// The Material component itself. Same shape as Scale/Color, plain,
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

/// What kind of light source an entity with a `Light` component is.
/// Deliberately not reusing `Rotation` for a directional light's direction:
/// `Rotation` is yaw-only (a turn around the world's vertical axis), and a
/// light angled down from the sky needs pitch too, a genuinely different
/// thing from how an entity itself is turned.
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq)]
pub enum LightKind {
    /// An infinitely-distant light with a fixed direction, like a sun. No
    /// falloff with distance. `direction` points toward the light (the same
    /// convention the engine's original hard-coded directional term always
    /// used), not the direction the light travels.
    Directional { direction: [f32; 3] },
    /// A light at the entity's own `Position`, with a limited `range`: a
    /// simple linear falloff to zero at that distance, not a
    /// physically-accurate inverse-square falloff, a deliberately simple
    /// first pass.
    Point { range: f32 },
}

impl Default for LightKind {
    fn default() -> Self {
        // Matches the engine's original hard-coded light_dir exactly, so a
        // scene's first Light, added with defaults, reproduces the old
        // fixed shading rather than looking different from day one.
        LightKind::Directional {
            direction: [0.4, 0.8, 0.6],
        }
    }
}

/// Per-entity light source. Optional, like `Static` or `Gravity`: most
/// entities have none, added only to the ones that should actually
/// illuminate the scene. A small, fixed number of active lights are combined
/// additively in the shader (see the renderer); beyond that cap, additional
/// lights are simply not drawn, a real, named limitation rather than an
/// unbounded cost.
///
/// A scene saved before this existed has no `Light` entities at all, and
/// will render flatter than before, ambient only, no directional shading,
/// until one is added. That's a real, one-time visible change on upgrade,
/// not hidden behind an implicit fallback light the shader would otherwise
/// need to invent.
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq)]
pub struct Light {
    pub kind: LightKind,
    /// A brightness multiplier. 1.0 matches the strength of the original
    /// hard-coded directional light this replaces; a scene with several
    /// active lights will usually want each dimmer than that, or the
    /// combined shading blows out past full white.
    pub intensity: f32,
}

impl Default for Light {
    fn default() -> Self {
        Light {
            kind: LightKind::default(),
            intensity: 1.0,
        }
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
    /// Optional per-entity light sources, the same "most entities have
    /// none" shape as `statics`/`gravities` above.
    #[serde(default)]
    pub lights: ComponentStorage<Light>,
    /// Metadata for imported meshes, keyed by mesh name. Works like
    /// script_library does for scripts.
    #[serde(default)]
    pub mesh_meta: std::collections::BTreeMap<String, MeshMeta>,
    /// Installed plugins, keyed by name, the same shared-registry shape as
    /// `mesh_meta` and `script_library`. What actually makes a plugin
    /// "installed" is its scripts already being merged into
    /// `script_library`, only for the ones marked enabled; this map is the
    /// record of which plugins a project depends on and which are turned on.
    #[serde(default)]
    pub installed_plugins: std::collections::BTreeMap<String, InstalledPlugin>,
    /// Flat, global values, not tied to any entity, keyed by name. What a
    /// plugin panel's fields and `ToggleGlobal` actions read and write.
    /// Unlike the per-entity dynamic component system (`insert_dynamic`/
    /// `get_dynamic`), this needs no type erasure: everything that reads or
    /// writes a global value is `f64`, the same scope the per-entity system
    /// already settled into in practice, so there's no reason to carry the
    /// extra complexity of `Box<dyn Any>` for something that's only ever one
    /// type. A plain map also serializes for free, no special mirroring the
    /// way `dynamic_f64` needed for the per-entity case. Public since there's
    /// no invariant here to protect the way `dynamic`/`dynamic_f64` have to
    /// stay in sync with each other.
    #[serde(default)]
    pub globals: std::collections::BTreeMap<String, f64>,
    /// Physical substance definitions, keyed by name, the same shared-registry
    /// shape as `mesh_meta` and `script_library`. Empty by default; no
    /// substances are defined here, only the shape they'd take.
    #[serde(default)]
    pub substances: std::collections::BTreeMap<String, Substance>,
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
    /// by name. Plain data, name to Rhai source, so the engine stays
    /// VM-free and the library saves and loads with the scene.
    #[serde(default)]
    pub script_library: std::collections::BTreeMap<String, String>,
    /// Pairs of entity ids whose boxes overlap, as of the last time the
    /// collision system ran, each with the contact point: the centre of the
    /// region where the two boxes overlap, in world space. Transient, derived
    /// state, recomputed each run and never saved with the scene, so it's
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
    /// Does not serialize with the scene directly. `Box<dyn Any>` can't be
    /// serialized generically without either a real dependency just for this
    /// (breaking the engine's zero-new-dependencies rule) or a hand-rolled
    /// per-type registry of serialize functions for every type a host might
    /// ever register, genuinely open-ended future work, not solved here. A
    /// registered value of any type other than `f64` still resets on reload,
    /// the same way `collisions` above is transient by design. `f64` values
    /// specifically do survive, through `dynamic_f64` below, since `f64` is
    /// the one type actually in use anywhere today, the script bridge's
    /// `custom_<name>` variables and the Inspector's plugin fields are both
    /// `f64` only.
    #[serde(skip)]
    dynamic: std::collections::HashMap<String, DynamicEntry>,
    /// The `f64` half of `dynamic`, kept in sync with it by `insert_dynamic`
    /// and `remove_dynamic` whenever the concrete type happens to be `f64`,
    /// and the only part of `dynamic` that actually serializes. On load,
    /// `load_from_file` rebuilds that part of `dynamic` from this field, so
    /// `get_dynamic::<f64>` reads correctly afterward without a caller
    /// needing to know any of this happened. A real generic solution
    /// (any `T`, not just `f64`) is still open; this closes the gap for the
    /// type that's genuinely used today rather than leaving persistence
    /// entirely unsolved.
    #[serde(default)]
    dynamic_f64: std::collections::BTreeMap<String, ComponentStorage<f64>>,
}

impl Default for Scale {
    fn default() -> Self {
        // 1.0 on every axis = unscaled. Deriving Default would give 0.0, a
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
            self.lights.remove(id);
            // Every registered dynamic component too, without needing to know
            // any of their types: remove_erased is exactly what that's for.
            for entry in self.dynamic.values_mut() {
                entry.storage.remove_erased(id);
            }
            // dynamic_f64 mirrors part of dynamic (see its own doc comment);
            // clear it here too, or a despawned entity's persisted value
            // could resurrect if a later spawn reuses this same freed slot,
            // the exact bug already fixed once for Static and Gravity.
            for storage in self.dynamic_f64.values_mut() {
                storage.remove(id);
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
        let inserted = match entry
            .storage
            .as_any_mut()
            .downcast_mut::<ComponentStorage<T>>()
        {
            Some(storage) => {
                storage.insert(id, value.clone());
                true
            }
            None => false,
        };
        // f64 mirrors into dynamic_f64 too, so it survives a save/reload;
        // see dynamic_f64's own doc comment for why only f64 gets this yet.
        // Only on an actual successful insert above, never on the type
        // mismatch this function already bailed out of.
        if inserted {
            if let Some(v) = (&value as &dyn std::any::Any).downcast_ref::<f64>() {
                self.dynamic_f64
                    .entry(name.to_string())
                    .or_default()
                    .insert(id, *v);
            }
        }
        inserted
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
        // Mirrors insert_dynamic's own mirroring, so dynamic_f64 never keeps
        // a value the type-erased map itself no longer has.
        if std::any::TypeId::of::<T>() == std::any::TypeId::of::<f64>() {
            if let Some(storage) = self.dynamic_f64.get_mut(name) {
                storage.remove(id);
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
        let mut world: World = ron::from_str(&text)?;
        // `dynamic` itself never serializes (see its own doc comment);
        // rebuild the f64 half of it from `dynamic_f64`, which does, so
        // `get_dynamic::<f64>` reads correctly straight after a load without
        // a caller needing to know any of this happened.
        for (name, storage) in world.dynamic_f64.clone() {
            world.dynamic.insert(
                name,
                DynamicEntry {
                    type_id: std::any::TypeId::of::<f64>(),
                    type_name: std::any::type_name::<f64>(),
                    storage: Box::new(storage),
                },
            );
        }
        Ok(world)
    }
}

pub trait ScriptRuntime {
    /// Called once per tick, before any entity's script runs. Lets a runtime
    /// advance shared per-tick state (such as a clock exposed to scripts), and
    /// hands it the currently-held input, since that's also shared context, the
    /// same for every entity, rather than something specific to one. Optional,
    /// the default does nothing.
    fn begin_tick(&mut self, input: &crate::input::InputState) {
        let _ = input;
    }

    /// Run one entity's script for this tick, applying its effects to `world`.
    fn run(&mut self, world: &mut World, entity: usize);
}
