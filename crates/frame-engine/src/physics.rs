// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! rapier3d integration: the engine's physics-simulated entities.
//!
//! First real slice of the physics foundation named on the roadmap (see
//! DESIGN.md and Roadmap and Decisions), not the whole thing. Deliberately
//! narrow, the same "ship a real, named-limitation slice" shape Sound and
//! Light shipped in:
//!
//! - `Static` maps to a fixed rapier body, `Gravity`-without-`Static` to a
//!   dynamic one, and `Controlled` to a kinematic character (see
//!   `move_characters`). Plain `movement` (a `Velocity` with none of those
//!   markers) has no rapier equivalent yet.
//! - A `Controlled` character is a kinematic body moved by rapier's
//!   `KinematicCharacterController`, not by forces. Input leaves an intent in
//!   `World.move_intents`; the controller sweeps the collider through the
//!   world and moves it as far as it can go, sliding along walls. `Gravity`
//!   makes it fall; without `Gravity` it floats but still collides. A
//!   grounded `Gravity` character jumps on a fresh press of Jump. A moving
//!   character turns to face the way it's going, at `TURN_SPEED`. Not yet:
//!   pushing dynamic bodies, or moving relative to the entity's yaw.
//!   `Controlled` together with `Static` is contradictory and skipped.
//! - Orientation follows the full `Rotation` component (yaw, pitch and
//!   roll). A `Static` body keeps whatever tilt it was given, so a tilted
//!   ramp is a tilted collider. A dynamic body is free to tumble, and its
//!   orientation is read back into `Rotation` every tick. A `Controlled`
//!   character stays upright: its pitch and roll are ignored and it only
//!   turns around the world's Y axis.
//! - Scene persistence is deliberately out of scope: which entities opt in
//!   is real scene data (the `RigidBody` marker, serialized like
//!   `Static`/`Gravity`), but the rapier state itself (handles, bodies,
//!   colliders) is rebuilt fresh every run, the same "rebuildable runtime
//!   state, not scene data" reasoning `World::collisions` already documents
//!   for itself. Physics state surviving a save/reload is open future work.
//!
//! Kept as its own struct passed alongside `World` rather than a field on
//! `World` itself, the same shape `ScriptRuntime` already takes in
//! `systems::run_scripts`: rapier's sets aren't `Clone`/`Serialize` the way
//! `World`'s own derives need, and don't belong in a saved scene anyway.
//!
//! Written against rapier3d 0.35's real published docs (docs.rs), not from
//! memory or from the earlier speculative research note (which guessed
//! wrong on a couple of points, corrected here: the engine's own
//! `Position`/`Rotation` are plain `f32` fields, not `glam` types, and
//! rapier3d ships its own convenience `pipeline::PhysicsWorld` bundling
//! every low-level piece, so this doesn't hand-wire `PhysicsPipeline::step`'s
//! dozen arguments itself). docs.rs still wasn't the last word, though: a
//! real `cargo build` caught two things the docs got wrong or this note
//! guessed wrong from them, both fixed here rather than in the docs:
//! `RigidBodySet::remove` takes six arguments in this build, not the seven
//! docs.rs showed (no `soft_bodies` parameter, and `PhysicsWorld` itself has
//! no such field either); and reading a rotation back through
//! `Quat::to_euler` needs a `glam::EulerRot` value, but this crate's own
//! directly-declared `glam` dependency resolved to a different version than
//! the one rapier vendors internally through `glamx`, so the two `EulerRot`
//! types didn't unify. Fixed by dropping the direct `glam` dependency
//! entirely and reading yaw back through plain field access on the
//! quaternion instead (see `write_back`), which needs no import at all.
//! `RigidBodyBuilder::rotation` takes an axis-angle vector (confirmed in the
//! rapier3d 0.35.3 source: it calls `Rotation::from_scaled_axis`). rapier
//! turns the opposite way to the engine's own yaw, so every rotation passes
//! through `rapier_rotation`, which negates it.

use rapier3d::control::{CharacterLength, KinematicCharacterController};
use rapier3d::prelude::*;

use crate::systems::half_extents;
use crate::world::{Position, World};

/// Downward acceleration for every physics body, in world units per second
/// squared. Scaled to this engine's size: treating an 8-unit entity as about
/// 1.8 m tall makes 1 unit about 0.22 m, so real gravity (9.81 m/s^2) is
/// about 43.6 units/s^2. Make it less negative for floatier falls and jumps.
pub const GRAVITY_Y: f32 = -43.6;

/// How fast a character turns to face the way it's moving, in radians per
/// second. 10.0 is a snappy half turn in about a third of a second. Lower
/// is a slower, heavier turn.
pub const TURN_SPEED: f32 = 10.0;

