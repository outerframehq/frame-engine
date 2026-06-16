use crate::world::World;

pub fn movement(world: &mut World) {
    for position in world.positions.iter_mut().flatten() {
        position.x += 1.0; //moves every entity along x
    }
}
