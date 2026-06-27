mod storage;
use serde::{Deserialize, Serialize};
pub use storage::ComponentStorage;

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

#[derive(Serialize, Deserialize)]
pub struct World {
    pub positions: ComponentStorage<Position>,
    pub velocities: ComponentStorage<Velocity>,
}

impl World {
    pub fn spawn(&mut self, position: Position, velocity: Velocity) -> usize {
        // reuse the first freed slot if there is one, otherwise grow the lists by one
        let free_slot = self.positions.iter().position(|slot| slot.is_none());
        let id = free_slot.unwrap_or_else(|| self.positions.len());

        self.positions.insert(id, position);
        self.velocities.insert(id, velocity);
        id
    }

    // removes an entity by clearing its slots, will make the index stay valid now Empty
    pub fn despawn(&mut self, id: usize) {
        if id < self.positions.len() {
            self.positions.remove(id);
            self.velocities.remove(id);
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
