// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Sound that modules ask for while a game plays (see
//! `EditorModule::audio`). Once a frame the Play window hands every module
//! a `Listener` and an `AudioFrame`; a module says what should be heard by
//! filling the frame, and `GameAudio` turns that into playing sounds:
//!
//! - Loops (`AudioFrame::hold_loop`): wind, rain, a river. A loop keeps
//!   playing while some module holds it, fades to each new volume it is
//!   held at, and fades out and stops once no module holds it any more.
//! - One-shots (`AudioFrame::play`, `AudioFrame::play_at`): a footstep, a
//!   spade. One placed in the world is quieter the further it is from the
//!   listener and leans to the side it is on.
//!
//! Sounds are found by file name under the project's `assets/`, the same
//! as the `Sound` component's, so they play the same from a folder or a
//! game build's pack.

use std::collections::HashMap;
use std::time::Duration;

use frame_engine::pack::ProjectFiles;
use kira::sound::static_sound::{StaticSoundData, StaticSoundHandle};
use kira::backend::Backend;
use kira::{AudioManager, Decibels, Panning, PlaybackRate, Tween};

/// Who hears the game: the camera Play draws from.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Listener {
    /// Where the listener is, in the world.
    pub position: [f64; 3],
    /// The listener's right, a unit vector: a sound on this side plays
    /// more from the right speaker.
    pub right: [f32; 3],
    /// Seconds since the last frame's sound, for anything that needs a
    /// speed (0 on the first frame).
    pub dt: f32,
    /// Whether the game's menu is showing.
    pub in_menu: bool,
}

/// One loop a module holds this frame.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct LoopRequest {
    pub key: String,
    pub sound: String,
    pub volume: f32,
}

/// One sound to play once, now.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ShotRequest {
    pub sound: String,
    pub volume: f32,
    pub pitch: f32,
    pub at: Option<[f64; 3]>,
}

/// What modules want heard this frame. Volumes are 0.0 (silent) to 1.0
/// (as recorded); the player's volume setting applies on top.
#[derive(Debug, Default)]
pub struct AudioFrame {
    pub(crate) loops: Vec<LoopRequest>,
    pub(crate) shots: Vec<ShotRequest>,
}

impl AudioFrame {
    /// Keep the looping sound `sound` (a file name under `assets/`) playing
    /// at `volume`, under `key`. Ask again every frame to keep it going: a
    /// key nobody holds fades out. Holding a key with another sound swaps
    /// to it. A volume of 0 holds the loop silently, so it picks up where
    /// it was when it comes back.
    pub fn hold_loop(&mut self, key: &str, sound: &str, volume: f32) {
        self.loops.push(LoopRequest {
            key: key.to_string(),
            sound: sound.to_string(),
            volume: clean(volume),
        });
    }

    /// Play `sound` once, as heard from where the listener is (a menu
    /// click, say). `pitch` 1.0 plays it as recorded; 1.1 a little higher
    /// and quicker.
    pub fn play(&mut self, sound: &str, volume: f32, pitch: f32) {
        self.shots.push(ShotRequest {
            sound: sound.to_string(),
            volume: clean(volume),
            pitch: clean_pitch(pitch),
            at: None,
        });
    }

    /// Play `sound` once from `at` in the world: quieter further from the
    /// listener (silent past `HEARING`), and from the side it is on.
    pub fn play_at(&mut self, sound: &str, volume: f32, pitch: f32, at: [f64; 3]) {
        if at.iter().all(|c| c.is_finite()) {
            self.shots.push(ShotRequest {
                sound: sound.to_string(),
                volume: clean(volume),
                pitch: clean_pitch(pitch),
                at: Some(at),
            });
        }
    }
}

impl AudioFrame {
    /// The sounds asked to play once this frame, by name: for a module's
    /// own tests.
    pub fn one_shots(&self) -> Vec<&str> {
        self.shots.iter().map(|s| s.sound.as_str()).collect()
    }

    /// The volume the loop `key` is held at this frame, if any module holds
    /// it: for a module's own tests.
    pub fn held(&self, key: &str) -> Option<f32> {
        self.loops.iter().find(|l| l.key == key).map(|l| l.volume)
    }
}

fn clean(volume: f32) -> f32 {
    if volume.is_finite() {
        volume.clamp(0.0, 1.0)
    } else {
        0.0
    }
}

fn clean_pitch(pitch: f32) -> f32 {
    if pitch.is_finite() {
        pitch.clamp(0.25, 4.0)
    } else {
        1.0
    }
}

