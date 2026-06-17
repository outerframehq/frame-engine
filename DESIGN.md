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
- `world/` — simulation *state*: entities, their components, and the storage that holds them. Numbers, no logic, no drawing.
- `systems/` — logic that runs each tick and *changes* world state (movement, AI, etc.).
- `render/` — reads world state and draws it. Deliberately separated so it can be replaced freely.

Flow: `world` (state) → `systems` (change state each tick) → `render` (draw state), with `core` driving the loop.

*(Folder structure is conventional, not sacred — Rust doesn't enforce it, so it can be reshaped cheaply as the real structure emerges.)*

## Tech decisions

- **Language:** Rust, for both engine and game (one language, no FFI seam; compile-time safety lands on the riskiest code — the long-lived server simulation).
- **Architecture pattern:** Hand-rolled ECS-style data layout — component data stored in parallel lists indexed by entity ID, rather than as objects that own their data. An entity is just an index; its data lives in per-component lists, and systems sweep those lists in bulk each tick. Built by hand rather than using an ECS crate, to keep the data layout under our own control and understand it fully.
- **Editor:** Zed.

## Open questions / not yet decided

- Renderer choice for the eventual *real* renderer (Bevy vs a hand-rolled stack like wgpu/macroquad). The current renderer is a headless ASCII debug view only.
- Networking, persistence, and multi-zone server architecture — designed at a high level for the target game, but not yet engine code.
- How far to take generic component storage — a fixed set of named component fields on `World` (current direction) vs. fully generic runtime component registration (a later, larger step).

## Implemented so far

- **Fixed-timestep tick loop** (`core` concern, currently in `main.rs`): an accumulator-based loop running at a fixed `TICK_RATE` (currently 30 ticks/sec). Decouples simulation speed from hardware speed — every tick advances the sim by an identical slice of time, so behaviour is deterministic across machines. A catch-up cap (`MAX_CATCHUP_TICKS`) prevents the spiral of death: after a hard stall, excess owed time is dropped rather than replayed.

- **Hand-rolled ECS world** (`world/`): entities are indices; component data lives in per-component lists indexed by entity ID (`Some` = entity has that component, `None` = it doesn't). Currently two component types — `Position` (x/y/z; the z axis is included from the start so verticality like floors, bunkers, and mining is native rather than bolted on later) and `Velocity` (dx/dy/dz). The world supports runtime **spawn** (create an entity, reusing freed slots where possible) and **despawn** (clear an entity's slots; the index stays valid, preserving stable IDs).

- **Systems, in their own module** (`systems/`): simulation logic lives in named functions, not inline in the tick loop. The movement system (`systems::movement`) reads each entity's velocity and applies it to its position, advancing every entity each tick. The tick loop *calls* systems (`systems::movement(&mut world)`) rather than spelling out the logic — so adding behaviour means adding a system and a call, not touching the loop. With this, the ECS shape is complete: entities (indices), components (the lists), and systems (the logic that sweeps them).

- **Debug renderer** (`render/`): a read-only ASCII view that draws the world as a grid each frame, mapping entity x/y onto grid cells. Read-only by design (takes `&World`, never `&mut`), enforcing the simulation/rendering separation. Render rate is decoupled from tick rate (the sim ticks every step; the view redraws every Nth tick) — a small first instance of the principle that simulation rate and render rate are independent.

- **Generic component storage** (`world/storage.rs`) — *in progress*: a reusable `ComponentStorage<T>` type (a `Vec<Option<T>>` plus `insert`/`get`/`remove`) so each component type uses the same tested storage code instead of a hand-written `Vec<Option<...>>` per type. Built and compiling; not yet wired into `World` (next step). Once wired in, adding a new component type becomes adding one storage field rather than repeating the storage pattern by hand.
