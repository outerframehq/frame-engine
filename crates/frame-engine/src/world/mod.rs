pub struct Position {
    pub x: f32,
    pub y: f32,
    pub z: f32,
}

pub struct Entity {
    pub position: Position,
}

pub struct World {
    pub entity: Entity,
}
