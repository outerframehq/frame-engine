use crate::world::World;

pub fn movement(world: &mut World) {
    for (position_slot, velocity_slot) in world.positions.iter_mut().zip(world.velocities.iter()) {
        if let (Some(position), Some(velocity)) = (position_slot, velocity_slot) {
            position.x += velocity.dx;
            position.y += velocity.dy;
            position.z += velocity.dz;
        }
    }
}