/// Roughly how high a character's jump peaks above where it left the
/// ground, in world units. 8.0 is one default entity height. The launch speed is worked
/// out from this and the gravity, so changing gravity keeps the same height.
pub const JUMP_HEIGHT: f32 = 8.0;

/// Frame Engine's yaw as a rapier rotation. The engine's yaw turns clockwise
/// seen from above (towards +X), the way the shader draws it, so yaw 0 faces
/// -Z and yaw pi/2 faces +X. rapier's, like glam's, turns the other way. So
/// the angle is negated here, and again when reading it back in
/// `write_back`, or a turned collider and its drawn mesh point in opposite
/// directions. `from_scaled_axis` is what `RigidBodyBuilder::rotation` uses
/// internally (checked in the rapier3d 0.35.3 source).
fn rapier_rotation(yaw: f32) -> Rotation {
    Rotation::from_scaled_axis(Vector::new(0.0, -f64::from(yaw), 0.0))
}

/// The full engine orientation as a rapier rotation: roll, then pitch, then
/// yaw, the same order as `world::Rotation::matrix`, each angle's sign
/// following the same convention as `rapier_rotation` above.
fn rapier_orientation(rotation: &crate::world::Rotation) -> Rotation {
    rapier_rotation(rotation.yaw)
        * Rotation::from_scaled_axis(Vector::new(f64::from(rotation.pitch), 0.0, 0.0))
        * Rotation::from_scaled_axis(Vector::new(0.0, 0.0, -f64::from(rotation.roll)))
}

/// A rapier orientation (quaternion components) as the engine's yaw, pitch
/// and roll. The inverse of `rapier_orientation`.
fn engine_rotation(x: f32, y: f32, z: f32, w: f32) -> crate::world::Rotation {
    // Standard quaternion to rotation-matrix conversion; the engine's own
    // `Rotation::matrix` is this same matrix in the same space.
    let m = [
        [
            1.0 - 2.0 * (y * y + z * z),
            2.0 * (x * y - z * w),
            2.0 * (x * z + y * w),
        ],
        [
            2.0 * (x * y + z * w),
            1.0 - 2.0 * (x * x + z * z),
            2.0 * (y * z - x * w),
        ],
        [
            2.0 * (x * z - y * w),
            2.0 * (y * z + x * w),
            1.0 - 2.0 * (x * x + y * y),
        ],
    ];
    crate::world::Rotation::from_matrix(m)
}

/// The engine yaw that faces along (dx, dz). Matches `rapier_rotation`'s
/// convention: (0, -1) is 0, (1, 0) is pi/2.
fn yaw_facing(dx: f32, dz: f32) -> f32 {
    dx.atan2(-dz)
}

/// Turn `from` towards `to` by at most `max_step` radians, the short way
/// round, and keep the result between -pi and pi.
fn turn_towards(from: f32, to: f32, max_step: f32) -> f32 {
    use std::f32::consts::{PI, TAU};
    let diff = (to - from + PI).rem_euclid(TAU) - PI;
    let turned = from + diff.clamp(-max_step, max_step);
    (turned + PI).rem_euclid(TAU) - PI
}

/// One fixed triangle-mesh collider, supplied from outside the `World` (a
/// generated terrain chunk, say). `triangles` is a flat list: every three
/// points are one triangle, in the mesh's own local space.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct StaticMesh {
    pub triangles: Vec<[f32; 3]>,
}

/// A group of fixed meshes that move together and are replaced together. All
/// of them sit at `origin` in the world. `key` says which version this is:
/// while it stays the same the physics side keeps what it already built, so
/// handing the same set in every tick costs nothing.
#[derive(Clone, Debug)]
pub struct StaticMeshSet {
    pub key: u64,
    pub origin: [f64; 3],
    pub meshes: std::sync::Arc<Vec<StaticMesh>>,
}

/// Frame Engine's physics state: a rapier3d `PhysicsWorld` plus the
/// entity-id <-> rapier-handle mapping the raw crate has no notion of. Not
/// named `PhysicsWorld` itself to avoid colliding with rapier's own type of
/// that name, which this wraps.
pub struct Physics {
    rapier: PhysicsWorld,
    handles: std::collections::BTreeMap<usize, (RigidBodyHandle, ColliderHandle)>,
    /// Kept so characters can fall at the same rate as dynamic bodies.
    /// Rapier's own gravity only acts on dynamic bodies, never kinematic ones.
    gravity_y: f32,
    /// One controller drives every character. It holds settings only, no
    /// per-entity state.
    controller: KinematicCharacterController,
    /// Per-character runtime state, keyed by entity id. An entity is in here
    /// exactly when it has a kinematic body driven by `controller`.
    characters: std::collections::BTreeMap<usize, Character>,
    /// Fixed mesh colliders supplied from outside the world, by name: the key
    /// of the version built, and the one fixed body holding all its colliders.
    mesh_sets: std::collections::BTreeMap<String, (u64, RigidBodyHandle)>,
}

