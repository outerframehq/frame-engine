use crate::input::{Button, InputState};
use crate::world::ScriptRuntime;
use crate::world::World;

/// Detect which entity boxes overlap and record the pairs on the world.
///
/// This is detection only — a *trigger*, not physics. It finds overlaps and
/// writes them to `world.collisions`; it never moves anything or changes a
/// velocity. Responding to a collision (separating, bouncing) is a deliberately
/// separate, later concern.
///
/// Each entity's box is axis-aligned, centred on its position, with half-extents
/// of `ENTITY_SIZE * 0.5 * scale` per axis — the same scale-box the editor picks
/// against. It ignores the actual mesh shape, so a sphere and a cube of equal
/// scale collide identically. The sweep is O(n^2) over live entities, which is
/// fine at these counts; a broad phase is a later concern if entity counts grow.
pub fn collision(world: &mut World) {
    use crate::world::ENTITY_SIZE;
    let half = ENTITY_SIZE * 0.5;

    // Snapshot every live entity's box first (this borrows the world's storages
    // immutably); the pairwise test below then only touches the local snapshot
    // and `world.collisions`, so there's no borrow clash.
    let mut boxes: Vec<(usize, [f32; 3], [f32; 3])> = Vec::new();
    for (id, slot) in world.positions.iter().enumerate() {
        let Some(p) = slot.as_ref() else { continue };
        let s = world.scales.get(id).copied().unwrap_or_default();
        let (hx, hy, hz) = (half * s.x, half * s.y, half * s.z);
        boxes.push((
            id,
            [p.x - hx, p.y - hy, p.z - hz],
            [p.x + hx, p.y + hy, p.z + hz],
        ));
    }

    world.collisions.clear();
    for i in 0..boxes.len() {
        for j in (i + 1)..boxes.len() {
            let (id_a, min_a, max_a) = &boxes[i];
            let (id_b, min_b, max_b) = &boxes[j];
            // Two boxes overlap only if their intervals overlap on every axis.
            let overlap = min_a[0] <= max_b[0]
                && max_a[0] >= min_b[0]
                && min_a[1] <= max_b[1]
                && max_a[1] >= min_b[1]
                && min_a[2] <= max_b[2]
                && max_a[2] >= min_b[2];
            if overlap {
                world.collisions.push((*id_a, *id_b));
            }
        }
    }
}

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
