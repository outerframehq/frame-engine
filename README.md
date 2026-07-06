# Frame Engine

A simulation engine written in Rust, with a companion 3D editor.

Frame Engine advances a world state forward in fixed, deterministic time steps. It is a simulation engine, not a renderer. The simulation runs headless with no window required, and rendering is a separate, swappable layer on top. That separation lets the engine run on servers and be reused across different games and tools.

## Forking and contributing

Frame Engine is built in the open and meant to be learned from. If it looks useful, fork it, clone it, and bend it to whatever you are making. The MIT licence means you can do that freely, for any purpose, with no permission asked.

A few things worth knowing before you dig in:

- **It is hand-rolled on purpose.** The engine's world, systems, and clock are written by hand rather than pulled from an ECS crate, so the code is meant to be read and understood top to bottom rather than treated as a black box. Reading [DESIGN.md](DESIGN.md) first is the fastest way in: it explains not just what each piece does but why it is shaped that way.
- **The engine stays graphics-free.** `crates/frame-engine` depends on nothing but `serde` and `ron`. All windowing, GPU, and UI code lives in `crates/frame-editor`. If you extend the engine, keeping that line clean is the one constraint worth respecting, because it is what lets the simulation run headless on a server.
- **Dependencies point one way.** The editor depends on the engine, never the reverse. The engine knows nothing about the editor or any application built on it.
- **`main` is the working branch.** It is where features land and can be unstable. Fork from `main` if you want to hack on the latest; depend on a tagged release if you want something steady (see below).

If you build something the engine is missing, a system, a component, a piece of the editor, and you think it fits, get in touch. Open an issue to talk it through, or a pull request if you have already built it. Nothing is guaranteed to land, but good work that fits the project's direction is genuinely welcome, and if it suits the engine it may well make it into the main public build.