/// What a character needs to remember between ticks. Runtime only, rebuilt
/// fresh every run like the rest of `Physics`.
#[derive(Default)]
struct Character {
    /// Vertical speed in world units per second. Negative means falling.
    fall_speed: f32,
    /// Whether the last move ended touching the ground. While grounded, the
    /// character doesn't build up falling speed, and it can jump.
    grounded: bool,
    /// Whether Jump was held last tick. A jump needs a fresh press, so
    /// holding Jump down doesn't jump again on landing.
    jump_held: bool,
}

impl Physics {
    /// `gravity_y` is a downward acceleration in world units per second
    /// squared, the real, continuously-integrated quantity rapier expects,
    /// not `world::GRAVITY` (a flat per-tick velocity nudge the hand-rolled
    /// path uses instead, and unaffected by this). Every caller passes
    /// `GRAVITY_Y`, which scales real gravity to this engine's units; see its
    /// own doc comment.
    pub fn new(gravity_y: f32) -> Self {
        let mut rapier = PhysicsWorld::new();
        rapier.gravity = Vector::new(0.0, f64::from(gravity_y), 0.0);
        Physics {
            rapier,
            handles: std::collections::BTreeMap::new(),
            gravity_y,
            controller: KinematicCharacterController {
                offset: CharacterLength::Absolute(0.05),
                ..KinematicCharacterController::default()
            },
            characters: std::collections::BTreeMap::new(),
            mesh_sets: std::collections::BTreeMap::new(),
        }
    }

    /// Make the fixed mesh colliders match `sets`, which are named groups
    /// supplied from outside the `World` (generated terrain, say). A name
    /// that is no longer given is removed, a name whose `key` changed is
    /// rebuilt, and one whose `key` is unchanged is left alone, so this is
    /// cheap to call every tick. Meshes rapier cannot build a collider from
    /// (empty or degenerate) are skipped.
    pub fn sync_static_meshes(&mut self, sets: &[(String, StaticMeshSet)]) {
        let given: std::collections::HashSet<&str> = sets.iter().map(|(n, _)| n.as_str()).collect();
        let gone: Vec<String> = self
            .mesh_sets
            .keys()
            .filter(|name| !given.contains(name.as_str()))
            .cloned()
            .collect();
        for name in gone {
            self.remove_mesh_set(&name);
        }
        for (name, set) in sets {
            if self.mesh_sets.get(name).map(|(key, _)| *key) == Some(set.key) {
                continue;
            }
            self.remove_mesh_set(name);
            let body = RigidBodyBuilder::fixed()
                .translation(Vector::new(set.origin[0], set.origin[1], set.origin[2]))
                .build();
            let body_handle = self.rapier.bodies.insert(body);
            for mesh in set.meshes.iter() {
                let count = mesh.triangles.len() / 3;
                if count == 0 {
                    continue;
                }
                let vertices: Vec<Vector> = mesh.triangles[..count * 3]
                    .iter()
                    .map(|p| Vector::new(f64::from(p[0]), f64::from(p[1]), f64::from(p[2])))
                    .collect();
                let indices: Vec<[u32; 3]> = (0..count as u32)
                    .map(|t| [t * 3, t * 3 + 1, t * 3 + 2])
                    .collect();
                // Welding shared corners and fixing internal edges stops a
                // character or ball catching on the seams between triangles.
                let Ok(builder) = ColliderBuilder::trimesh_with_flags(
                    vertices,
                    indices,
                    TriMeshFlags::FIX_INTERNAL_EDGES | TriMeshFlags::DELETE_DEGENERATE_TRIANGLES,
                ) else {
                    continue;
                };
                self.rapier.colliders.insert_with_parent(
                    builder.build(),
                    body_handle,
                    &mut self.rapier.bodies,
                );
            }
            self.mesh_sets.insert(name.clone(), (set.key, body_handle));
        }
    }

    /// How many fixed mesh colliders are currently built, across every named
    /// set.
    pub fn static_mesh_collider_count(&self) -> usize {
        self.mesh_sets
            .values()
            .filter_map(|(_, body)| self.rapier.bodies.get(*body))
            .map(|body| body.colliders().len())
            .sum()
    }

    fn remove_mesh_set(&mut self, name: &str) {
        if let Some((_, body_handle)) = self.mesh_sets.remove(name) {
            self.rapier.bodies.remove(
                body_handle,
                &mut self.rapier.islands,
                &mut self.rapier.colliders,
                &mut self.rapier.impulse_joints,
                &mut self.rapier.multibody_joints,
                true,
            );
        }
    }

