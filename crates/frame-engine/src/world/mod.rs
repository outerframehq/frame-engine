mod storage;

pub use storage::ComponentStorage;

pub struct Position {
    pub x: f32,
    pub y: f32,
    pub z: f32,
}

pub struct Velocity {
    pub dx: f32,
    pub dy: f32,
    pub dz: f32,
}

pub struct World {
    //one slot per entity, indexed by entity ID
    pub positions: ComponentStorage<Position>,
    pub velocities: ComponentStorage<Velocity>,
}

impl World {
    //reuse an empty slot if available, otherwise grow both lists by one

    pub fn spawn(&mut self, position: Position, velocity: Velocity) -> usize {
        // looking for a freed slot
        let id = self.positions.len();
        // no free slot , grow both list by one
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
}
