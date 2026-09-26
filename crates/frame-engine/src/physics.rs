//! rapier3d integration: the engine's physics-simulated entities.
//!
//! First real slice of the physics foundation named on the roadmap (see
//! DESIGN.md and Roadmap and Decisions), not the whole thing. Deliberately
//! narrow, the same "ship a real, named-limitation slice" shape Sound and
//! Light shipped in:
//!
//! - Only `Static` (-> a fixed rapier body) and `Gravity`-without-`Static`
//!   (-> a dynamic one) entities are supported. Plain `movement` (a
//!   `Velocity` with neither marker) has no rapier equivalent yet.
//! - A `RigidBody`-marked entity that is also `Controlled` is not yet
//!   supported and is silently skipped every tick (see `sync_new_and_removed`
//!   below) rather than given a body it can't drive correctly; deciding
//!   whether a player-controlled entity becomes `KinematicPositionBased` or
//!   something else is a real design call for later, not a default to guess
//!   at here.
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
//! `RigidBodyBuilder::rotation`'s own argument convention (axis-angle vector,
//! assumed here, versus Euler angles) is still unconfirmed by anything
//! stronger than the docs; an actual rotated fall, not just a clean build,
//! is the real check.

use rapier3d::prelude::*;

use crate::systems::half_extents;
use crate::world::{Position, World};

/// Frame Engine's physics state: a rapier3d `PhysicsWorld` plus the
/// entity-id <-> rapier-handle mapping the raw crate has no notion of. Not
/// named `PhysicsWorld` itself to avoid colliding with rapier's own type of
/// that name, which this wraps.
pub struct Physics {
    rapier: PhysicsWorld,
    handles: std::collections::BTreeMap<usize, (RigidBodyHandle, ColliderHandle)>,
}

impl Physics {
    /// `gravity_y` is a downward acceleration in world units per second
    /// squared, the real, continuously-integrated quantity rapier expects,
    /// not `world::GRAVITY` (a flat per-tick velocity nudge the hand-rolled
    /// path uses instead, and unaffected by this). -9.81 is a starting,
    /// tunable value in the same "pick something reasonable, feel it out"
    /// spirit as `world::GRAVITY`'s own doc comment; Frame Engine's world
    /// units aren't real metres, so there's no "correct" figure to match it
    /// against.
    pub fn new(gravity_y: f32) -> Self {
        let mut rapier = PhysicsWorld::new();
        rapier.gravity = Vector::new(0.0, gravity_y, 0.0);
        Physics {
            rapier,
            handles: std::collections::BTreeMap::new(),
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
            if world.controlled.get(id).is_some() {
                // Not yet supported, see the module doc comment. Left out of
                // `self.handles` on purpose: retried, harmlessly, every tick
                // rather than silently dropped for good, in case `Controlled`
                // is removed from this entity later.
                continue;
            }
            let Some(position) = world.positions.get(id).copied() else {
                continue;
            };
            let is_static = world.statics.get(id).is_some();
            if !is_static && world.gravities.get(id).is_none() {
                // Neither mapping in the module doc comment applies; nothing
                // safe to build yet. Same "retry every tick" reasoning as
                // the `Controlled` case above.
                continue;
            }

            let scale = world.scales.get(id).copied().unwrap_or_default();
            let mesh = world.meshes.get(id).cloned().unwrap_or_default();
            let [hx, hy, hz] = half_extents(&mesh, scale, &world.mesh_meta);
            let yaw = world.rotations.get(id).map(|r| r.yaw).unwrap_or(0.0);

            let builder = if is_static {
                RigidBodyBuilder::fixed()
            } else {
                RigidBodyBuilder::dynamic()
            };
            let body = builder
                .translation(Vector::new(position.x, position.y, position.z))
                // Axis-angle rotation vector (direction = axis, length =
                // angle), rapier's historical nalgebra-era convention for
                // this method; see the module doc comment's one open item.
                .rotation(Vector::new(0.0, yaw, 0.0))
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
        }

        let gone: Vec<usize> = self
            .handles
            .keys()
            .copied()
            .filter(|id| world.rigid_bodies.get(*id).is_none())
            .collect();
        for id in gone {
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
            let yaw = 2.0 * rotation_quat.y.atan2(rotation_quat.w);
            if let Some(rotation) = world.rotations.get_mut(id) {
                rotation.yaw = yaw;
            }
        }
    }
}
