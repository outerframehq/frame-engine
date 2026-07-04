mod storage;
use serde::{Deserialize, Serialize};
pub use storage::ComponentStorage;

#[derive(Serialize, Deserialize, Clone)]
pub struct Script {
    /// Names a behaviour the runtime knows how to run — e.g. "spinner".
    /// The engine never looks inside this. It's just an identifier the host's
    /// script runtime resolves to actual code.
    pub name: String,
    // Next step, not now — per-entity parameters a script can read.
    // BTreeMap, not HashMap, so iteration order is deterministic:
    // pub vars: std::collections::BTreeMap<String, ScriptValue>,
}

#[derive(Serialize, Deserialize, Clone, Copy)]
pub struct Controlled;

#[derive(Serialize, Deserialize, Clone, Copy)]
pub struct Position {
    pub x: f32,
    pub y: f32,
    pub z: f32,
}

#[derive(Serialize, Deserialize, Clone, Copy)]
pub struct Velocity {
    pub dx: f32,
    pub dy: f32,
    pub dz: f32,
}

#[derive(Serialize, Deserialize, Clone, Copy)]
pub struct Color {
    pub r: f32,
    pub g: f32,
    pub b: f32,
}

#[derive(Serialize, Deserialize, Default)]
pub struct World {
    pub positions: ComponentStorage<Position>,
    pub velocities: ComponentStorage<Velocity>,
    #[serde(default)]
    pub colors: ComponentStorage<Color>,
    #[serde(default)]
    pub controlled: ComponentStorage<Controlled>,
    #[serde(default)]
    pub scales: ComponentStorage<Scale>,
    #[serde(default)]
    pub scripts: ComponentStorage<Script>,
}

#[derive(Serialize, Deserialize, Clone, Copy)]
pub struct Scale {
    pub x: f32,
    pub y: f32,
    pub z: f32,
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
            self.scripts.remove(id);
        }
    }

    /// Serialize the whole world to a RON file.
    pub fn save_to_file(&self, path: &str) -> Result<(), Box<dyn std::error::Error>> {
        let ron = ron::ser::to_string_pretty(self, ron::ser::PrettyConfig::default())?;
        std::fs::write(path, ron)?;
        Ok(())
    }

    /// Load a world back from a RON file, replacing whatever was there.
    pub fn load_from_file(path: &str) -> Result<World, Box<dyn std::error::Error>> {
        let text = std::fs::read_to_string(path)?;
        let world = ron::from_str(&text)?;
        Ok(world)
    }
}

pub trait ScriptRuntime {
    /// Called once per tick, before any entity's script runs. Lets a runtime
    /// advance shared per-tick state (such as a clock exposed to scripts).
    /// Optional — the default does nothing.
    fn begin_tick(&mut self) {}

    /// Run one entity's script for this tick, applying its effects to `world`.
    fn run(&mut self, world: &mut World, entity: usize);
}
