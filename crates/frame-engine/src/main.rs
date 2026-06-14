use std::time::{Duration, Instant};

const TICK_RATE: u32 = 30;

fn main() {
    println!("Frame Engine starting up.");

    let tick_duration = Duration::from_secs(1) / TICK_RATE;

    let mut tick: u64 = 0;
    let mut last_time = Instant::now();
    let mut accumulator = Duration::ZERO;

    loop {
        let now = Instant::now();
        let delta = now.duration_since(last_time);
        last_time = now; // so next loop measures the gap since this loop, not since startup

        accumulator += delta; // pour that real time into the bucket

        while accumulator >= tick_duration {
            accumulator -= tick_duration;
            tick += 1; // add on to tick count
            println!("Tick {}", tick); // real simulation will run here
        }
    }
}
