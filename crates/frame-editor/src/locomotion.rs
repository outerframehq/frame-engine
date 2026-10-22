// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Characters pick their clips from how they move.
//!
//! A character (an entity with a `CharacterBody`) whose model has any of the
//! clips named in `CLIPS` gets its `Animation` set every tick from what its
//! body is doing: standing, walking, running, crouched, jumping or falling.
//! A model with none of them is left alone, so a clip chosen in the
//! Inspector keeps playing.
//!
//! - **Speed** is measured from how far the character really moved since
//!   the last tick, smoothed, so it works the same for a character this
//!   computer moves and for one that arrives over the network.
//! - **Walking and running clips play at the pace the character moves**:
//!   a walk clip is made at the body's walking speed, so moving at that speed
//!   plays it as made and moving faster plays it faster, and the feet keep
//!   to the ground instead of sliding. Clips stay in place: moving the
//!   character is the controller's job.
//! - **Moving backwards plays the clip backwards**: a character faces where
//!   it looks, so one stepping back plays its walk in reverse rather than
//!   striding forward while it slides the other way.
//! - **A missing clip falls back** to the nearest one the model has: no
//!   `run` plays `walk` faster, no `crouch_walk` plays `walk`, no `jump`
//!   plays `fall`, and anything else falls back to `idle`.
//! - **Changes fade** over `FADE` seconds: the new clip's weight rises as
//!   the old ones' fall, and a clip faded out is removed.

use frame_engine::assets::MeshData;
use frame_engine::world::{Animation, Mesh, PlayingClip, World};
use std::collections::{BTreeMap, HashMap};

/// The clips a character's model may have, by name.
pub const CLIPS: [&str; 7] = [
    "idle",
    "walk",
    "run",
    "jump",
    "fall",
    "crouch_idle",
    "crouch_walk",
];

/// Seconds a change of clip takes to fade in.
pub const FADE: f32 = 0.2;
/// Slower than this (m/s) counts as standing still.
const MOVING: f32 = 0.15;
/// Rising faster than this (m/s) in the air is a jump, not a fall.
const RISING: f32 = 0.5;
/// How quickly the measured speed follows the real one (per second).
const SMOOTHING: f32 = 10.0;
/// A move further than this in one tick is a teleport, not running.
const TELEPORT: f64 = 20.0;
/// The slowest and fastest a walking or running clip is played.
const PACE: (f32, f32) = (0.25, 3.0);
/// How far against the way it faces a character must move (the cosine of
/// the angle off straight back, 1 being straight back) for its clip to play
/// backwards. Sideways keeps it forwards.
const BACKWARDS: f32 = 0.3;

/// Where each character was last tick and how fast it was going.
#[derive(Default)]
pub struct Locomotion {
    last: HashMap<usize, Seen>,
}

#[derive(Clone, Copy)]
struct Seen {
    position: [f64; 3],
    speed: f32,
    /// Whether it was last moving against the way it faces.
    backwards: bool,
}

/// The clip to play when `want` is missing, in order: the first the model
/// has.
fn fallbacks(want: &str) -> &'static [&'static str] {
    match want {
        "run" => &["run", "walk", "idle"],
        "walk" => &["walk", "run", "idle"],
        "crouch_walk" => &["crouch_walk", "walk", "crouch_idle", "idle"],
        "crouch_idle" => &["crouch_idle", "idle"],
        "jump" => &["jump", "fall", "idle"],
        "fall" => &["fall", "jump", "idle"],
        _ => &["idle"],
    }
}

