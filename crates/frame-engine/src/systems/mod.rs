use crate::input::{Button, InputState};
use crate::world::ScriptRuntime;
use crate::world::World;

/// Half extents of an entity's axis aligned collision box, per axis. A Plane
/// is a flat floor tile so its box is flat in Y (zero height) to match what is
/// drawn, otherwise things rest on an invisible ledge half an ENTITY_SIZE above
/// the surface. An imported Custom mesh uses its unit space half extents from
/// the world's mesh_meta, fitted to the model at import, and falls back to a
/// full cube box if the metadata is missing. Cubes and spheres use the full
/// scale box.
fn half_extents(
    mesh: &crate::world::Mesh,
    scale: crate::world::Scale,
    meta: &std::collections::BTreeMap<String, crate::world::MeshMeta>,
) -> [f32; 3] {
    use crate::world::{ENTITY_SIZE, Mesh};
    let h = ENTITY_SIZE * 0.5;
    match mesh {
        Mesh::Plane => [h * scale.x, 0.0, h * scale.z],
        Mesh::Custom(name) => match meta.get(name) {
            // Unit half extents map to world size through ENTITY_SIZE (a unit
            // half of 0.5 is exactly the primitives' h), then per axis scale.
            Some(m) => [
                ENTITY_SIZE * m.half_extents[0] * scale.x,
                ENTITY_SIZE * m.half_extents[1] * scale.y,
                ENTITY_SIZE * m.half_extents[2] * scale.z,
            ],
            None => [h * scale.x, h * scale.y, h * scale.z],
        },
        _ => [h * scale.x, h * scale.y, h * scale.z],
    }
}

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
    // Snapshot every live entity's box first (this borrows the world's storages
    // immutably); the pairwise test below then only touches the local snapshot
    // and `world.collisions`, so there's no borrow clash.
    let mut boxes: Vec<(usize, [f32; 3], [f32; 3])> = Vec::new();
    for (id, slot) in world.positions.iter().enumerate() {
        let Some(p) = slot.as_ref() else { continue };
        let s = world.scales.get(id).copied().unwrap_or_default();
        let mesh = world.meshes.get(id).cloned().unwrap_or_default();
        let [hx, hy, hz] = half_extents(&mesh, s, &world.mesh_meta);
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

/// Push overlapping entities apart along their least-overlapping axis (the
/// minimum translation vector) so they stop interpenetrating. Entities carrying
/// the `Static` marker don't move: a dynamic-vs-static pair pushes the dynamic
/// entity the full way out, a dynamic-vs-dynamic pair splits the push evenly,
/// and two static entities are left alone. Runs after movement, correcting the
/// overlaps this tick's motion produced.
///
/// Corrections are gathered against a snapshot and applied together, so the pass
/// is deterministic and order-independent within a tick. It is a single pass, so
/// deep stacks may take a few ticks to settle — fine at these scales. Uses the
/// same axis-aligned scale-boxes as `collision`.
pub fn resolve_collisions(world: &mut World) {
    // Snapshot each live entity's centre, half-extents, and static flag.
    let mut boxes: Vec<(usize, [f32; 3], [f32; 3], bool)> = Vec::new();
    for (id, slot) in world.positions.iter().enumerate() {
        let Some(p) = slot.as_ref() else { continue };
        let s = world.scales.get(id).copied().unwrap_or_default();
        let mesh = world.meshes.get(id).cloned().unwrap_or_default();
        let is_static = world.statics.get(id).is_some();
        boxes.push((id, [p.x, p.y, p.z], half_extents(&mesh, s, &world.mesh_meta), is_static));
    }

    // Accumulate corrections keyed by entity, then apply them all at once.
    // (entity id, resolution axis, direction it was pushed, position delta).
    let mut corrections: Vec<(usize, usize, f32, [f32; 3])> = Vec::new();
    for i in 0..boxes.len() {
        for j in (i + 1)..boxes.len() {
            let (id_a, c_a, h_a, static_a) = boxes[i];
            let (id_b, c_b, h_b, static_b) = boxes[j];
            if static_a && static_b {
                continue; // neither can move
            }
            // Overlap on each axis; if any is <= 0 the boxes don't intersect.
            let mut overlap = [0.0f32; 3];
            let mut separated = false;
            for axis in 0..3 {
                let o = (h_a[axis] + h_b[axis]) - (c_a[axis] - c_b[axis]).abs();
                if o <= 0.0 {
                    separated = true;
                    break;
                }
                overlap[axis] = o;
            }
            if separated {
                continue;
            }
            // Resolve along the axis of least overlap (the minimum translation).
            let mut axis = 0;
            for a in 1..3 {
                if overlap[a] < overlap[axis] {
                    axis = a;
                }
            }
            // Push a away from b; if the centres coincide on this axis, pick a
            // stable default direction.
            let dir = if c_a[axis] >= c_b[axis] { 1.0 } else { -1.0 };
            let push = overlap[axis];
            let (share_a, share_b) = match (static_a, static_b) {
                (false, true) => (1.0, 0.0),
                (true, false) => (0.0, 1.0),
                _ => (0.5, 0.5),
            };
            if share_a > 0.0 {
                let mut d = [0.0; 3];
                d[axis] = dir * push * share_a;
                corrections.push((id_a, axis, dir, d));
            }
            if share_b > 0.0 {
                let mut d = [0.0; 3];
                d[axis] = -dir * push * share_b;
                corrections.push((id_b, axis, -dir, d));
            }
        }
    }

    for (id, axis, dir, d) in corrections {
        if let Some(p) = world.positions.get_mut(id) {
            p.x += d[0];
            p.y += d[1];
            p.z += d[2];
        }
        // Kill the velocity heading *into* the surface, so a fallen entity rests
        // instead of accumulating downward speed. Velocity already moving away
        // (a script-driven bounce) is left alone.
        if let Some(v) = world.velocities.get_mut(id) {
            let vc = match axis {
                0 => &mut v.dx,
                1 => &mut v.dy,
                _ => &mut v.dz,
            };
            if *vc * dir < 0.0 {
                *vc = 0.0;
            }
        }
    }
}

/// Accelerate every falling entity downward (−Y). An entity falls if it carries
/// the `Gravity` marker and isn't `Static`. This adds to velocity, not position,
/// so `movement` integrates it and `resolve_collisions` can arrest it against a
/// floor. Strength is the `GRAVITY` constant.
pub fn gravity(world: &mut World) {
    use crate::world::GRAVITY;
    let falling: Vec<usize> = (0..world.velocities.len())
        .filter(|&id| {
            world.velocities.get(id).is_some()
                && world.gravities.get(id).is_some()
                && world.statics.get(id).is_none()
        })
        .collect();
    for id in falling {
        if let Some(v) = world.velocities.get_mut(id) {
            v.dy -= GRAVITY;
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