    /// Advance the physics simulation by exactly one fixed tick and write
    /// the result back into every `RigidBody`-marked entity's `Position` and
    /// `Rotation`. `dt` should be the engine's own fixed tick duration
    /// (`1.0 / TICK_RATE` in `main.rs`), so rapier's clock and the engine's
    /// clock agree, the same discipline `core::Clock` already applies to the
    /// hand-rolled systems; passing a wall-clock delta instead would make
    /// rapier disagree with everything else about how much time a tick is.
    pub fn step(&mut self, world: &mut World, dt: f32) {
        self.sync_new_and_removed(world);
        self.move_characters(world, dt);
        self.rapier.integration_parameters.dt = f64::from(dt);
        self.rapier.step();
        self.write_back(world);
    }

    /// Give every newly `RigidBody`-marked entity a real rapier body and
    /// collider, sized and positioned from its current `Position`/`Scale`/
    /// `Mesh`/`Rotation`, and remove the rapier side of any entity this
    /// struct was tracking that's been despawned or lost the marker since.
    /// An entity already tracked is left alone: a rapier body is created
    /// once, then simulated, never rebuilt from `World` every tick (that
    /// would fight with rapier's own integration instead of driving it).
    fn sync_new_and_removed(&mut self, world: &mut World) {
        let marked: Vec<usize> = world
            .rigid_bodies
            .iter()
            .enumerate()
            .filter_map(|(id, slot)| slot.as_ref().map(|_| id))
            .collect();
        for id in marked {
            if self.handles.contains_key(&id) {
                continue;
            }
            let Some(position) = world.positions.get(id).copied() else {
                continue;
            };
            let is_controlled = world.controlled.get(id).is_some();
            let is_static = world.statics.get(id).is_some();
            if is_controlled && is_static {
                // A player-driven entity that is also fixed in place makes no
                // sense. Skipped and retried every tick, in case one marker
                // is removed later.
                continue;
            }
            if !is_controlled && !is_static && world.gravities.get(id).is_none() {
                // Neither mapping in the module doc comment applies; nothing
                // safe to build yet. Same "retry every tick" reasoning as
                // the `Controlled` case above.
                continue;
            }

            let scale = world.scales.get(id).copied().unwrap_or_default();
            let mesh = world.meshes.get(id).cloned().unwrap_or_default();
            if !mesh.has_shape() {
                // Nothing to collide with, so nothing to simulate. Retried
                // every tick, like the cases above, in case it gets a mesh.
                continue;
            }
            let [hx, hy, hz] = half_extents(&mesh, scale, &world.mesh_meta);
            let engine_rot = world.rotations.get(id).copied().unwrap_or_default();

            let builder = if is_controlled {
                // Moved by `move_characters`, not by forces.
                RigidBodyBuilder::kinematic_position_based()
            } else if is_static {
                RigidBodyBuilder::fixed()
            } else {
                RigidBodyBuilder::dynamic()
            };
            // A character stays upright: yaw only, and locked to the Y axis.
            // Everything else uses its full orientation and, if dynamic, may
            // tumble freely.
            let body = if is_controlled {
                builder
                    .rotation(rapier_rotation(engine_rot.yaw).to_scaled_axis())
                    .enabled_rotations(false, true, false)
            } else {
                builder.rotation(rapier_orientation(&engine_rot).to_scaled_axis())
            }
            .translation(Vector::new(position.x, position.y, position.z))
            .build();
            let body_handle = self.rapier.bodies.insert(body);
            let collider =
                ColliderBuilder::cuboid(f64::from(hx), f64::from(hy), f64::from(hz)).build();
            let collider_handle = self.rapier.colliders.insert_with_parent(
                collider,
                body_handle,
                &mut self.rapier.bodies,
            );
            self.handles.insert(id, (body_handle, collider_handle));
            if is_controlled {
                self.characters.insert(id, Character::default());
            }
        }

        let gone: Vec<usize> = self
            .handles
            .keys()
            .copied()
            // Also rebuild an entity whose `Controlled` marker changed since
            // its body was made: a kinematic body can't turn into a dynamic
            // one in place, so remove it and let the next tick build the
            // right kind.
            .filter(|id| {
                world.rigid_bodies.get(*id).is_none()
                    || self.characters.contains_key(id) != world.controlled.get(*id).is_some()
            })
            .collect();
        for id in gone {
            self.characters.remove(&id);
            if let Some((body_handle, _)) = self.handles.remove(&id) {
                // Removing the body also detaches its collider; passing
                // `true` (remove_attached_colliders) is what actually does
                // that rather than leaving an orphaned collider behind.
                // Six arguments, not seven: this build's `RigidBodySet`
                // has no `soft_bodies` parameter here (docs.rs showed one;
                // the real compiler didn't agree, and the compiler wins).
                self.rapier.bodies.remove(
                    body_handle,
                    &mut self.rapier.islands,
                    &mut self.rapier.colliders,
                    &mut self.rapier.impulse_joints,
                    &mut self.rapier.multibody_joints,
                    true,
                );
            }
        }
    }