impl Locomotion {
    /// Set every character's clips from how it moved over the last `dt`
    /// seconds. `models` are the loaded models by name.
    pub fn tick(&mut self, world: &mut World, models: &BTreeMap<String, MeshData>, dt: f32) {
        if !(dt > 0.0) {
            return;
        }
        let ids: Vec<usize> = world.characters.entities().collect();
        self.last.retain(|id, _| ids.contains(id));
        for id in ids {
            let Some(Mesh::Custom(name)) = world.meshes.get(id) else {
                continue;
            };
            let Some(skin) = models.get(name).and_then(|m| m.skin.as_ref()) else {
                continue;
            };
            if !skin.clips.iter().any(|c| CLIPS.contains(&c.name.as_str())) {
                continue;
            }
            let Some(p) = world.positions.get(id).map(|p| [p.x, p.y, p.z]) else {
                continue;
            };
            let seen = self.last.get(&id).copied().unwrap_or(Seen {
                position: p,
                speed: 0.0,
                backwards: false,
            });
            let d = [0, 1, 2].map(|i| p[i] - seen.position[i]);
            let across = (d[0] * d[0] + d[2] * d[2]).sqrt();
            let (across, up) = if across > TELEPORT || d[1].abs() > TELEPORT {
                (0.0, 0.0)
            } else {
                (across as f32 / dt, d[1] as f32 / dt)
            };
            let speed = seen.speed + (across - seen.speed) * (dt * SMOOTHING).min(1.0);
            // Forward is -Z turned by the yaw: (sin yaw, -cos yaw) on x, z.
            let yaw = world
                .looks
                .get(id)
                .map(|l| l.yaw)
                .or_else(|| world.rotations.get(id).map(|r| r.yaw))
                .unwrap_or(0.0);
            let length = (d[0] * d[0] + d[2] * d[2]).sqrt();
            let backwards = if across > MOVING && length > 0.0 {
                let ahead = (f64::from(yaw.sin()) * d[0] - f64::from(yaw.cos()) * d[2]) / length;
                ahead < -f64::from(BACKWARDS)
            } else {
                seen.backwards
            };
            self.last.insert(
                id,
                Seen {
                    position: p,
                    speed,
                    backwards,
                },
            );

            let body = world
                .characters
                .get(id)
                .copied()
                .unwrap_or_default()
                .sanitized();
            // A character this computer does not move (one arriving over
            // the network) has no word from physics: standing still up and
            // down counts as on the ground.
            let grounded = world
                .grounded
                .get(&id)
                .copied()
                .unwrap_or(up.abs() < RISING);
            let crouched = world.crouched.get(&id).copied().unwrap_or(false);
            let moving = speed > MOVING;
            let want = if !grounded && !world.swimming.contains(&id) {
                if up > RISING { "jump" } else { "fall" }
            } else if crouched {
                if moving { "crouch_walk" } else { "crouch_idle" }
            } else if !moving {
                "idle"
            } else if speed > (body.walk_speed + body.run_speed) * 0.5 {
                "run"
            } else {
                "walk"
            };
            let Some(clip) = fallbacks(want).iter().find_map(|name| skin.clip(name)) else {
                continue;
            };
            // Walking clips at the body's own pace.
            let direction = if backwards { -1.0 } else { 1.0 };
            let pace = |made_at: f32| direction * (speed / made_at.max(0.1)).clamp(PACE.0, PACE.1);
            let playback = match clip.name.as_str() {
                "walk" => pace(body.walk_speed),
                "run" => pace(body.run_speed),
                "crouch_walk" => pace(body.crouch_speed),
                _ => 1.0,
            };
            let looping = clip.name != "jump";
            let (target, length) = (clip.name.clone(), clip.duration);
            let animation = match world.animations.get_mut(id) {
                Some(a) => a,
                None => {
                    world.animations.insert(id, Animation::default());
                    world.animations.get_mut(id).expect("just inserted")
                }
            };
            fade_to(animation, &target, length, playback, looping, dt);
        }
    }
}

/// Fade `animation` towards clip `target` over `FADE` seconds: its weight
/// rises (added at 0 if it is not playing) while every other clip's falls,
/// and a clip that reaches 0 is removed.
fn fade_to(
    animation: &mut Animation,
    target: &str,
    length: f32,
    speed: f32,
    looping: bool,
    dt: f32,
) {
    let step = dt / FADE;
    if !animation.clips.iter().any(|c| c.clip == target) {
        animation.clips.push(PlayingClip {
            clip: target.to_string(),
            length,
            weight: if animation.clips.is_empty() { 1.0 } else { 0.0 },
            speed,
            looping,
            ..PlayingClip::default()
        });
    }
    for clip in &mut animation.clips {
        if clip.clip == target {
            clip.weight = (clip.weight + step).min(1.0);
            clip.speed = speed;
            clip.looping = looping;
            clip.length = length;
        } else {
            clip.weight -= step;
        }
    }
    animation
        .clips
        .retain(|c| c.clip == target || c.weight > 0.0);
}

#[cfg(test)]
mod tests {
    use super::*;
    use frame_engine::assets::MeshVertexData;
    use frame_engine::assets::skin::{Clip, Skin};
    use frame_engine::world::{CharacterBody, Position, Velocity};

    const DT: f32 = 1.0 / 30.0;

    /// A model with `idle`, `walk` and `jump` (no `run`).
    fn models() -> BTreeMap<String, MeshData> {
        let clip = |name: &str| Clip {
            name: name.to_string(),
            duration: 1.0,
            tracks: Vec::new(),
        };
        let data = MeshData {
            vertices: vec![MeshVertexData {
                position: [0.0; 3],
                normal: [0.0, 1.0, 0.0],
                uv: [0.0; 2],
            }],
            half_extents: [0.5; 3],
            shapes: Vec::new(),
            skin: Some(Skin {
                bones: Vec::new(),
                vertices: Vec::new(),
                clips: vec![clip("idle"), clip("walk"), clip("jump")],
            }),
        };
        [("person".to_string(), data)].into()
    }

