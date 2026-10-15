// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

pub mod clock;
pub use clock::Clock;

/// Simulation ticks per second. Every host runs the world at this rate, so a
/// tick is always `1.0 / TICK_RATE as f32` seconds.
pub const TICK_RATE: u32 = 30;