    /// Move every character by the intent the input systems left in
    /// `World.move_intents`, plus its own falling speed if it has `Gravity`.
    /// The controller sweeps the character's collider through the world and
    /// returns how far it can really go, then that is queued on the kinematic
    /// body for rapier's next step. Uses up all of `move_intents`, so a stale
    /// intent never carries over to the next tick.
    fn move_characters(&mut self, world: &mut World, dt: f32) {
        // Rebuilt fresh every step from `self.characters`, the live set,
        // rather than left to accumulate stale entries for a despawned or
        // un-Controlled entity.
        world.grounded.clear();
        let intents = std::mem::take(&mut world.move_intents);
        // The query pipeline borrows all of `self.rapier`, so a body can't be
        // changed while it's alive. Work out every move first, then apply.
        let mut moves: Vec<(RigidBodyHandle, Vector, f32)> = Vec::new();
        for (&id, character) in self.characters.iter_mut() {
            let Some(&(body_handle, collider_handle)) = self.handles.get(&id) else {
                continue;
            };
            let Some(collider) = self.rapier.colliders.get(collider_handle) else {
                continue;
            };
            let intent = intents.get(&id).copied().unwrap_or_default();
            let jump_pressed = intent.jump && !character.jump_held;
            character.jump_held = intent.jump;
            let falls = world.gravities.get(id).is_some();

            if falls && !character.grounded {
                // Only build up falling speed in the air. Pushing down into
                // the floor every tick while standing makes a box snag on it
                // and stutter when walking; `snap_to_ground` keeps it on the
                // floor instead.
                character.fall_speed += self.gravity_y * dt;
            } else {
                character.fall_speed = 0.0;
            }
            if falls && character.grounded && jump_pressed {
                // Launch speed that peaks at about JUMP_HEIGHT under this
                // gravity, from v^2 = 2 * g * h. Moving in whole ticks
                // overshoots a little (about 0.4 units at 30 ticks/s).
                character.fall_speed = (2.0 * -self.gravity_y * JUMP_HEIGHT).sqrt();
            }
            let desired = Vector::new(
                f64::from(intent.dx),
                f64::from(character.fall_speed * dt),
                f64::from(intent.dz),
            );

            // Leave the character's own collider out of the obstacles it
            // checks against, or it would collide with itself.
            let filter = QueryFilter::default().exclude_rigid_body(body_handle);
            let queries = self.rapier.query_pipeline_with_filter(filter);
            let movement = self.controller.move_shape(
                f64::from(dt),
                &queries,
                collider.shape(),
                collider.position(),
                desired,
                |_| {},
            );

            character.grounded = movement.grounded;
            world.grounded.insert(id, character.grounded);
            if movement.grounded && character.fall_speed < 0.0 {
                // Landed: stop building up speed while standing still.
                character.fall_speed = 0.0;
            }

            // Turn to face the way the character is trying to move. Starts
            // from the entity's own yaw, not the body's, so a yaw set by a
            // script or the Inspector is kept rather than overwritten. With
            // no move this tick the yaw is left as it is.
            let mut yaw = world.rotations.get(id).map(|r| r.yaw).unwrap_or(0.0);
            if intent.dx != 0.0 || intent.dz != 0.0 {
                yaw = turn_towards(yaw, yaw_facing(intent.dx, intent.dz), TURN_SPEED * dt);
            }
            moves.push((
                body_handle,
                collider.position().translation + movement.translation,
                yaw,
            ));
        }
        for (body_handle, target, yaw) in moves {
            if let Some(body) = self.rapier.bodies.get_mut(body_handle) {
                body.set_next_kinematic_translation(target);
                body.set_next_kinematic_rotation(rapier_rotation(yaw));
            }
        }
    }