    fn character(world: &mut World) -> usize {
        let id = world.spawn(
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
        world.meshes.insert(id, Mesh::Custom("person".to_string()));
        world.characters.insert(id, CharacterBody::default());
        world.grounded.insert(id, true);
        id
    }

    /// Move `id` by `step` each tick for `ticks` ticks.
    fn run(world: &mut World, l: &mut Locomotion, id: usize, step: [f64; 3], ticks: usize) {
        let models = models();
        for _ in 0..ticks {
            let p = world.positions.get_mut(id).unwrap();
            p.x += step[0];
            p.y += step[1];
            p.z += step[2];
            l.tick(world, &models, DT);
        }
    }

    /// The clips playing on `id` that count, with their weights rounded to
    /// thousandths.
    fn playing(world: &World, id: usize) -> Vec<(String, f32)> {
        world
            .animations
            .get(id)
            .map(|a| {
                a.clips
                    .iter()
                    .filter(|c| c.weight > 1e-3)
                    .map(|c| (c.clip.clone(), (c.weight * 1000.0).round() / 1000.0))
                    .collect()
            })
            .unwrap_or_default()
    }

    #[test]
    fn standing_idles_and_walking_plays_walk_at_the_bodys_pace() {
        let mut world = World::new();
        let mut l = Locomotion::default();
        let id = character(&mut world);
        run(&mut world, &mut l, id, [0.0; 3], 3);
        assert_eq!(
            playing(&world, id),
            [("idle".to_string(), 1.0)],
            "straight in"
        );
        // At the body's walking speed: walk, played as made.
        let walk = f64::from(CharacterBody::default().walk_speed / 30.0);
        run(&mut world, &mut l, id, [walk, 0.0, 0.0], 30);
        let a = world.animations.get(id).unwrap();
        assert_eq!(a.clips.len(), 1, "idle faded out and gone");
        assert_eq!(a.clips[0].clip, "walk");
        assert!(
            (a.clips[0].speed - 1.0).abs() < 0.02,
            "{}",
            a.clips[0].speed
        );
        // Running speed, and no run clip: walk, faster.
        let fast = f64::from(CharacterBody::default().run_speed / 30.0);
        run(&mut world, &mut l, id, [0.0, 0.0, -fast], 30);
        let a = world.animations.get(id).unwrap();
        assert_eq!(a.clips[0].clip, "walk");
        assert!(a.clips[0].speed > 2.0, "{}", a.clips[0].speed);
        // Facing -Z (yaw 0) and stepping back towards +Z: backwards.
        run(&mut world, &mut l, id, [0.0, 0.0, walk], 30);
        let a = world.animations.get(id).unwrap();
        assert!(
            (a.clips[0].speed + 1.0).abs() < 0.02,
            "{}",
            a.clips[0].speed
        );
    }

    #[test]
    fn a_change_of_clip_fades_over_a_fifth_of_a_second() {
        let mut world = World::new();
        let mut l = Locomotion::default();
        let id = character(&mut world);
        run(&mut world, &mut l, id, [0.0; 3], 2);
        // Off the ground and rising: jump fades in as idle fades out.
        world.grounded.insert(id, false);
        run(&mut world, &mut l, id, [0.0, 0.05, 0.0], 3);
        let now = playing(&world, id);
        assert_eq!(now, [("idle".to_string(), 0.5), ("jump".to_string(), 0.5)]);
        run(&mut world, &mut l, id, [0.0, 0.05, 0.0], 3);
        assert_eq!(playing(&world, id), [("jump".to_string(), 1.0)]);
        let jump = &world.animations.get(id).unwrap().clips[0];
        assert!(!jump.looping, "a jump plays once");
        // Falling with no fall clip: jump stays, the nearest it has.
        run(&mut world, &mut l, id, [0.0, -0.05, 0.0], 3);
        assert_eq!(playing(&world, id)[0].0, "jump");
    }

    #[test]
    fn a_model_without_these_clips_and_a_teleport_are_left_alone() {
        let mut world = World::new();
        let mut l = Locomotion::default();
        let id = character(&mut world);
        world.meshes.insert(id, Mesh::Custom("rock".to_string()));
        run(&mut world, &mut l, id, [1.0, 0.0, 0.0], 5);
        assert!(world.animations.get(id).is_none(), "not a model with clips");
        world.meshes.insert(id, Mesh::Custom("person".to_string()));
        run(&mut world, &mut l, id, [0.0; 3], 1);
        run(&mut world, &mut l, id, [500.0, 0.0, 0.0], 1);
        assert_eq!(
            playing(&world, id)[0].0,
            "idle",
            "a teleport is not running"
        );
        // A character that despawns is forgotten.
        world.despawn(id);
        l.tick(&mut world, &models(), DT);
        assert!(l.last.is_empty());
    }
}
