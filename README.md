# Frame Engine

A custom **simulation engine** written in Rust, with a companion authoring tool.

Frame Engine advances a world state forward in fixed, deterministic time steps. It is a *simulation* engine, not a renderer — the simulation runs headless (no window required) and rendering is a separate, swappable layer on top. This separation lets the engine run on servers and be reused across different games and tools.

## Status

Early development. Working so far:

**Engine**
- A deterministic fixed-timestep tick loop with spiral-of-death protection.
- A hand-rolled ECS world with multiple component types (position, velocity) and runtime spawn/despawn of entities.
- A generic `ComponentStorage<T>` type, wired into the world, so adding new component types is cheap and uniform.
- A movement system that advances entities each tick.
- A read-only ASCII debug renderer that draws the world as a grid.
- A library/binary split, so the engine is importable as a library by other crates.

**frame-editor** (companion authoring tool — links to the engine, runs the sim in a window)
- Opens a native window (`winit`) and draws into it via a CPU pixel buffer (`softbuffer`).
- Reads the engine's world and draws its entities, projected through a camera.
- Runs the simulation **live** on its own fixed-timestep clock, so the sim ticks at a true 30/sec independent of the window's repaint rate.
- **Play / pause / step** controls (`Space` to pause, `S` to step one tick) — the editor controls *when* the world advances.
- A **pan/zoom camera** (left-drag to pan, scroll to zoom).
- A **fake-depth cue**: entity z modulates square size and brightness, making the (always-3D) z axis visible — a depth cue, not yet real 3D rendering.

Currently at the frontier: building an **inspector** — clicking an entity to read its data (position/velocity) back through the engine's `ComponentStorage`. Further out: a real 3D renderer (the simulation is already 3D; only the viewport is 2D), and scene save/load.

## Principles

- **Simulation is separate from rendering** — the simulation knows nothing about how it's drawn. (The editor *reads* the world and draws it across a crate boundary; it never owns the state.)
- **Headless by default** — runs with no window; rendering is optional and added on top.
- **Deterministic, fixed-timestep** — one tick is always the same slice of simulated time, so behaviour is identical across machines. The editor honours this with its own fixed-timestep clock rather than ticking per rendered frame.
- **Reusable** — the engine is its own library crate, so it can power more than one game or tool. Dependencies point inward: tools depend on the engine, never the reverse.
- **Hand-roll the heart, buy the rest** — the engine is built by hand to be understood deeply; solved problems that aren't the heart (cross-platform windowing, getting pixels on screen) use existing libraries (`winit`, `softbuffer`).
- **No premature abstraction** — machinery is built when the pain is real, not before. (Example: the fixed-timestep clock is currently duplicated between engine and editor; it stays duplicated until that genuinely hurts.)

## Workspace structure

This repository is a Cargo workspace holding multiple crates:

```
crates/
├── frame-engine/       the simulation engine (library + binary)
│   └── src/
│       ├── core/       tick loop and fixed-timestep clock (loop currently lives in main.rs)
│       ├── world/      simulation state (entities, components, storage)
│       ├── systems/    logic that runs each tick
│       ├── render/     reads world state and draws it (ASCII debug view)
│       ├── lib.rs      library root — exposes the engine to other crates
│       └── main.rs     binary runner — drives the tick loop
└── frame-editor/       authoring tool; depends on frame-engine
    └── src/
        └── main.rs     windowed viewport: draws the live world, with
                        play/pause/step controls and a pan/zoom camera
```

The engine and editor live in one repo so they evolve together, but build as separate targets.

## Building

Requires [Rust](https://rustup.rs).

Run the engine (headless tick loop + ASCII debug view):

```
cargo run -p frame-engine
```

Run the editor (opens a window showing the live world):

```
cargo run -p frame-editor
```

Editor controls: **Space** play/pause · **S** step one tick (while paused) · **left-drag** pan · **scroll** zoom.

*(Build instructions will expand as the project grows.)*

## License

MIT — see [LICENSE](LICENSE).

## Design notes

See [DESIGN.md](DESIGN.md) for architecture decisions and reasoning.
