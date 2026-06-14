 Frame Engine

A custom **simulation engine** written in Rust.

Frame Engine advances a world state forward in fixed, deterministic time steps. It is a *simulation* engine, not a renderer — the simulation runs headless (no window required) and rendering is a separate, swappable layer on top. This separation lets the engine run on servers and be reused across different games.

## Status

Early development. The core fixed-timestep tick loop is working — the engine runs at a steady, deterministic tick rate. Next: building out the world state that the loop simulates.

## Principles

- **Simulation is separate from rendering** — the simulation knows nothing about how it's drawn.
- **Headless by default** — runs with no window; rendering is optional and added on top.
- **Deterministic, fixed-timestep** — one tick is always the same slice of simulated time, so behaviour is identical across machines.
- **Reusable** — kept as its own crate so it can power more than one game.

## Structure
crates/frame-engine/src/

├── core/      tick loop and fixed-timestep clock

├── world/     simulation state (grid, entities)

├── systems/   logic that runs each tick

└── render/    reads world state and draws it

## Building

Requires [Rust](https://rustup.rs).
cargo run

*(Build instructions will expand as the engine grows.)*

## License

MIT — see [LICENSE](LICENSE).

## Design notes

See [DESIGN.md](DESIGN.md) for architecture decisions and reasoning.