/// Within this distance (m) a placed sound plays at full volume.
const NEAR: f64 = 2.0;
/// Past this distance (m) a placed sound isn't played at all.
pub const HEARING: f64 = 60.0;
/// How far a sound straight to one side leans to that speaker (1 would be
/// that speaker only, which sounds odd on headphones).
const PAN_SIDE: f32 = 0.7;
/// The quietest volume (dB) a sound is played at: anything quieter is
/// silent.
const SILENT_DB: f32 = -60.0;
/// How long (s) a new loop fades in, and a dropped one fades out.
const LOOP_FADE: f64 = 2.0;
/// How long (s) a loop takes to reach a new volume.
const LOOP_GLIDE: f64 = 0.6;

/// How loud (0 to 1) and how far to one side (-1 left to 1 right) a sound
/// at `at` is for `listener`. Full volume within `NEAR`, then falling with
/// distance (halving as the distance doubles), and 0 past `HEARING`.
pub(crate) fn placed(listener: &Listener, at: [f64; 3]) -> (f32, f32) {
    let d = [
        at[0] - listener.position[0],
        at[1] - listener.position[1],
        at[2] - listener.position[2],
    ];
    let dist = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt();
    if !dist.is_finite() || dist > HEARING {
        return (0.0, 0.0);
    }
    // Fades to nothing over the last fifth of the hearing range, so a sound
    // walking away doesn't stop with a click.
    let edge = ((HEARING - dist) / (0.2 * HEARING)).clamp(0.0, 1.0);
    let gain = (NEAR / dist.max(NEAR)) * edge;
    let pan = if dist < 1e-6 {
        0.0
    } else {
        let r = listener.right;
        ((d[0] * r[0] as f64 + d[1] * r[1] as f64 + d[2] * r[2] as f64) / dist) as f32 * PAN_SIDE
    };
    (gain as f32, pan.clamp(-1.0, 1.0))
}

/// A volume 0 to 1 as decibels, `SILENT_DB` for silence.
pub(crate) fn decibels(volume: f32) -> f32 {
    if !volume.is_finite() || volume <= 0.001 {
        return SILENT_DB;
    }
    (20.0 * volume.min(1.0).log10()).max(SILENT_DB)
}

fn tween(seconds: f64) -> Tween {
    Tween {
        duration: Duration::from_secs_f64(seconds),
        ..Tween::default()
    }
}

/// A loop that is playing.
struct Playing {
    sound: String,
    handle: StaticSoundHandle,
    volume: f32,
}

/// The loops a game's modules hold, and the sounds already decoded.
#[derive(Default)]
pub(crate) struct GameAudio {
    loops: HashMap<String, Playing>,
    /// Sounds that weren't found or didn't decode, so they are reported
    /// once rather than every frame.
    missing: std::collections::HashSet<String>,
}

impl GameAudio {
    /// Play what `frame` asks for. Returns lines for the log (a sound that
    /// couldn't be found or played), each sound reported only once.
    pub(crate) fn apply<B: Backend>(
        &mut self,
        frame: AudioFrame,
        listener: &Listener,
        manager: Option<&mut AudioManager<B>>,
        cache: &mut HashMap<String, StaticSoundData>,
        files: Option<&ProjectFiles>,
    ) -> Vec<String> {
        let mut messages = Vec::new();
        let Some(manager) = manager else {
            return messages;
        };
        // Loops: the first request for a key wins if two modules ask.
        let mut wanted: HashMap<String, LoopRequest> = HashMap::new();
        for request in frame.loops {
            wanted.entry(request.key.clone()).or_insert(request);
        }
        // Drop loops nobody holds, and those switching to another sound.
        let gone: Vec<String> = self
            .loops
            .iter()
            .filter(|(key, playing)| wanted.get(*key).is_none_or(|w| w.sound != playing.sound))
            .map(|(key, _)| key.clone())
            .collect();
        for key in gone {
            if let Some(mut playing) = self.loops.remove(&key) {
                playing.handle.stop(tween(LOOP_FADE));
            }
        }
        for (key, request) in wanted {
            match self.loops.get_mut(&key) {
                Some(playing) => {
                    if (playing.volume - request.volume).abs() > 0.005 {
                        playing
                            .handle
                            .set_volume(Decibels(decibels(request.volume)), tween(LOOP_GLIDE));
                        playing.volume = request.volume;
                    }
                }
                None => {
                    let Some(data) = self.load(&request.sound, cache, files, &mut messages) else {
                        continue;
                    };
                    // Starts silent and fades up, so a loop never starts
                    // with a jump.
                    let data = data.loop_region(..).volume(Decibels(SILENT_DB));
                    match manager.play(data) {
                        Ok(mut handle) => {
                            handle.set_volume(Decibels(decibels(request.volume)), tween(LOOP_FADE));
                            self.loops.insert(
                                key,
                                Playing {
                                    sound: request.sound,
                                    handle,
                                    volume: request.volume,
                                },
                            );
                        }
                        Err(e) => {
                            if self.missing.insert(request.sound.clone()) {
                                messages.push(format!("Couldn't play sound '{}': {e:?}", request.sound));
                            }
                        }
                    }
                }
            }
        }
        for shot in frame.shots {
            let (gain, pan) = match shot.at {
                Some(at) => placed(listener, at),
                None => (1.0, 0.0),
            };
            let volume = shot.volume * gain;
            if volume <= 0.001 {
                continue;
            }
            let Some(data) = self.load(&shot.sound, cache, files, &mut messages) else {
                continue;
            };
            let data = data
                .volume(Decibels(decibels(volume)))
                .playback_rate(PlaybackRate(f64::from(shot.pitch)))
                .panning(Panning(pan));
            if let Err(e) = manager.play(data)
                && self.missing.insert(shot.sound.clone())
            {
                messages.push(format!("Couldn't play sound '{}': {e:?}", shot.sound));
            }
        }
        messages
    }

