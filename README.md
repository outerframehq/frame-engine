# Frame Engine

A custom **simulation engine** written in Rust, with a companion authoring tool.

Frame Engine advances a world state forward in fixed, deterministic time steps. It is a *simulation* engine, not a renderer — the simulation runs headless (no window required) and rendering is a separate, swappable layer on top. This separation lets the engine run on servers and be reused across different games and tools.

## Status

Early development. Working so far:

- A deterministic fixed-timestep tick loop with spiral-of-death protection.
- A hand-rolled ECS world with multiple component types (position, velocity) and runtime spawn/despawn of entities.
- A generic `ComponentStorage<T>` type, wired into the world, so adding new component types is cheap and uniform.
- A movement system that advances entities each tick.
- A read-only ASCII debug renderer that draws the world as a grid.
- A library/binary split, so the engine is importable as a library by other crates.
- **frame-editor**, a separate tool crate that links to the engine and opens a native window (windowing via `winit`).

Currently at the frontier: getting the editor window to reliably display (a known winit-on-Wayland quirk), then drawing into it and building an inspector over the engine's world.

## Principles

- **Simulation is separate from rendering** — the simulation knows nothing about how it's drawn.
- **Headless by default** — runs with no window; rendering is optional and added on top.
- **Deterministic, fixed-timestep** — one tick is always the same slice of simulated time, so behaviour is identical across machines.
- **Reusable** — the engine is its own library crate, so it can power more than one game or tool. Dependencies point inward: tools depend on the engine, never the reverse.
- **Hand-roll the heart, buy the rest** — the engine is built by hand to be understood deeply; solved problems that aren't the heart (like cross-platform windowing) use existing libraries.

## Workspace structure

This repository is a Cargo workspace holding multiple crates:

```
crates/
├── frame-engine/       the simulation engine (library + binary)
│   └── src/
│       ├── core/       tick loop and fixed-timestep clock
│       ├── world/      simulation state (entities, components, storage)
│       ├── systems/    logic that runs each tick
│       ├── render/     reads world state and draws it
│       ├── lib.rs      library root — exposes the engine to other crates
│       └── main.rs     binary runner — drives the tick loop
└── frame-editor/       authoring tool; depends on frame-engine
    └── src/
        └── main.rs     opens a window and holds an engine world
```

The engine and editor live in one repo so they evolve together, but build as separate targets.

## Building

Requires [Rust](https://rustup.rs).

Run the engine (headless tick loop + ASCII debug view):

```
cargo run -p frame-engine
```

Run the editor (opens a window):

```
cargo run -p frame-editor
```

*(Build instructions will expand as the project grows.)*

## License

MIT — see [LICENSE](LICENSE).

## Design notes

See [DESIGN.md](DESIGN.md) for architecture decisions and reasoning.