    /// Copy every tracked entity's simulated position and yaw back into its
    /// `Position`/`Rotation` components. A fixed (`Static`) body never
    /// moves, so this is only ever a real change for a dynamic one, but it
    /// costs nothing to run unconditionally, the same "just recompute it"
    /// choice `systems::movement` already makes for everything else.
    fn write_back(&mut self, world: &mut World) {
        for (&id, (body_handle, _)) in self.handles.iter() {
            let Some(body) = self.rapier.bodies.get(*body_handle) else {
                continue;
            };
            let translation = body.translation();
            if let Some(position) = world.positions.get_mut(id) {
                *position = Position {
                    x: f64::from(translation.x),
                    y: f64::from(translation.y),
                    z: f64::from(translation.z),
                };
            }
            // A fixed body never moves, so its authored rotation is left
            // exactly as it is rather than round-tripped through a
            // quaternion (which would only add float noise).
            if body.is_fixed() {
                continue;
            }
            // `body.rotation()` is a quaternion; its components are read
            // directly (no `Quat::to_euler`, whose `EulerRot` type doesn't
            // unify with this crate's own `glam` version, see the module
            // doc comment) and turned into engine angles by
            // `engine_rotation`.
            let q = body.rotation();
            let new_rotation = engine_rotation(q.x as f32, q.y as f32, q.z as f32, q.w as f32);
            if let Some(rotation) = world.rotations.get_mut(id) {
                if body.is_kinematic() {
                    // A character only ever turns around Y.
                    rotation.yaw = new_rotation.yaw;
                } else {
                    *rotation = new_rotation;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::world::{Gravity, Rotation as EngineRotation, Static, Velocity};

    fn quat_of(r: &Rotation) -> (f32, f32, f32, f32) {
        (r.x as f32, r.y as f32, r.z as f32, r.w as f32)
    }

    #[test]
    fn a_meshless_entity_gets_no_physics_body_until_it_has_a_mesh() {
        let mut world = World::new();
        let id = world.spawn(
            crate::world::Position {
                x: 0.0,
                y: 5.0,
                z: 0.0,
            },
            Velocity {
                dx: 0.0,
                dy: 0.0,
                dz: 0.0,
            },
        );
        world.rigid_bodies.insert(id, crate::world::RigidBody);
        world.gravities.insert(id, Gravity);
        world.meshes.insert(id, crate::world::Mesh::Empty);
        let mut physics = Physics::new(GRAVITY_Y);
        for _ in 0..30 {
            physics.step(&mut world, 1.0 / 30.0);
        }
        assert!(!physics.handles.contains_key(&id));
        assert_eq!(world.positions.get(id).unwrap().y, 5.0);
        world.meshes.insert(id, crate::world::Mesh::Cube);
        physics.step(&mut world, 1.0 / 30.0);
        assert!(physics.handles.contains_key(&id));
    }

    #[test]
    fn engine_and_rapier_orientations_round_trip() {
        for yaw in [-2.5f32, -0.4, 0.0, 1.1, 3.0] {
            for pitch in [-1.2f32, 0.0, 0.7] {
                for roll in [-1.5f32, 0.0, 2.0] {
                    let original = EngineRotation { yaw, pitch, roll };
                    let (x, y, z, w) = quat_of(&rapier_orientation(&original));
                    let back = engine_rotation(x, y, z, w);
                    for v in [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]] {
                        let a = original.apply(v);
                        let b = back.apply(v);
                        assert!(
                            a.iter().zip(b.iter()).all(|(p, q)| (p - q).abs() < 1e-4),
                            "{original:?} -> {back:?}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn yaw_only_matches_the_old_rapier_rotation() {
        let (x, y, z, w) = quat_of(&rapier_orientation(&EngineRotation::from_yaw(0.8)));
        let (ox, oy, oz, ow) = quat_of(&rapier_rotation(0.8));
        for (a, b) in [(x, ox), (y, oy), (z, oz), (w, ow)] {
            assert!((a - b).abs() < 1e-6);
        }
    }

    #[test]
    fn a_tilted_static_body_keeps_its_authored_rotation() {
        let mut world = World::new();
        let ramp = world.spawn(
            Position {
                x: 0.0,
                y: 0.0,
                z: 0.0,
            },
            Velocity {
                dx: 0.0,
                dy: 0.0,
                dz: 0.0,
            },
        );
        world.statics.insert(ramp, Static);
        world.rigid_bodies.insert(ramp, crate::world::RigidBody);
        let tilt = EngineRotation {
            yaw: 0.4,
            pitch: 0.3,
            roll: -0.2,
        };
        world.rotations.insert(ramp, tilt);
        let mut physics = Physics::new(GRAVITY_Y);
        for _ in 0..5 {
            physics.step(&mut world, 1.0 / 30.0);
        }
        assert_eq!(*world.rotations.get(ramp).unwrap(), tilt);
    }

    #[test]
    fn a_box_dropped_on_a_tilted_ramp_slides_instead_of_resting_flat() {
        let mut world = World::new();
        let ramp = world.spawn(
            Position {
                x: 0.0,
                y: 0.0,
                z: 0.0,
            },
            Velocity {
                dx: 0.0,
                dy: 0.0,
                dz: 0.0,
            },
        );
        world.statics.insert(ramp, Static);
        world.rigid_bodies.insert(ramp, crate::world::RigidBody);
        // A wide flat slab tipped 30 degrees about the forward axis.
        world.scales.insert(
            ramp,
            crate::world::Scale {
                x: 10.0,
                y: 0.5,
                z: 10.0,
            },
        );
        world.rotations.insert(
            ramp,
            EngineRotation {
                yaw: 0.0,
                pitch: 0.0,
                roll: 0.5,
            },
        );
        let ball = world.spawn(
            Position {
                x: 0.0,
                y: 12.0,
                z: 0.0,
            },
            Velocity {
                dx: 0.0,
                dy: 0.0,
                dz: 0.0,
            },
        );
        world.gravities.insert(ball, Gravity);
        world.rigid_bodies.insert(ball, crate::world::RigidBody);
        let mut physics = Physics::new(GRAVITY_Y);
        for _ in 0..90 {
            physics.step(&mut world, 1.0 / 30.0);
        }
        let p = world.positions.get(ball).unwrap();
        // It fell onto the slab and was carried sideways by the slope
        // (positive roll leans the right side down, so it slides to +X).
        assert!(p.y < 12.0, "it fell: {} {} {}", p.x, p.y, p.z);
        assert!(p.x.abs() > 1.0, "it slid sideways: {} {} {}", p.x, p.y, p.z);
    }

    fn floor_set(key: u64, origin: [f64; 3]) -> StaticMeshSet {
        // One big square at y = 0, as two triangles.
        let a = [-60.0, 0.0, -60.0];
        let b = [60.0, 0.0, -60.0];
        let c = [60.0, 0.0, 60.0];
        let d = [-60.0, 0.0, 60.0];
        StaticMeshSet {
            key,
            origin,
            meshes: std::sync::Arc::new(vec![StaticMesh {
                triangles: vec![a, d, c, a, c, b],
            }]),
        }
    }

    fn falling_box(world: &mut World, y: f32) -> usize {
        let id = world.spawn(
            Position {
                x: 0.0,
                y: f64::from(y),
                z: 0.0,
            },
            Velocity {
                dx: 0.0,
                dy: 0.0,
                dz: 0.0,
            },
        );
        world.gravities.insert(id, Gravity);
        world.rigid_bodies.insert(id, crate::world::RigidBody);
        id
    }

    #[test]
    fn a_box_falls_through_empty_space_but_rests_on_a_static_mesh() {
        let mut empty_world = World::new();
        let lost = falling_box(&mut empty_world, 12.0);
        let mut physics = Physics::new(GRAVITY_Y);
        for _ in 0..120 {
            physics.step(&mut empty_world, 1.0 / 30.0);
        }
        assert!(empty_world.positions.get(lost).unwrap().y < -20.0);

        let mut world = World::new();
        let id = falling_box(&mut world, 12.0);
        let mut physics = Physics::new(GRAVITY_Y);
        physics.sync_static_meshes(&[("ground".to_string(), floor_set(1, [0.0; 3]))]);
        for _ in 0..120 {
            physics.step(&mut world, 1.0 / 30.0);
        }
        let y = world.positions.get(id).unwrap().y;
        assert!(y > 1.0 && y < 12.0, "it should rest on the floor: {y}");
    }

    #[test]
    fn the_mesh_origin_moves_the_floor() {
        let mut world = World::new();
        let id = falling_box(&mut world, 62.0);
        let mut physics = Physics::new(GRAVITY_Y);
        physics.sync_static_meshes(&[("ground".to_string(), floor_set(1, [0.0, 50.0, 0.0]))]);
        for _ in 0..150 {
            physics.step(&mut world, 1.0 / 30.0);
        }
        let y = world.positions.get(id).unwrap().y;
        assert!(
            y > 51.0 && y < 62.0,
            "it should rest on the raised floor: {y}"
        );
    }

    #[test]
    fn a_character_stands_on_a_static_mesh() {
        let mut world = World::new();
        let id = falling_box(&mut world, 10.0);
        world.controlled.insert(id, crate::world::Controlled);
        let mut physics = Physics::new(GRAVITY_Y);
        physics.sync_static_meshes(&[("ground".to_string(), floor_set(1, [0.0; 3]))]);
        for _ in 0..150 {
            physics.step(&mut world, 1.0 / 30.0);
        }
        let y = world.positions.get(id).unwrap().y;
        assert!(y > 1.0 && y < 10.0, "the character should stand on it: {y}");
    }

    /// Drop a body 10 units above a floor at `origin` and let it settle;
    /// where it ends up, relative to `origin`.
    fn settle_at(origin: [f64; 3], character: bool) -> [f64; 3] {
        let mut world = World::new();
        let id = world.spawn(
            Position {
                // Off to one side of the floor's centre, by an amount that
                // is not a round number: with f32 positions out here that
                // offset is lost (the nearest representable value can be
                // tens of metres away at a billion units).
                x: origin[0] + 3.3,
                y: origin[1] + 10.0,
                z: origin[2] + 1.7,
            },
            Velocity {
                dx: 0.0,
                dy: 0.0,
                dz: 0.0,
            },
        );
        world.gravities.insert(id, Gravity);
        world.rigid_bodies.insert(id, crate::world::RigidBody);
        if character {
            world.controlled.insert(id, crate::world::Controlled);
        }
        let mut physics = Physics::new(GRAVITY_Y);
        physics.sync_static_meshes(&[("ground".to_string(), floor_set(1, origin))]);
        for _ in 0..150 {
            physics.step(&mut world, 1.0 / 30.0);
        }
        let p = world.positions.get(id).unwrap();
        [p.x - origin[0], p.y - origin[1], p.z - origin[2]]
    }

    #[test]
    fn physics_behaves_the_same_twenty_thousand_kilometres_from_the_origin() {
        // With f32 physics the floor and the body would each be rounded to
        // the nearest couple of metres out here, so nothing could rest on
        // anything. In f64 the result matches the one at the origin to
        // within a couple of centimetres (a billion units out, the solver's
        // own rounding is a few millimetres; f32 would be tens of metres off).
        for character in [false, true] {
            let near = settle_at([0.0; 3], character);
            for far in [
                [2.0e7, 0.0, 0.0],
                [-2.0e7, 5.0, 2.0e7],
                [1.0e9, 0.0, -1.0e9],
            ] {
                let got = settle_at(far, character);
                for axis in 0..3 {
                    assert!(
                        (got[axis] - near[axis]).abs() < 0.02,
                        "character={character} at {far:?}: {got:?} vs {near:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn a_body_far_from_the_origin_really_rests_on_the_floor() {
        // Not just "the same as near the origin": it has landed on the floor
        // (a half-extent above it), not fallen through or floated off.
        let rest = settle_at([2.0e7, 0.0, 2.0e7], false);
        assert!(rest[1] > 3.0 && rest[1] < 5.0, "{rest:?}");
        assert!(
            (rest[0] - 3.3).abs() < 0.5 && (rest[2] - 1.7).abs() < 0.5,
            "it stayed where it was dropped: {rest:?}"
        );
    }

    #[test]
    fn syncing_the_same_key_keeps_the_colliders_and_a_new_key_rebuilds() {
        let mut physics = Physics::new(GRAVITY_Y);
        assert_eq!(physics.static_mesh_collider_count(), 0);
        let sets = [("ground".to_string(), floor_set(1, [0.0; 3]))];
        physics.sync_static_meshes(&sets);
        assert_eq!(physics.static_mesh_collider_count(), 1);
        let before = physics.mesh_sets["ground"].1;
        physics.sync_static_meshes(&sets);
        assert_eq!(physics.mesh_sets["ground"].1, before, "same key: untouched");
        physics.sync_static_meshes(&[("ground".to_string(), floor_set(2, [0.0; 3]))]);
        assert_eq!(
            physics.static_mesh_collider_count(),
            1,
            "replaced, not added"
        );
        assert_eq!(physics.mesh_sets["ground"].0, 2);
    }

    #[test]
    fn a_set_that_is_no_longer_given_is_removed() {
        let mut physics = Physics::new(GRAVITY_Y);
        physics.sync_static_meshes(&[
            ("a".to_string(), floor_set(1, [0.0; 3])),
            ("b".to_string(), floor_set(1, [0.0; 3])),
        ]);
        assert_eq!(physics.static_mesh_collider_count(), 2);
        physics.sync_static_meshes(&[("a".to_string(), floor_set(1, [0.0; 3]))]);
        assert_eq!(physics.static_mesh_collider_count(), 1);
        physics.sync_static_meshes(&[]);
        assert_eq!(physics.static_mesh_collider_count(), 0);
    }

    #[test]
    fn empty_and_degenerate_meshes_are_skipped_without_failing() {
        let mut physics = Physics::new(GRAVITY_Y);
        let set = StaticMeshSet {
            key: 1,
            origin: [0.0; 3],
            meshes: std::sync::Arc::new(vec![
                StaticMesh { triangles: vec![] },
                StaticMesh {
                    triangles: vec![[0.0; 3], [0.0; 3]],
                },
                StaticMesh {
                    triangles: vec![[0.0; 3], [0.0; 3], [0.0; 3]],
                },
            ]),
        };
        physics.sync_static_meshes(&[("odd".to_string(), set)]);
        // Nothing usable, but the set is still tracked and nothing panicked.
        assert!(physics.static_mesh_collider_count() <= 1);
    }
}
