use frame_engine::world::ComponentStorage;
use frame_engine::world::World;

fn main() {
    let world = World {
        positions: ComponentStorage::new(),
        velocities: ComponentStorage::new(),
    };

    println!(
        "Editor Started, Engine World created with {} entities",
        world.positions.len()
    );
}
