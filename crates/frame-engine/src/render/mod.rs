use crate::world::World;

pub fn debug_print(world: &World) {
    for (id, slot) in world.positions.iter().enumerate() {
        if let Some(position) = slot {
            println!(
                "entity {} at ({}, {}, {})",
                id, position.x, position.y, position.z
            )
        }
    }
}
