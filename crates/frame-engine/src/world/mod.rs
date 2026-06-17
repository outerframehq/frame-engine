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
    pub positions: Vec<Option<Position>>, //one slot per entity
    pub velocities: Vec<Option<Velocity>>,
}

impl World {
    //reuse an empty slot if available, otherwise grow both lists by one

    pub fn spawn(&mut self, position: Position, velocity: Velocity) -> usize {
        // looking for a freed slot
        for id in 0..self.positions.len() {
            if self.positions[id].is_none() {
                self.positions[id] = Some(position);
                self.velocities[id] = Some(velocity);
                return id;
            }
        }
        // no free slot , grow both list by one

        self.positions.push(Some(position));
        self.velocities.push(Some(velocity));
        self.positions.len() - 1
    }

    // removes an entity by clearing its slots, will make the index stay valid now Empty
    pub fn despawn(&mut self, id: usize) {
        if id < self.positions.len() {
            self.positions[id] = None;
            self.velocities[id] = None;
        }
    }
}