    /// The decoded sound `name`, from the cache or else from the project.
    fn load(
        &mut self,
        name: &str,
        cache: &mut HashMap<String, StaticSoundData>,
        files: Option<&ProjectFiles>,
        messages: &mut Vec<String>,
    ) -> Option<StaticSoundData> {
        if let Some(data) = cache.get(name) {
            return Some(data.clone());
        }
        if self.missing.contains(name) {
            return None;
        }
        let found = files.and_then(|files| {
            let path = files.find_by_name("assets", name)?;
            files.read(&path).ok()
        });
        let decoded = found.and_then(|bytes| {
            StaticSoundData::from_cursor(std::io::Cursor::new(bytes)).ok()
        });
        match decoded {
            Some(data) => {
                cache.insert(name.to_string(), data.clone());
                Some(data)
            }
            None => {
                self.missing.insert(name.to_string());
                messages.push(format!(
                    "Couldn't find or read sound '{name}' under assets/"
                ));
                None
            }
        }
    }

    /// Stop every loop at once (the game closed).
    pub(crate) fn stop_all(&mut self) {
        for (_, mut playing) in self.loops.drain() {
            playing.handle.stop(tween(0.05));
        }
    }

    /// The keys of the loops playing now, for tests.
    #[cfg(test)]
    fn keys(&self) -> Vec<String> {
        let mut keys: Vec<String> = self.loops.keys().cloned().collect();
        keys.sort();
        keys
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn listener() -> Listener {
        Listener {
            position: [0.0, 0.0, 0.0],
            right: [1.0, 0.0, 0.0],
            dt: 0.0,
            in_menu: false,
        }
    }

    #[test]
    fn placed_sounds_fade_with_distance_and_lean_to_their_side() {
        let l = listener();
        assert_eq!(placed(&l, [0.0, 0.0, -1.0]), (1.0, 0.0));
        let (near, _) = placed(&l, [0.0, 0.0, -4.0]);
        let (far, _) = placed(&l, [0.0, 0.0, -8.0]);
        assert!((near - 0.5).abs() < 1e-6 && (far - 0.25).abs() < 1e-6, "{near} {far}");
        assert_eq!(placed(&l, [0.0, 0.0, -61.0]).0, 0.0);
        // Faded to nothing at the edge, not cut off.
        assert!(placed(&l, [0.0, 0.0, -59.9]).0 < 0.01);
        let (_, right) = placed(&l, [10.0, 0.0, 0.0]);
        let (_, left) = placed(&l, [-10.0, 0.0, 0.0]);
        assert!((right - PAN_SIDE).abs() < 1e-6 && (left + PAN_SIDE).abs() < 1e-6);
        assert_eq!(placed(&l, [f64::NAN, 0.0, 0.0]), (0.0, 0.0));
    }

    #[test]
    fn volumes_become_decibels_and_bad_values_are_cleaned() {
        assert_eq!(decibels(1.0), 0.0);
        assert!((decibels(0.5) + 6.02).abs() < 0.01);
        assert_eq!(decibels(0.0), SILENT_DB);
        assert_eq!(decibels(f32::NAN), SILENT_DB);
        let mut frame = AudioFrame::default();
        frame.hold_loop("wind", "amb_wind_light.wav", 3.0);
        frame.play("ui_click.wav", f32::NAN, f32::INFINITY);
        frame.play_at("step.wav", 1.0, 1.0, [f64::NAN, 0.0, 0.0]);
        assert_eq!(frame.loops[0].volume, 1.0);
        assert_eq!((frame.shots[0].volume, frame.shots[0].pitch), (0.0, 1.0));
        assert_eq!(frame.shots.len(), 1, "a sound at no real place is dropped");
    }

    #[test]
    fn without_an_audio_device_nothing_plays_and_nothing_breaks() {
        let mut audio = GameAudio::default();
        let mut frame = AudioFrame::default();
        frame.hold_loop("wind", "amb_wind_light.wav", 0.5);
        let none: Option<&mut AudioManager<kira::DefaultBackend>> = None;
        let messages = audio.apply(frame, &listener(), none, &mut HashMap::new(), None);
        assert!(messages.is_empty());
        assert!(audio.keys().is_empty());
    }

    /// A project folder holding one short sound at `assets/audio/<name>`.
    fn project_with(name: &str) -> (std::path::PathBuf, ProjectFiles) {
        let dir = std::env::temp_dir().join(format!("frame-game-audio-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("assets/audio")).unwrap();
        // A tenth of a second of a quiet tone, as a 16-bit mono WAV.
        let rate = 48_000u32;
        let samples: Vec<i16> = (0..rate / 10)
            .map(|i| ((i as f32 * 0.05).sin() * 3000.0) as i16)
            .collect();
        let data_len = (samples.len() * 2) as u32;
        let mut wav = Vec::new();
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&(36 + data_len).to_le_bytes());
        wav.extend_from_slice(b"WAVEfmt ");
        wav.extend_from_slice(&16u32.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes());
        wav.extend_from_slice(&rate.to_le_bytes());
        wav.extend_from_slice(&(rate * 2).to_le_bytes());
        wav.extend_from_slice(&2u16.to_le_bytes());
        wav.extend_from_slice(&16u16.to_le_bytes());
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&data_len.to_le_bytes());
        for s in samples {
            wav.extend_from_slice(&s.to_le_bytes());
        }
        std::fs::write(dir.join("assets/audio").join(name), wav).unwrap();
        (dir.clone(), ProjectFiles::Folder(dir))
    }

    #[test]
    fn loops_start_keep_going_and_stop_and_missing_sounds_are_told_once() {
        use kira::backend::mock::{MockBackend, MockBackendSettings};
        let mut manager = AudioManager::<MockBackend>::new(kira::AudioManagerSettings {
            backend_settings: MockBackendSettings { sample_rate: 48_000 },
            ..Default::default()
        })
        .unwrap();
        let (dir, files) = project_with("wind.wav");
        let mut cache = HashMap::new();
        let mut audio = GameAudio::default();
        let mut frame = AudioFrame::default();
        frame.hold_loop("wind", "wind.wav", 0.5);
        frame.play_at("wind.wav", 1.0, 1.0, [3.0, 0.0, 0.0]);
        let messages = audio.apply(frame, &listener(), Some(&mut manager), &mut cache, Some(&files));
        assert!(messages.is_empty(), "{messages:?}");
        assert_eq!(audio.keys(), vec!["wind".to_string()]);
        // Held again at another volume: still the one loop.
        let mut frame = AudioFrame::default();
        frame.hold_loop("wind", "wind.wav", 0.9);
        audio.apply(frame, &listener(), Some(&mut manager), &mut cache, Some(&files));
        assert_eq!(audio.keys(), vec!["wind".to_string()]);
        // Not held: gone.
        audio.apply(AudioFrame::default(), &listener(), Some(&mut manager), &mut cache, Some(&files));
        assert!(audio.keys().is_empty());
        // A sound that isn't there is reported once, however often asked for.
        let mut told = Vec::new();
        for _ in 0..3 {
            let mut frame = AudioFrame::default();
            frame.play("nothing.wav", 1.0, 1.0);
            told.extend(audio.apply(frame, &listener(), Some(&mut manager), &mut cache, Some(&files)));
        }
        assert_eq!(told.len(), 1, "{told:?}");
        let _ = std::fs::remove_dir_all(dir);
    }
}
