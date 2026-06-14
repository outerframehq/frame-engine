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
- **Architecture pattern:** ECS-style data layout (flat columns of component data processed in bulk) — both pleasant in Rust and fast for many-entity simulation.
- **Editor:** Zed.

## Open questions / not yet decided

- Renderer choice (Bevy vs a hand-rolled stack like wgpu/macroquad + an ECS crate).
- Tick rate (starting assumption: 30 ticks/sec).
- Networking, persistence, and multi-zone server architecture — designed at a high level for the target game, but not yet engine code.
