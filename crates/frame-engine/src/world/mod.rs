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
