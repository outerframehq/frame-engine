# Changelog

All notable changes to Frame Engine are recorded here.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project aims to follow [Semantic Versioning](https://semver.org/spec/v2.0.0.html)
once it reaches 1.0. Until then, the minor version bumps when a real new capability
lands and the patch version bumps for fixes and small additions.

## [Unreleased]

## [0.2.0] - 2026-07-05

The first release meant to be genuinely usable: you can build a scene, script it,
watch it collide, arrange the editor to taste, and save what you made. Past a tech
demo, though still early.

### Added

Engine:

- A `Color` component, kept in lockstep with position and velocity and serialized with the scene.
- An input system and a logical `InputState`, so entities can be driven by held buttons instead of only fixed velocity.
- A `Controlled` marker (tag) component; the input system now drives every entity that carries it.
- A `Scale` component (per-axis size factor, x/y/z), kept in lockstep with the other components and serialized with the scene.
- A `Script` component that names a shared behaviour, plus a `script_library` on the world (script name to source) so a script's source lives once and every entity that uses it changes together.
- A `ScriptRuntime` trait (the seam a host implements to run scripts) and a `run_scripts` system. The engine stores script source as data and owns the seam; it interprets nothing itself.
- A `Mesh` component (Cube, Sphere, or Plane primitive), per-entity appearance data serialized with the scene; defaults to Cube, so older scenes load unchanged.
- AABB collision detection: an `ENTITY_SIZE` constant and a `collision` system that records overlapping entity-box pairs on the world as triggers — detection only, with no physics response. The boxes are scale-boxes (shape-agnostic), and the result is transient (never saved with the scene).

Editor (frame-editor):

- Entities now render in their own color, editable per entity from the Inspector. The selected entity is brightened rather than recolored, so its color stays visible while you edit it.
- WASD drives the selected entity while the simulation is running.
- Mark an entity Controlled from the Inspector to drive it with WASD. Step moved to the period key so it no longer shares S with movement.
- The Frame Editor logo now appears in the toolbar.
- The editor now has its own application icon, shown by the desktop and taskbar.
- Resize the selected entity from the Inspector. Picking grows the entity's hit-box with its scale, so a scaled-up cube stays clickable.
- Toolbar File, Edit, View, and Help menus, wired to the same actions as the keyboard shortcuts: save and reload a scene, quit, spawn and despawn entities, clear the selection, play/pause, step a tick, and toggle the controls overlay.
- Entity scripts now run, through a Rhai backend in the editor.
- A Script Editor tab in the centre area: a sidebar of script names beside one large code editor with a line-number gutter, for writing and editing the shared script library.
- Assign a script to the selected entity from the Inspector, through a searchable, filterable picker.
- Live syntax checking in the Script Editor: the open script is compile-checked and a status line shows the syntax error's line, column, and message, or confirms it parses. (Syntax only — Rhai surfaces unknown-variable and type errors at run time.)
- Entities render as their chosen primitive (cube, sphere, or plane), and a Mesh dropdown in the Inspector picks the shape per entity.
- Overlapping entities are tinted red in the viewport, a live view of the engine's collision detection.
- Scripts can read whether their entity is colliding this tick, via a read-only `hit` variable. Collision detection now runs first in the tick loop so scripts see it deterministically.
- Dockable panels (egui_dock): the Viewport, Scene, Inspector, and Script Editor are now tabs that can be dragged, tabbed together, and split apart at runtime. The Viewport is one of them, drawn transparently so the 3D scene shows through, with 3D input routed by the viewport tab's own rect. The toolbar and console stay fixed.
- Open a scene or save one to a chosen path through a native file dialog (File → Open scene…, Save scene as…), alongside the existing F5/F9 save-and-reload of the current scene.

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

[Unreleased]: https://github.com/outerframehq/frame-engine/compare/0.2.0...HEAD
[0.2.0]: https://github.com/outerframehq/frame-engine/compare/0.1.0...0.2.0
[0.1.0]: https://github.com/outerframehq/frame-engine/releases/tag/0.1.0
