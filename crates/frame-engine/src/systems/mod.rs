use crate::input::{Button, InputState};
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

// How far a controlled entity moves per tick while a direction is held.
const INPUT_SPEED: f32 = 1.0;

/// Move the controlled entity according to the held input buttons. Runs once
/// per tick, so motion is the same regardless of frame rate. Does nothing if no
/// entity is controlled or the controlled id has no position.
pub fn input_movement(world: &mut World, input: &InputState, controlled: Option<usize>) {
    let Some(id) = controlled else {
        return;
    };
    let Some(position) = world.positions.get_mut(id) else {
        return;
    };

    if input.is_held(Button::Left) {
        position.x -= INPUT_SPEED;
    }
    if input.is_held(Button::Right) {
        position.x += INPUT_SPEED;
    }
    if input.is_held(Button::Up) {
        position.y += INPUT_SPEED;
    }
    if input.is_held(Button::Down) {
        position.y -= INPUT_SPEED;
    }
}
