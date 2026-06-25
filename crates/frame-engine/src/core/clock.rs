use std::time::{Duration, Instant};

/// A fixed-timestep clock. It measures real elapsed time and reports how many
/// fixed-size simulation ticks are owed — so the simulation advances at a steady
/// rate no matter how fast the host loop runs.
pub struct Clock {
    tick_duration: Duration,   // how long one tick represents
    max_accumulator: Duration, // catch-up cap: never owe more than this
    last_time: Instant,        // when we last measured
    accumulator: Duration,     // unspent real time, waiting to become ticks
}

impl Clock {
    /// Run at `tick_rate` ticks/sec, refusing to replay more than
    /// `max_catchup_ticks` after a stall (the spiral-of-death guard).
    pub fn new(tick_rate: u32, max_catchup_ticks: u32) -> Self {
        let tick_duration = Duration::from_secs(1) / tick_rate;
        Clock {
            tick_duration,
            max_accumulator: tick_duration * max_catchup_ticks,
            last_time: Instant::now(),
            accumulator: Duration::ZERO,
        }
    }

    /// Call once per host-loop iteration. Returns how many ticks to run now.
    ///
    /// `running` lets a caller pause: when false, the clock keeps its timing
    /// current but accumulates nothing and returns 0 — so unpausing resumes
    /// cleanly instead of lurching forward by the whole pause length.
    pub fn advance(&mut self, running: bool) -> u32 {
        let now = Instant::now();
        let delta = now.duration_since(self.last_time);
        self.last_time = now;

        if !running {
            return 0;
        }

        self.accumulator += delta;
        if self.accumulator > self.max_accumulator {
            self.accumulator = self.max_accumulator;
        }

        let mut ticks = 0;
        while self.accumulator >= self.tick_duration {
            self.accumulator -= self.tick_duration;
            ticks += 1;
        }
        ticks
    }
}