Built something with it, or forked it into your own thing? Tag me on X at [@OuterFrameInter](https://x.com/OuterFrameInter), I would love to see it, and that is also the easiest way to reach me if GitHub is not your preferred route.

Frame Engine is a learning project as much as a tool: I am still growing my own Rust as I build the engine and editor in the open, which is part of why the code is written to be read rather than to show off. Considered feedback, on the engine, the editor, or the overall approach, is always welcome.

## Releases and stability

`main` is the working branch, where in-progress features land. It is under constant change and can be incomplete or unstable at any moment.

**For a stable build, download a tagged release rather than cloning `main`.** Releases are cut at points where the project is known to build and run, so a release is your dependable copy. Tagged releases are on the project's Releases page.

The latest release is **0.2.0**. The capabilities listed below reflect current `main`, which may be ahead of that release.

## Status

Early development, past the toy stage, with a working engine and a usable editor shell.

**Engine**

- A deterministic fixed-timestep clock (in `core/`) with spiral-of-death protection, used by both the engine binary and the editor.
- A hand-rolled ECS world with several component types (position, velocity, colour, a per-axis scale, a `Mesh` primitive, and a `Controlled` marker) and runtime spawn and despawn. Spawn reuses freed slots, so entity ids stay stable and despawned slots are reclaimed.
- A generic `ComponentStorage<T>` type, wired into the world. It implements `Default`, and `World` derives `Default`, so a fresh world is built in one place and adding a component type is cheap and uniform.
- A movement system that advances entities each tick, and an input system that drives `Controlled` entities from held WASD keys.
- Per-entity colour and scale, stored as component data and serialized with the scene.
- A per-entity `Mesh` primitive (cube, sphere, or plane), stored as component data and serialized with the scene; defaults to cube, so older scenes load unchanged.
- AABB collision detection: a `collision` system records which entity boxes overlap each run, as triggers — detection only, with no physics response. Boxes are axis-aligned scale-boxes derived from a shared `ENTITY_SIZE`, the world-space size the editor also renders and picks against.
- Per-entity scripting: an entity can carry a `Script` that names a shared library script (source held once on the world, in a name-to-source map), with a `ScriptRuntime` trait and a `run_scripts` system forming the seam. The engine stores script source as data and runs nothing itself — the interpreter lives in the editor, the same way rendering does.
- A graphics-free input abstraction (which buttons are held), fed by the editor and read by systems, so input can drive the simulation without the engine knowing about windowing.
- Scene serialization to and from a human-readable RON file (`serde` and RON), with backward compatibility for scenes saved before newer fields like colour and scale existed.
- A read-only ASCII debug renderer that draws the world as a grid.
- A library and binary split, so the engine is importable by other crates.

**frame-editor** (companion editor; links to the engine, runs the sim in a window)

- Opens a native window (`winit`) and renders the world on the GPU through `wgpu`.
- Draws entities as instanced, shaded primitives (cube, sphere, or plane) in real 3D, each in its own colour and size, with a perspective camera and a depth buffer for correct occlusion. Overlapping entities are tinted red, a live view of the engine's collision detection.
- An orbit, pan, and zoom camera (left-drag pan, scroll zoom, middle-drag orbit).
- Click-to-pick selection: click an entity to select it. The selected entity is brightened rather than recoloured, so its own colour stays visible while you edit it.
- Live entity editing: nudge the selection with the arrow keys and Page Up/Down, spawn with `N`, despawn with `Delete`, and drive a `Controlled` entity with WASD while the sim is playing.
- A project launcher: the editor opens on a launcher to create a named project, open one by folder, or reopen a recent one from a list of cards (name, description, last-edited date, version, and Edit / Play / Settings actions). A project is a folder holding a scene file named after it and a `project.ron` manifest (description, version); Settings edits those and renames the scene file.
- Scene save and load: opening a project loads its scene, `F5` saves it and `F9` reloads it, and the File menu also opens or saves a scene to any path through a native file dialog.
- A Script Editor: a dockable tab where the shared script library is written — a sidebar of script names beside a single code editor with a line-number gutter and a live syntax check (a status line flags parse errors with their line and column). Scripts run live through a Rhai backend and are assigned to entities from the Inspector. See [SCRIPTING.md](SCRIPTING.md) for how to write them.
- A dockable panel layout built with `egui` and `egui_dock`. The Viewport, Scene, Inspector, and Script Editor are tabs you can drag, tab together, and split apart; the Viewport is a transparent tab so the 3D shows through. Alongside them:
  - a top toolbar showing the editor's logo and working File/Edit/View/Help menus, each item mirroring a keyboard shortcut (open/save/reload scene, close project, quit; spawn, despawn, clear selection; play/pause, step, controls overlay),
  - a Scene tab (lists entities, click to select) and an Inspector tab (edit the selected entity's position, velocity, colour, and scale, pick its mesh primitive, toggle whether it is `Controlled`, and assign a library script through a searchable picker, all written straight back into the world),
  - a fixed bottom console dock with an Output tab showing a live log and a Terminal placeholder.
- Runs the simulation live on the engine's fixed-timestep clock, so the sim ticks at a true 30 per second independent of the window's repaint rate, with play, pause, and step controls.

Currently at the frontier: playing a project as a standalone game window; richer authoring (gizmos, undo and redo, prefabs); per-entity appearance beyond colour, scale, and mesh (material, textures); collision that does more than detect (a response, mesh-fitted boxes, richer script queries); and a more discoverable script API.

## Principles

- **Simulation is separate from rendering.** The simulation knows nothing about how it is drawn. The editor reads the world and draws it across a crate boundary, and never owns the state. The engine crate pulls in no graphics libraries — and no script interpreter: it holds script source as data and leaves running it to the host, just as it leaves drawing to the host.
- **Headless by default.** Runs with no window. Rendering is optional and added on top.
- **Deterministic, fixed-timestep.** One tick is always the same slice of simulated time, so behaviour is identical across machines. The editor honours this with the engine's own clock rather than ticking once per rendered frame.
- **Reusable.** The engine is its own library crate, so it can power more than one game or tool. Dependencies point inward: tools depend on the engine, never the reverse.
- **Hand-roll the heart, buy the rest.** The engine is built by hand to be understood deeply. Solved problems that are not the heart (windowing, the GPU API, linear algebra, UI, file dialogs, config paths, dates) use existing libraries: `winit`, `wgpu`, `glam`, `egui` and `egui_dock`, `rfd`, `dirs`, `chrono`, and — in the editor, for the project manifest — `serde` with `ron`.
- **No premature abstraction.** Machinery is built when the pain is real, not before. The fixed-timestep clock stayed duplicated inline until a second consumer made the duplication real, then moved into `core/`.

## Workspace structure

This repository is a Cargo workspace holding multiple crates:

```
crates/
├── frame-engine/       the simulation engine (library and binary)
│   └── src/
│       ├── core/       fixed-timestep clock (clock.rs)
│       ├── world/      simulation state (entities, components, storage)
│       ├── systems/    logic that runs each tick
│       ├── input/      graphics-free input abstraction (held buttons)
│       ├── render/     read-only ASCII debug view
│       ├── lib.rs      library root, exposes the engine to other crates
│       └── main.rs     binary runner, drives the tick loop
└── frame-editor/       editor; depends on frame-engine
    └── src/
        ├── main.rs     windowed 3D editor: app state, camera, picking,
        │               entity editing, the egui panel layout, and the
        │               wgpu render pipeline
        ├── script.rs   the Rhai script runtime (the editor's ScriptRuntime)
        ├── shader.wgsl entity shader (instanced, shaded primitives)
        ├── text.wgsl   screen-space overlay shader (controls legend)
        └── font.rs     hand-rolled bitmap font for the overlay
```

The engine and editor live in one repo so they evolve together, but build as separate targets.

## Building

Requires [Rust](https://rustup.rs).

Run the engine (headless tick loop and ASCII debug view):

```
cargo run -p frame-engine
```

Run the editor (opens the project launcher; pick or create a project to enter the 3D editor):

```
cargo run -p frame-editor
```

Editor controls:

- **Space** play or pause, **.** step one tick while paused
- **WASD** drive a `Controlled` entity (while playing)
- **Left-click** select an entity, **Esc** clear selection
- **Arrow keys** move selection on X and Y, **Page Up / Page Down** move on Z
- **N** spawn an entity, **Delete** despawn the selection
- **F5** save scene, **F9** reload scene
- **Left-drag** pan, **Scroll** zoom, **Middle-drag** orbit
- **H** toggle the controls overlay

## License

MIT. See [LICENSE](LICENSE).

## Design notes

See [DESIGN.md](DESIGN.md) for architecture decisions and reasoning, and [SCRIPTING.md](SCRIPTING.md) for the guide to writing entity scripts.
