# Frame Engine

A simulation engine written in Rust, with a companion 3D editor.

Frame Engine advances a world state forward in fixed, deterministic time steps. It is a simulation engine, not a renderer. The simulation runs headless with no window required, and rendering is a separate, swappable layer on top. That separation lets the engine run on servers and be reused across different games and tools.

## Releases and stability

`main` is the working branch, where in-progress features land. It is under constant change and can be incomplete or unstable at any moment.

**For a stable build, download a tagged release rather than cloning `main`.** Releases are cut at points where the project is known to build and run, so a release is your dependable copy. Tagged releases are on the project's Releases page.

The latest release is **0.1.0**. The capabilities listed below reflect current `main`, which is ahead of that release.

## Status

Early development, past the toy stage, with a working engine and a usable editor shell.

**Engine**

- A deterministic fixed-timestep clock (in `core/`) with spiral-of-death protection, used by both the engine binary and the editor.
- A hand-rolled ECS world with several component types (position, velocity, colour, and a `Controlled` marker) and runtime spawn and despawn. Spawn reuses freed slots, so entity ids stay stable and despawned slots are reclaimed.
- A generic `ComponentStorage<T>` type, wired into the world. It implements `Default`, and `World` derives `Default`, so a fresh world is built in one place and adding a component type is cheap and uniform.
- A movement system that advances entities each tick, and an input system that drives `Controlled` entities from held WASD keys.
- Per-entity colour, stored as component data and serialized with the scene.
- A graphics-free input abstraction (which buttons are held), fed by the editor and read by systems, so input can drive the simulation without the engine knowing about windowing.
- Scene serialization to and from a human-readable RON file (`serde` and RON), with backward compatibility for scenes saved before colours existed.
- A read-only ASCII debug renderer that draws the world as a grid.
- A library and binary split, so the engine is importable by other crates.

**frame-editor** (companion editor; links to the engine, runs the sim in a window)

- Opens a native window (`winit`) and renders the world on the GPU through `wgpu`.
- Draws entities as instanced, shaded cubes in real 3D, each in its own colour, with a perspective camera and a depth buffer for correct occlusion.
- An orbit, pan, and zoom camera (left-drag pan, scroll zoom, middle-drag orbit).
- Click-to-pick selection: click an entity to select it. The selected entity is brightened rather than recoloured, so its own colour stays visible while you edit it.
- Live entity editing: nudge the selection with the arrow keys and Page Up/Down, spawn with `N`, despawn with `Delete`, and drive a `Controlled` entity with WASD while the sim is playing.
- Scene save and load: `F5` saves the world to a RON file, `F9` reloads it, and the editor loads a scene on startup.
- A docked panel layout built with `egui`:
  - a top toolbar showing the editor's logo, with placeholder menus,
  - a right inspector dock with a Scene tab (lists entities, click to select) and an Inspector tab (edit the selected entity's position, velocity, and colour, and toggle whether it is `Controlled`, all written straight back into the world),
  - a bottom console dock with an Output tab showing a live log and a Terminal placeholder,
  - panels that are solid but resizable.
- Runs the simulation live on the engine's fixed-timestep clock, so the sim ticks at a true 30 per second independent of the window's repaint rate, with play, pause, and step controls.

Currently at the frontier: wiring the toolbar and File menu to the actions that already work by keyboard, then richer authoring (gizmos, undo and redo, prefabs) and further per-entity appearance (scale, mesh) beyond the colour that now exists.

## Principles

- **Simulation is separate from rendering.** The simulation knows nothing about how it is drawn. The editor reads the world and draws it across a crate boundary, and never owns the state. The engine crate pulls in no graphics libraries.
- **Headless by default.** Runs with no window. Rendering is optional and added on top.
- **Deterministic, fixed-timestep.** One tick is always the same slice of simulated time, so behaviour is identical across machines. The editor honours this with the engine's own clock rather than ticking once per rendered frame.
- **Reusable.** The engine is its own library crate, so it can power more than one game or tool. Dependencies point inward: tools depend on the engine, never the reverse.
- **Hand-roll the heart, buy the rest.** The engine is built by hand to be understood deeply. Solved problems that are not the heart (windowing, the GPU API, linear algebra, UI) use existing libraries: `winit`, `wgpu`, `glam`, `egui`.
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
        ├── shader.wgsl entity shader (instanced, shaded cubes)
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

Run the editor (opens a 3D window with the docked panel layout):

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

See [DESIGN.md](DESIGN.md) for architecture decisions and reasoning.
