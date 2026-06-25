# Frame Engine

A custom **simulation engine** written in Rust, with a companion 3D authoring tool.

Frame Engine advances a world state forward in fixed, deterministic time steps. It is a *simulation* engine, not a renderer — the simulation runs headless (no window required) and rendering is a separate, swappable layer on top. This separation lets the engine run on servers and be reused across different games and tools.

## Status

Early development, but past the toy stage. Working so far:

**Engine**
- A deterministic fixed-timestep clock (in `core/`) with spiral-of-death protection, consumed by both the engine binary and the editor.
- A hand-rolled ECS world with multiple component types (position, velocity) and runtime spawn/despawn of entities.
- A generic `ComponentStorage<T>` type, wired into the world, so adding new component types is cheap and uniform.
- A movement system that advances entities each tick.
- Scene serialization to/from a human-readable RON file (`serde` + RON).
- A read-only ASCII debug renderer that draws the world as a grid (the headless, zero-dependency view).
- A library/binary split, so the engine is importable as a library by other crates.

**frame-editor** (companion authoring tool — links to the engine, runs the sim in a window)
- Opens a native window (`winit`) and renders the world on the **GPU via `wgpu`**.
- Draws entities as **instanced, shaded cubes** in real 3D, with a **perspective camera** and a **depth buffer** for correct occlusion.
- An **orbit / pan / zoom camera** (left-drag pan, scroll zoom, middle-drag orbit).
- **Click-to-pick selection**: click an entity to select it, highlight it (orange), and read its position/velocity back through the engine's `ComponentStorage`.
- An **on-screen inspector**: the selected entity's data is drawn in the viewport with a hand-rolled bitmap font, screen-anchored so it stays put as you move the camera.
- Runs the simulation **live** on the engine's fixed-timestep clock, so the sim ticks at a true 30/sec independent of the window's repaint rate.
- **Play / pause / step** controls (`Space` to pause, `S` to step one tick) — the editor controls *when* the world advances; pausing freezes the sim but not the view.

Currently at the frontier: wiring the engine's scene serialization into the editor as a **save/load authoring flow** — load a scene, edit it with the selection and picking that now exist, save it back. Further out: per-entity appearance, gizmos, and richer authoring.

## Principles

- **Simulation is separate from rendering** — the simulation knows nothing about how it's drawn. (The editor *reads* the world and draws it across a crate boundary; it never owns the state. The engine crate pulls in no graphics libraries at all.)
- **Headless by default** — runs with no window; rendering is optional and added on top.
- **Deterministic, fixed-timestep** — one tick is always the same slice of simulated time, so behaviour is identical across machines. The editor honours this with the engine's own clock rather than ticking per rendered frame.
- **Reusable** — the engine is its own library crate, so it can power more than one game or tool. Dependencies point inward: tools depend on the engine, never the reverse.
- **Hand-roll the heart, buy the rest** — the engine is built by hand to be understood deeply; solved problems that aren't the heart (windowing, the GPU API, linear algebra) use existing libraries (`winit`, `wgpu`, `glam`).
- **No premature abstraction** — machinery is built when the pain is real, not before. (Example: the fixed-timestep clock lived duplicated inline until a second consumer made the duplication real, then moved into `core/` — not a moment sooner.)

## Workspace structure

This repository is a Cargo workspace holding multiple crates:

```
crates/
├── frame-engine/       the simulation engine (library + binary)
│   └── src/
│       ├── core/       fixed-timestep clock (clock.rs)
│       ├── world/      simulation state (entities, components, storage)
│       ├── systems/    logic that runs each tick
│       ├── render/     read-only ASCII debug view
│       ├── lib.rs      library root — exposes the engine to other crates
│       └── main.rs     binary runner — drives the tick loop
└── frame-editor/       authoring tool; depends on frame-engine
    └── src/
        ├── main.rs     windowed 3D viewport: app state, camera, picking,
        │               the wgpu render pipeline, and the tick loop
        ├── shader.wgsl entity shader (instanced, shaded cubes)
        ├── text.wgsl   screen-space overlay shader (inspector text)
        └── font.rs     hand-rolled bitmap font for the inspector
```

The engine and editor live in one repo so they evolve together, but build as separate targets.

## Building

Requires [Rust](https://rustup.rs).

Run the engine (headless tick loop + ASCII debug view):

```
cargo run -p frame-engine
```

Run the editor (opens a 3D window showing the live world):

```
cargo run -p frame-editor
```

Editor controls: **Space** play/pause · **S** step one tick (while paused) · **left-click** select entity · **Esc** clear selection · **left-drag** pan · **scroll** zoom · **middle-drag** orbit.

*(Build instructions will expand as the project grows.)*

## License

MIT — see [LICENSE](LICENSE).

## Design notes

See [DESIGN.md](DESIGN.md) for architecture decisions and reasoning.
