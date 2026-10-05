use crate::input::{Button, InputState};
use crate::world::World;
use crate::world::{MoveIntent, QueryFilter, ScriptRuntime};

/// Expand an axis-aligned box's half-extents to bound the same box after it's
/// rotated (yaw, pitch and roll). This is a conservative over-approximation,
/// an axis-aligned box that fully contains the rotated one, not true
/// oriented-box precision: each world axis gets the sum of the rotation
/// matrix's absolute row entries times the matching half-extents. Two
/// entities whose real rotated shapes don't actually touch can still be
/// reported as colliding once either is tilted; that's the honest cost of
/// staying axis-aligned rather than doing real oriented-box math, a bigger,
/// separate step. (Rapier-simulated entities use true oriented colliders.)
fn expand_for_rotation(half: [f32; 3], rotation: &crate::world::Rotation) -> [f32; 3] {
    let m = rotation.matrix();
    let mut out = [0.0f32; 3];
    for (axis, slot) in out.iter_mut().enumerate() {
        *slot =
            m[axis][0].abs() * half[0] + m[axis][1].abs() * half[1] + m[axis][2].abs() * half[2];
    }
    out
}

/// Half extents of an entity's axis aligned collision box, per axis. A Plane
/// is a flat floor tile so its box is flat in Y (zero height) to match what is
/// drawn, otherwise things rest on an invisible ledge half an ENTITY_SIZE above
/// the surface. An imported Custom mesh uses its unit space half extents from
/// the world's mesh_meta, fitted to the model at import, and falls back to a
/// full cube box if the metadata is missing. Cubes and spheres use the full
/// scale box.
///
/// `pub(crate)`, not private: `physics::Physics` reuses this to size a
/// rapier collider the same way, so a rapier-simulated entity's box matches
/// the one its hand-rolled sibling would have gotten.
pub(crate) fn half_extents(
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

/// Detect which entity boxes overlap and record the pairs on the world, each
/// with a contact point: the centre of the region where the two boxes
/// overlap, in world space.
///
/// This is detection only, a *trigger*, not physics. It finds overlaps and
/// writes them to `world.collisions`; it never moves anything or changes a
/// velocity. Responding to a collision (separating, bouncing) is a deliberately
/// separate, later concern.
///
/// Each entity's box is axis-aligned, centred on its position, with half-extents
/// of `ENTITY_SIZE * 0.5 * scale` per axis, the same scale-box the editor picks
/// against. It ignores the actual mesh shape, so a sphere and a cube of equal
/// scale collide identically. The sweep is O(n^2) over live entities, which is
/// fine at these counts; a broad phase is a later concern if entity counts grow.
pub fn collision(world: &mut World) {
    // Snapshot every live entity's box first (this borrows the world's storages
    // immutably); the pairwise test below then only touches the local snapshot
    // and `world.collisions`, so there's no borrow clash.
    let mut boxes: Vec<(usize, [f32; 3], [f32; 3])> = Vec::new();
    // A `RigidBody`-marked entity is detected and resolved by rapier
    // instead (see `physics::Physics::step`); including it here too
    // would report the same overlap twice, once from each system.
    for (id, p) in world.positions.query().without(&world.rigid_bodies) {
        let s = world.scales.get(id).copied().unwrap_or_default();
        let mesh = world.meshes.get(id).cloned().unwrap_or_default();
        let rotation = world.rotations.get(id).copied().unwrap_or_default();
        let [hx, hy, hz] = expand_for_rotation(half_extents(&mesh, s, &world.mesh_meta), &rotation);
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
                // The contact point is the centre of the region the two boxes
                // share: per axis, the midpoint between the later of the two
                // mins and the earlier of the two maxes.
                let mut point = [0.0f32; 3];
                for axis in 0..3 {
                    let lo = min_a[axis].max(min_b[axis]);
                    let hi = max_a[axis].min(max_b[axis]);
                    point[axis] = (lo + hi) * 0.5;
                }
                world.collisions.push((*id_a, *id_b, point));
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
/// deep stacks may take a few ticks to settle, fine at these scales. Uses the
/// same axis-aligned scale-boxes as `collision`.
pub fn resolve_collisions(world: &mut World) {
    // Snapshot each live entity's centre, half-extents, and static flag.
    let mut boxes: Vec<(usize, [f32; 3], [f32; 3], bool)> = Vec::new();
    // Same reasoning as `collision` above: rapier resolves a
    // `RigidBody`-marked entity's overlaps itself.
    for (id, p) in world.positions.query().without(&world.rigid_bodies) {
        let s = world.scales.get(id).copied().unwrap_or_default();
        let mesh = world.meshes.get(id).cloned().unwrap_or_default();
        let is_static = world.statics.get(id).is_some();
        let rotation = world.rotations.get(id).copied().unwrap_or_default();
        boxes.push((
            id,
            [p.x, p.y, p.z],
            expand_for_rotation(half_extents(&mesh, s, &world.mesh_meta), &rotation),
            is_static,
        ));
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

/// Move and turn every `Parent`-attached entity to match its parent, plus its
/// own local offset. Meant to run last each tick, after every other system
/// that might move the parent (hand-rolled movement, scripts, physics), so a
/// rider sees the parent's *final* position for that tick, not a stale one
/// from before the parent moved.
///
/// The offset is rotated by the parent's whole orientation (yaw, pitch and
/// roll; see `Rotation::apply`), so an entity mounted behind-and-above a
/// parent stays there as the parent turns or tilts. The child takes the
/// parent's orientation with `offset_yaw` added as an extra turn around the
/// parent's own vertical axis (`Rotation::with_extra_yaw`). For a parent with
/// only a yaw this is exactly the old behaviour: yaw 0 faces -Z, a purely
/// forward local offset points where the parent faces.
///
/// An entity with no `Parent`, or whose named parent has since despawned, is
/// left untouched, the same tolerance a stale `Script` reference gets.
pub fn apply_parenting(world: &mut World) {
    // Ids first, because the loop body writes `Position` and `Rotation`
    // through the whole world, which a live query would still be borrowing.
    let ids: Vec<usize> = world.parents.entities().collect();
    for id in ids {
        let Some(parent) = world.parents.get(id).copied() else {
            continue;
        };
        let Some(parent_pos) = world.positions.get(parent.entity).copied() else {
            continue; // dangling or missing parent: leave this entity as it is
        };
        let parent_rotation = world
            .rotations
            .get(parent.entity)
            .copied()
            .unwrap_or_default();
        let [world_dx, world_dy, world_dz] =
            parent_rotation.apply([parent.offset_x, parent.offset_y, parent.offset_z]);
        if let Some(p) = world.positions.get_mut(id) {
            p.x = parent_pos.x + world_dx;
            p.y = parent_pos.y + world_dy;
            p.z = parent_pos.z + world_dz;
        }
        // Unconditional insert, not `get_mut`: a freshly spawned entity has
        // no `Rotation` at all until something gives it one (the Inspector
        // does, on first edit; a script never has to). A `Parent`-attached
        // entity needs a real, current orientation to be worth attaching at
        // all (a mounted camera's own facing depends on it), so this creates
        // one rather than silently doing nothing for an entity that never
        // happened to get a `Rotation` from anywhere else.
        world
            .rotations
            .insert(id, parent_rotation.with_extra_yaw(parent.offset_yaw));
    }
}

/// Accelerate every falling entity downward (−Y). An entity falls if it carries
/// the `Gravity` marker and isn't `Static`. This adds to velocity, not position,
/// so `movement` integrates it and `resolve_collisions` can arrest it against a
/// floor. Strength is the `GRAVITY` constant.
pub fn gravity(world: &mut World) {
    use crate::world::GRAVITY;
    // A `RigidBody`-marked entity gets rapier's own gravity instead
    // (`physics::Physics::new`'s gravity setting).
    for (_, v) in world
        .velocities
        .query_mut()
        .with(&world.gravities)
        .without(&world.statics)
        .without(&world.rigid_bodies)
    {
        v.dy -= GRAVITY;
    }
}

pub fn run_scripts(
    world: &mut World,
    runtime: &mut dyn ScriptRuntime,
    input: &crate::input::InputState,
) {
    runtime.begin_tick(input);
    // Ids first: a script gets the whole `&mut World`, so no storage can stay
    // borrowed while it runs.
    let ids: Vec<usize> = world.scripts.entities().collect();
    for id in ids {
        runtime.run(world, id);
    }
}

pub fn movement(world: &mut World) {
    // Entities rapier owns are moved by `physics::Physics::step` instead.
    for (_, position, velocity) in world
        .positions
        .join_mut(&world.velocities)
        .without(&world.rigid_bodies)
    {
        position.x += velocity.dx;
        position.y += velocity.dy;
        position.z += velocity.dz;
    }
}

// How far a controlled entity moves per tick while a direction is held.
const INPUT_SPEED: f32 = 1.0;

/// What the held buttons ask for this tick. Up is forward, which is -Z: the
/// direction the editor camera faces at yaw 0.
fn held_intent(input: &InputState) -> MoveIntent {
    let mut dx = 0.0;
    let mut dz = 0.0;
    if input.is_held(Button::Left) {
        dx -= INPUT_SPEED;
    }
    if input.is_held(Button::Right) {
        dx += INPUT_SPEED;
    }
    if input.is_held(Button::Up) {
        dz -= INPUT_SPEED;
    }
    if input.is_held(Button::Down) {
        dz += INPUT_SPEED;
    }
    MoveIntent {
        dx,
        dz,
        jump: input.is_held(Button::Jump),
    }
}

/// Apply one entity's intent. A `RigidBody` entity only records it:
/// `physics::Physics::step` checks it against the world and decides how far
/// it really goes and whether it can jump. Anything else moves straight away
/// and ignores Jump, since the hand-rolled path has no idea of standing on
/// the ground.
fn apply_move(world: &mut World, id: usize, intent: MoveIntent) {
    if world.rigid_bodies.get(id).is_some() {
        world.move_intents.insert(id, intent);
    } else if let Some(position) = world.positions.get_mut(id) {
        position.x += intent.dx;
        position.z += intent.dz;
    }
}

/// Move every controlled entity according to the held input buttons. Runs
/// once per tick, so motion is the same regardless of frame rate. An entity
/// is driven only if it has both a position and the Controlled marker.
pub fn input_movement(world: &mut World, input: &InputState) {
    let intent = held_intent(input);
    if intent == MoveIntent::default() {
        return;
    }
    let ids: Vec<usize> = world
        .controlled
        .join(&world.positions)
        .map(|(id, _, _)| id)
        .collect();
    for id in ids {
        apply_move(world, id, intent);
    }
}

/// Like `input_movement`, but drives exactly one entity rather than every
/// `Controlled` entity at once, from that entity's own input specifically.
/// What a multiplayer server needs: each client's input can only ever move
/// the one entity the server assigned that connection, never anyone else's,
/// since there's nothing in the message a client sends that could name a
/// different one.
pub fn input_movement_for(world: &mut World, entity: usize, input: &InputState) {
    let intent = held_intent(input);
    if intent == MoveIntent::default() {
        return;
    }
    apply_move(world, entity, intent);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::world::{
        Controlled, GRAVITY, Gravity, Parent, Position, RigidBody, Static, Velocity, World,
    };

    fn at(world: &mut World, x: f32, y: f32, z: f32) -> usize {
        world.spawn(
            Position { x, y, z },
            Velocity {
                dx: 0.0,
                dy: 0.0,
                dz: 0.0,
            },
        )
    }

    #[test]
    fn gravity_only_pulls_free_falling_entities() {
        let mut world = World::new();
        let falling = at(&mut world, 0.0, 0.0, 0.0);
        let floating = at(&mut world, 0.0, 0.0, 0.0); // no Gravity marker
        let floor = at(&mut world, 0.0, 0.0, 0.0);
        let physics_owned = at(&mut world, 0.0, 0.0, 0.0);
        for id in [falling, floor, physics_owned] {
            world.gravities.insert(id, Gravity);
        }
        world.statics.insert(floor, Static);
        world.rigid_bodies.insert(physics_owned, RigidBody);
        gravity(&mut world);
        assert_eq!(world.velocities.get(falling).unwrap().dy, -GRAVITY);
        assert_eq!(world.velocities.get(floating).unwrap().dy, 0.0);
        assert_eq!(world.velocities.get(floor).unwrap().dy, 0.0);
        assert_eq!(world.velocities.get(physics_owned).unwrap().dy, 0.0);
    }

    #[test]
    fn movement_applies_velocity_but_skips_physics_owned() {
        let mut world = World::new();
        let a = at(&mut world, 1.0, 2.0, 3.0);
        let b = at(&mut world, 1.0, 2.0, 3.0);
        for id in [a, b] {
            *world.velocities.get_mut(id).unwrap() = Velocity {
                dx: 1.0,
                dy: 1.0,
                dz: 1.0,
            };
        }
        world.rigid_bodies.insert(b, RigidBody);
        movement(&mut world);
        let pa = world.positions.get(a).unwrap();
        assert_eq!((pa.x, pa.y, pa.z), (2.0, 3.0, 4.0));
        let pb = world.positions.get(b).unwrap();
        assert_eq!((pb.x, pb.y, pb.z), (1.0, 2.0, 3.0));
    }

    #[test]
    fn input_moves_controlled_entities_and_records_intent_for_rigid_bodies() {
        let mut world = World::new();
        let walker = at(&mut world, 0.0, 0.0, 0.0);
        let bystander = at(&mut world, 0.0, 0.0, 0.0);
        let rigid = at(&mut world, 0.0, 0.0, 0.0);
        world.controlled.insert(walker, Controlled);
        world.controlled.insert(rigid, Controlled);
        world.rigid_bodies.insert(rigid, RigidBody);
        let mut input = InputState::new();
        input.set(Button::Right, true);
        input_movement(&mut world, &input);
        assert_eq!(world.positions.get(walker).unwrap().x, 1.0);
        assert_eq!(world.positions.get(bystander).unwrap().x, 0.0);
        assert_eq!(world.positions.get(rigid).unwrap().x, 0.0);
        assert_eq!(world.move_intents.get(&rigid).unwrap().dx, 1.0);
    }

    #[test]
    fn parenting_follows_the_parent_and_ignores_dangling_ones() {
        let mut world = World::new();
        let parent = at(&mut world, 10.0, 0.0, 0.0);
        let child = at(&mut world, 0.0, 0.0, 0.0);
        let orphan = at(&mut world, 5.0, 5.0, 5.0);
        world.parents.insert(
            child,
            Parent {
                entity: parent,
                offset_x: 0.0,
                offset_y: 2.0,
                offset_z: 0.0,
                offset_yaw: 0.0,
            },
        );
        world.parents.insert(
            orphan,
            Parent {
                entity: 99,
                offset_x: 0.0,
                offset_y: 0.0,
                offset_z: 0.0,
                offset_yaw: 0.0,
            },
        );
        apply_parenting(&mut world);
        let c = world.positions.get(child).unwrap();
        assert_eq!((c.x, c.y, c.z), (10.0, 2.0, 0.0));
        let o = world.positions.get(orphan).unwrap();
        assert_eq!((o.x, o.y, o.z), (5.0, 5.0, 5.0));
    }

    #[test]
    fn collision_reports_overlaps_and_skips_physics_owned() {
        let mut world = World::new();
        let a = at(&mut world, 0.0, 0.0, 0.0);
        let b = at(&mut world, 4.0, 0.0, 0.0); // overlaps a (boxes are 8 wide)
        let far = at(&mut world, 100.0, 0.0, 0.0);
        let rigid = at(&mut world, 2.0, 0.0, 0.0); // overlaps both, but rapier's
        world.rigid_bodies.insert(rigid, RigidBody);
        collision(&mut world);
        assert_eq!(world.collisions.len(), 1);
        let (x, y, _) = world.collisions[0];
        assert_eq!((x, y), (a, b));
        assert!(
            world
                .collisions
                .iter()
                .all(|(p, q, _)| *p != far && *q != far)
        );
    }

    #[test]
    fn resolve_pushes_dynamic_out_of_static_only() {
        let mut world = World::new();
        let floor = at(&mut world, 0.0, 0.0, 0.0);
        let ball = at(&mut world, 0.0, 6.0, 0.0); // 2 units into the floor
        let other_static = at(&mut world, 0.0, 6.0, 0.0);
        world.statics.insert(floor, Static);
        world.statics.insert(other_static, Static);
        resolve_collisions(&mut world);
        assert_eq!(world.positions.get(floor).unwrap().y, 0.0);
        assert_eq!(world.positions.get(other_static).unwrap().y, 6.0);
        assert_eq!(world.positions.get(ball).unwrap().y, 8.0);
    }

    #[test]
    fn scripts_run_for_every_scripted_entity_in_id_order() {
        use crate::world::Script;
        struct Recorder(Vec<usize>);
        impl ScriptRuntime for Recorder {
            fn begin_tick(&mut self, _input: &InputState) {}
            fn run(&mut self, _world: &mut World, id: usize) {
                self.0.push(id);
            }
        }
        let mut world = World::new();
        let a = at(&mut world, 0.0, 0.0, 0.0);
        let _plain = at(&mut world, 0.0, 0.0, 0.0);
        let c = at(&mut world, 0.0, 0.0, 0.0);
        for id in [a, c] {
            world.scripts.insert(
                id,
                Script {
                    uses: String::new(),
                },
            );
        }
        let mut runtime = Recorder(Vec::new());
        run_scripts(&mut world, &mut runtime, &InputState::new());
        assert_eq!(runtime.0, vec![a, c]);
    }

    #[test]
    fn a_tilted_box_gets_a_bigger_bounding_box() {
        use crate::world::Rotation;
        let half = [4.0, 4.0, 4.0];
        // Unrotated: unchanged.
        let flat = expand_for_rotation(half, &Rotation::default());
        assert_eq!(flat, half);
        // Yaw 45 degrees: x and z grow to 4*(cos+sin), y unchanged.
        let yawed = expand_for_rotation(half, &Rotation::from_yaw(std::f32::consts::FRAC_PI_4));
        let grown = 4.0 * std::f32::consts::SQRT_2;
        assert!((yawed[0] - grown).abs() < 1e-4 && (yawed[2] - grown).abs() < 1e-4);
        assert!((yawed[1] - 4.0).abs() < 1e-4);
        // Pitch 45 degrees: y and z grow, x unchanged.
        let pitched = expand_for_rotation(
            half,
            &Rotation {
                yaw: 0.0,
                pitch: std::f32::consts::FRAC_PI_4,
                roll: 0.0,
            },
        );
        assert!((pitched[0] - 4.0).abs() < 1e-4);
        assert!((pitched[1] - grown).abs() < 1e-4 && (pitched[2] - grown).abs() < 1e-4);
        // A flat plane (zero height) tipped on its side gains height.
        let plane = expand_for_rotation(
            [4.0, 0.0, 4.0],
            &Rotation {
                yaw: 0.0,
                pitch: std::f32::consts::FRAC_PI_2,
                roll: 0.0,
            },
        );
        assert!((plane[1] - 4.0).abs() < 1e-4 && plane[2].abs() < 1e-4);
    }

    #[test]
    fn a_child_follows_a_tilted_parent() {
        use crate::world::Rotation;
        let mut world = World::new();
        let parent = at(&mut world, 0.0, 0.0, 0.0);
        let child = at(&mut world, 0.0, 0.0, 0.0);
        // Parent nose straight up: a child 2 units "forward" (-Z) ends up 2
        // units above it.
        world.rotations.insert(
            parent,
            Rotation {
                yaw: 0.0,
                pitch: std::f32::consts::FRAC_PI_2,
                roll: 0.0,
            },
        );
        world.parents.insert(
            child,
            Parent {
                entity: parent,
                offset_x: 0.0,
                offset_y: 0.0,
                offset_z: -2.0,
                offset_yaw: 0.0,
            },
        );
        apply_parenting(&mut world);
        let p = world.positions.get(child).unwrap();
        assert!(
            p.x.abs() < 1e-5 && (p.y - 2.0).abs() < 1e-5 && p.z.abs() < 1e-5,
            "{} {} {}",
            p.x,
            p.y,
            p.z
        );
        // And it inherits the parent's tilt.
        assert!(
            (world.rotations.get(child).unwrap().pitch - std::f32::consts::FRAC_PI_2).abs() < 1e-5
        );
    }
}
