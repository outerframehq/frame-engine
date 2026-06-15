use crate::world::World;

pub fn movement(world: &mut World) {
    for slot in &mut world.positions {
        if let Some(position) = slot {
            position.x += 1.0; //moves every entity along x
        }
    }
}
