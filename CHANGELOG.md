# Changelog

All notable changes to Frame Engine are recorded here.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project aims to follow [Semantic Versioning](https://semver.org/spec/v2.0.0.html)
once it reaches 1.0. Until then, the minor version bumps when a real new capability
lands and the patch version bumps for fixes and small additions.

## [Unreleased]

### Added

Engine:

- A `Color` component, kept in lockstep with position and velocity and serialized with the scene.

Editor (frame-editor):

- Entities now render in their own color, editable per entity from the Inspector. The selected entity is brightened rather than recolored, so its color stays visible while you edit it.

### Changed

Engine:

- Spawning now reuses the first freed entity slot instead of always allocating a new id, so ids stay stable and freed slots are reclaimed.

### Fixed

Editor (frame-editor):

- The editor no longer segfaults on window close. GPU and window resources are now released while the platform connection is still alive, instead of being dropped after the event loop has already torn down.

## [0.1.0] - 2026-06-27

First tagged release: a working simulation engine with a companion 3D editor. Still an
early tech demo, not ready to build a game with.

### Added

Engine:

- Deterministic fixed-timestep clock (30 ticks per second) with spiral-of-death protection, in its own `core/` module.
- Hand-rolled ECS-style world: entities as indices, component data in per-component storage, with runtime spawn and despawn.
- Generic `ComponentStorage<T>` wired into the world, so adding a component type is one field.
- `Position` and `Velocity` components (both `Copy`), and a movement system that advances entities each tick.
- Scene serialization to and from human-readable RON.
- Read-only ASCII debug renderer for headless use.
- Library and binary split, so the engine is importable by other crates.

Editor (frame-editor):

- Native window (winit) with a 3D viewport rendered on the GPU (wgpu): instanced shaded cubes, a perspective camera, and a depth buffer for correct occlusion.
- Orbit, pan, and zoom camera (left-drag pan, scroll zoom, middle-drag orbit).
- Click-to-pick entity selection with highlight.
- Live entity editing: move, spawn, and despawn from the keyboard, and edit the selected entity's position and velocity from the Inspector panel, written straight back into the world.
- Scene save and load (`F5` and `F9`), with a scene loaded on startup.
- A docked, resizable egui panel layout: a top toolbar, a right inspector dock (Scene list and Inspector), and a bottom console dock (live Output log and a Terminal placeholder).
- Runs the simulation live on the engine's clock, with play, pause, and step.

### Known issues

- The editor can crash on window close during GPU teardown. It does not affect editing or saved scenes.

[Unreleased]: https://github.com/outerframehq/frame-engine/compare/0.1.0...HEAD
[0.1.0]: https://github.com/outerframehq/frame-engine/releases/tag/0.1.0
