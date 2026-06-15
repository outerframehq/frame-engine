# Frame Engine — Design & Tech Notes

*A living document for Frame Engine itself. Engine concerns only — game-specific design lives with the game that uses the engine.*

---

## What Frame Engine is

A custom **simulation engine** written in Rust. Its job is to advance a world state forward in fixed time steps, deterministically. It is not tied to any one game.

## Core principles

- **Simulation is separate from rendering.** The simulation is pure logic over state — it knows nothing about windows, graphics, or how the world is drawn. A renderer is a separate layer that *reads* simulation state and draws it. This is what allows the simulation to run headless (on servers) and lets the renderer be swapped (e.g. a debug view now, a fuller renderer later) without touching the engine.
- **Headless by default.** The simulation must run with no window — that is its primary home (authoritative servers). Rendering is an optional layer added on top.
- **Deterministic, fixed-timestep simulation.** One tick always represents the same slice of simulated time, independent of how fast the hardware runs. Same inputs → same outputs on any machine. (Required for networked, multi-machine simulation.)
- **Reusable.** The engine is its own crate, kept structurally separate from any game that depends on it, so it can power more than one game.

## Architecture / folder layout

Workspace layout (`crates/` holds Rust packages; the engine is one crate, a game would be another):

- `core/` — the heartbeat: the tick loop and fixed-timestep clock that drives everything.
- `world/` — simulation *state*: the world grid, entities, their data. Numbers, no logic, no drawing.
- `systems/` — logic that runs each tick and *changes* world state (movement, AI, etc.).
- `render/` — reads world state and draws it. Deliberately separated so it can be replaced freely.

Flow: `world` (state) → `systems` (change state each tick) → `render` (draw state), with `core` driving the loop.

*(Folder structure is conventional, not sacred — Rust doesn't enforce it, so it can be reshaped cheaply as the real structure emerges.)*

## Tech decisions

- **Language:** Rust, for both engine and game (one language, no FFI seam; compile-time safety lands on the riskiest code — the long-lived server simulation).
- **Architecture pattern:** Hand-rolled ECS-style data layout — component data stored in parallel lists indexed by entity ID, rather than as objects that own their data. An entity is just an index; its data lives in per-component lists, and systems sweep those lists in bulk each tick. Built by hand rather than using an ECS crate, to keep the data layout under our own control and understand it fully.
- **Editor:** Zed.

## Open questions / not yet decided

- Renderer choice (Bevy vs a hand-rolled stack like wgpu/macroquad + an ECS crate).
- Tick rate (starting assumption: 30 ticks/sec).
- Networking, persistence, and multi-zone server architecture — designed at a high level for the target game, but not yet engine code.

## Implemented so far

- **Fixed-timestep tick loop** (`core` concern, currently in `main.rs`): an accumulator-based loop running at a fixed `TICK_RATE` (currently 30 ticks/sec). Decouples simulation speed from hardware speed — every tick advances the sim by an identical slice of time, so behaviour is deterministic across machines. A catch-up cap (`MAX_CATCHUP_TICKS`) prevents the spiral of death: after a hard stall, excess owed time is dropped rather than replayed.

- **Minimal hand-rolled ECS world** (`world/`): the world stores components in parallel lists indexed by entity ID — an entity is just an index, and its data lives in per-component lists (currently `positions: Vec<Option<Position>>`, where `Some` means the entity has that component and `None` means it doesn't). The `Vec<Option<T>>` (index-is-ID) storage is the simple, clear version; a sparser layout can replace it later if scale demands, without the rest of the engine caring. `Position` carries x/y/z — the z axis is included from the start so verticality (floors, bunkers, mining) is native rather than bolted on later.

- **First system, in its own module** (`systems/`): simulation logic lives in named functions in the `systems` module, not inline in the tick loop. The movement system (`systems::movement`) sweeps the world's component lists each tick and advances every entity. The tick loop *calls* systems (`systems::movement(&mut world)`) rather than spelling out the logic itself — so adding new behaviour (AI, etc.) means adding a system and a call, not touching the loop. With this, the ECS shape is complete: entities (indices), components (the lists), and systems (the logic that sweeps them).
