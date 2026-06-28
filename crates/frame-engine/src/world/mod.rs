mod storage;
use serde::{Deserialize, Serialize};
pub use storage::ComponentStorage;

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
        id
    }

    // removes an entity by clearing its slots, will make the index stay valid now Empty
    pub fn despawn(&mut self, id: usize) {
        if id < self.positions.len() {
            self.positions.remove(id);
            self.velocities.remove(id);
            self.colors.remove(id);
            self.controlled.remove(id);
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
