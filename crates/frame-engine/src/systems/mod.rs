use crate::input::{Button, InputState};
use crate::world::ScriptRuntime;
use crate::world::World;

pub fn run_scripts(world: &mut World, runtime: &mut dyn ScriptRuntime) {
    runtime.begin_tick();
    let ids: Vec<usize> = world
        .scripts
        .iter()
        .enumerate()
        .filter_map(|(id, slot)| slot.as_ref().map(|_| id))
        .collect();
    for id in ids {
        runtime.run(world, id);
    }
}

pub fn movement(world: &mut World) {
    for (position_slot, velocity_slot) in world.positions.iter_mut().zip(world.velocities.iter()) {
        if let (Some(position), Some(velocity)) = (position_slot, velocity_slot) {
            position.x += velocity.dx;
            position.y += velocity.dy;
            position.z += velocity.dz;
        }
    }
}

// How far a controlled entity moves per tick while a direction is held.
const INPUT_SPEED: f32 = 1.0;

/// Move every controlled entity according to the held input buttons. Runs once
/// per tick, so motion is the same regardless of frame rate. An entity is
/// driven only if it has both a position and the Controlled marker.
pub fn input_movement(world: &mut World, input: &InputState) {
    // Work out this tick's movement once, from the held buttons.
    let mut dx = 0.0;
    let mut dy = 0.0;
    if input.is_held(Button::Left) {
        dx -= INPUT_SPEED;
    }
    if input.is_held(Button::Right) {
        dx += INPUT_SPEED;
    }
    if input.is_held(Button::Up) {
        dy += INPUT_SPEED;
    }
    if input.is_held(Button::Down) {
        dy -= INPUT_SPEED;
    }
    if dx == 0.0 && dy == 0.0 {
        return;
    }

    // Apply it to every entity that has BOTH a position and the Controlled
    // marker. Zipping the two storages and matching on (Some, Some) is the ECS
    // query: act only where the required components co-exist. We ignore the
    // marker's value with `Some(_)` because it has none, only presence matters.
    for (position_slot, controlled_slot) in world.positions.iter_mut().zip(world.controlled.iter())
    {
        if let (Some(position), Some(_)) = (position_slot, controlled_slot) {
            position.x += dx;
            position.y += dy;
        }
    }
}
