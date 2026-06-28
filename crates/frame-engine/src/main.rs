use frame_engine::core::Clock;
use frame_engine::world::{ComponentStorage, Position, Velocity, World};
use frame_engine::{render, systems};

const TICK_RATE: u32 = 30;
const MAX_CATCHUP_TICKS: u32 = 5;

fn main() {
    println!("Frame Engine starting up.");

    let mut clock = Clock::new(TICK_RATE, MAX_CATCHUP_TICKS);
    let mut tick: u64 = 0;

    let mut world = World {
        positions: ComponentStorage::new(),
        velocities: ComponentStorage::new(),
        colors: ComponentStorage::new(),
    };

    world.spawn(
        Position {
            x: 0.0,
            y: 0.0,
            z: 0.0,
        },
        Velocity {
            dx: 1.0,
            dy: 0.0,
            dz: 0.0,
        },
    );
    world.spawn(
        Position {
            x: 10.0,
            y: 5.0,
            z: 1.0,
        },
        Velocity {
            dx: 0.0,
            dy: 1.0,
            dz: 0.0,
        },
    );
    world.spawn(
        Position {
            x: -3.0,
            y: 2.0,
            z: 1.0,
        },
        Velocity {
            dx: 1.0,
            dy: 1.0,
            dz: 0.0,
        },
    );

    world.spawn(
        Position {
            x: 5.0,
            y: 2.0,
            z: 2.0,
        },
        Velocity {
            dx: 1.0,
            dy: 1.0,
            dz: 0.0,
        },
    );

    loop {
        // ask the shared clock how many fixed ticks are owed, then run each.
        // the engine always runs, so we pass `true`.
        let owed = clock.advance(true);
        for _ in 0..owed {
            tick += 1; // add on to tick count
            systems::movement(&mut world); // run the movement system
            if tick % 6 == 0 {
                println!("Tick {}", tick);
                render::debug_print(&world);
            }
        }
    }
}
