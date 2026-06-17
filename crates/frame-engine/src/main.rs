use std::{
    time::{Duration, Instant},
    vec,
};

mod render;
mod systems;
mod world;
use world::{Position, World};

use crate::world::Velocity;

const TICK_RATE: u32 = 30;
const MAX_CATCHUP_TICKS: u32 = 5;

fn main() {
    println!("Frame Engine starting up.");

    let tick_duration = Duration::from_secs(1) / TICK_RATE;
    let max_accumulator = tick_duration * MAX_CATCHUP_TICKS;

    let mut tick: u64 = 0;
    let mut last_time = Instant::now();
    let mut accumulator = Duration::ZERO;

    let mut world = World {
        positions: vec![
            Some(Position {
                x: 0.0,
                y: 0.0,
                z: 0.0,
            }),
            Some(Position {
                x: 10.0,
                y: 5.0,
                z: 1.0,
            }),
            Some(Position {
                x: -3.0,
                y: 2.0,
                z: 1.0,
            }),
        ],

        velocities: vec![
            Some(Velocity {
                dx: 1.0,
                dy: 0.0,
                dz: 0.0,
            }), // moves entity 0: right
            Some(Velocity {
                dx: 0.0,
                dy: 1.0,
                dz: 0.0,
            }), // moves entity 1: down
            Some(Velocity {
                dx: 1.0,
                dy: 1.0,
                dz: 0.0,
            }), // moves entity 2: diagonally
        ],
    };

    loop {
        let now = Instant::now();
        let delta = now.duration_since(last_time);
        last_time = now; // so next loop measures the gap since this loop, not since startup

        accumulator += delta; // pour that real time into the bucket
        // cap the bucket so big stall cant make us replay endless ticks
        if accumulator > max_accumulator {
            accumulator = max_accumulator;
        }

        while accumulator >= tick_duration {
            accumulator -= tick_duration;
            tick += 1; // add on to tick count
            systems::movement(&mut world); //run the movement system
            println!("Tick {}:", tick);
            render::debug_print(&world);
        }
    }
}
