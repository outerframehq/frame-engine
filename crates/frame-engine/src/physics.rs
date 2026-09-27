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
//! - Rotation is locked to the world's own Y axis only (`enabled_rotations`),
//!   matching the engine's yaw-only `Rotation` component: a rapier body never
//!   tumbles on an axis Frame Engine has no field to read it back from.
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
    Rotation::from_scaled_axis(Vector::new(0.0, -yaw, 0.0))
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
        rapier.gravity = Vector::new(0.0, gravity_y, 0.0);
        Physics {
            rapier,
            handles: std::collections::BTreeMap::new(),
            gravity_y,
            controller: KinematicCharacterController {
                offset: CharacterLength::Absolute(0.05),
                ..KinematicCharacterController::default()
            },
            characters: std::collections::BTreeMap::new(),
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
        self.rapier.integration_parameters.dt = dt;
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
            let [hx, hy, hz] = half_extents(&mesh, scale, &world.mesh_meta);
            let yaw = world.rotations.get(id).map(|r| r.yaw).unwrap_or(0.0);

            let builder = if is_controlled {
                // Moved by `move_characters`, not by forces.
                RigidBodyBuilder::kinematic_position_based()
            } else if is_static {
                RigidBodyBuilder::fixed()
            } else {
                RigidBodyBuilder::dynamic()
            };
            let body = builder
                .translation(Vector::new(position.x, position.y, position.z))
                .rotation(rapier_rotation(yaw).to_scaled_axis())
                // Yaw-only, matching `world::Rotation`.
                .enabled_rotations(false, true, false)
                .build();
            let body_handle = self.rapier.bodies.insert(body);
            let collider = ColliderBuilder::cuboid(hx, hy, hz).build();
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
            let desired = Vector::new(intent.dx, character.fall_speed * dt, intent.dz);

            // Leave the character's own collider out of the obstacles it
            // checks against, or it would collide with itself.
            let filter = QueryFilter::default().exclude_rigid_body(body_handle);
            let queries = self.rapier.query_pipeline_with_filter(filter);
            let movement = self.controller.move_shape(
                dt,
                &queries,
                collider.shape(),
                collider.position(),
                desired,
                |_| {},
            );

            character.grounded = movement.grounded;
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
                    x: translation.x,
                    y: translation.y,
                    z: translation.z,
                };
            }
            // `body.rotation()` is a quaternion (rapier3d's public API has
            // been glam-based since 0.32; see the module doc comment).
            // Read yaw back directly from its components rather than
            // through `Quat::to_euler`: that needs a `glam::EulerRot`
            // value, and the `glam` this crate depends on directly turned
            // out not to be the same version rapier vendors internally
            // through `glamx`, two nominally-identical but distinct types
            // the compiler correctly refused to mix. Field access has no
            // such problem, and `enabled_rotations(false, true, false)`
            // above guarantees this is always a pure Y-axis rotation, so
            // (w, x, y, z) = (cos(yaw/2), 0, sin(yaw/2), 0) and
            // yaw = 2 * atan2(y, w) recovers it exactly.
            let rotation_quat = body.rotation();
            // Negated back into the engine's clockwise convention; see
            // `rapier_rotation`.
            let yaw = -2.0 * rotation_quat.y.atan2(rotation_quat.w);
            if let Some(rotation) = world.rotations.get_mut(id) {
                rotation.yaw = yaw;
            }
        }
    }
}
