use std::time::{Duration, Instant};

const TICK_RATE: u32 = 30;
const MAX_CATCHUP_TICKS: u32 = 5;

fn main() {
    println!("Frame Engine starting up.");

    let max_accumulator = tick_duration * MAX_CATCHUP_TICKS;
    let tick_duration = Duration::from_secs(1) / TICK_RATE;

    let mut tick: u64 = 0;
    let mut last_time = Instant::now();
    let mut accumulator = Duration::ZERO;

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
            println!("Tick {}", tick); // real simulation will run here
        }
    }
}
