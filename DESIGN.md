# Frame Engine — Design & Tech Notes

*A living document for Frame Engine itself. Engine concerns only — game-specific design lives with the game that uses the engine.*

---

## What Frame Engine is

A custom **simulation engine** written in Rust. Its job is to advance a world state forward in fixed time steps, deterministically. It is not tied to any one game.

## Core principles

- **Simulation is separate from rendering.** The simulation is pure logic over state — it knows nothing about windows, graphics, or how the world is drawn. A renderer is a separate layer that *reads* simulation state and draws it. This is what allows the simulation to run headless (on servers) and lets the renderer be swapped (e.g. a debug view now, a fuller renderer later) without touching the engine.
- **Headless by default.** The simulation must run with no window — that is its primary home (authoritative servers). Rendering is an optional layer added on top.
- **Deterministic, fixed-timestep simulation.** One tick always represents the same slice of simulated time, independent of how fast the hardware runs. Same inputs → same outputs on any machine. (Required for networked, multi-machine simulation.)
- **Reusable.** The engine is its own crate, kept structurally separate from any game or tool that depends on it, so it can power more than one thing. Dependencies point *inward*: tools and games depend on the engine; the engine depends on nothing above it.

## Workspace layout

The repository is a Cargo workspace (a monorepo) holding multiple crates under `crates/`:

- `crates/frame-engine/` — the engine itself. Both a **library** (its reusable guts, exposed via `lib.rs`) and a **binary** (a thin `main.rs` runner that drives the tick loop). The library is what other crates import; the binary is one specific way of running it.
- `crates/frame-editor/` — a separate authoring tool that **depends on** `frame-engine` (path dependency). It reaches into the engine's types to inspect and edit world state. The engine knows nothing about the editor — the dependency is one-directional.

Keeping everything in one repo means the engine and editor evolve in lockstep (one `Cargo.lock`, one history, shared `target/`), while still building as separate targets (`cargo build -p frame-engine` vs `-p frame-editor`) — so a headless server build and an editor build are cleanly separable when real builds arrive.

### Engine module layout (`crates/frame-engine/src/`)

- `core/` — the heartbeat: the tick loop and fixed-timestep clock. (Currently the loop still lives in `main.rs`; extracting it into `core/` is a pending refactor.)
- `world/` — simulation *state*: entities, their components, and the storage that holds them. Numbers, no logic, no drawing.
- `systems/` — logic that runs each tick and *changes* world state (movement, AI, etc.).
- `render/` — reads world state and draws it. Deliberately separated so it can be replaced freely.

Flow: `world` (state) → `systems` (change state each tick) → `render` (draw state), with `core` driving the loop.

*(Folder structure is conventional, not sacred — Rust doesn't enforce it, so it can be reshaped cheaply as the real structure emerges.)*

## Tech decisions

- **Language:** Rust, for engine, editor, and eventual game (one language, no FFI seam; compile-time safety lands on the riskiest code — the long-lived server simulation).
- **Architecture pattern:** Hand-rolled ECS-style data layout — component data stored in parallel lists indexed by entity ID, rather than as objects that own their data. An entity is just an index; its data lives in per-component lists, and systems sweep those lists in bulk each tick. Built by hand rather than using an ECS crate, to keep the data layout under our own control and understand it fully.
- **Build vs. buy:** hand-roll what is the heart of the project and worth understanding deeply (the engine). Use existing libraries for solved problems that aren't the heart (e.g. `winit` for cross-platform windowing in the editor) — these are load-bearing but not where the learning or the differentiation lives.
- **Editor:** Zed.

## Open questions / not yet decided

- Renderer choice for the eventual *real* renderer (Bevy vs a hand-rolled stack like wgpu/macroquad). The current renderer is a headless ASCII debug view only.
- Networking, persistence, and multi-zone server architecture — designed at a high level for the target game, but not yet engine code.
- How far to take generic component storage — a fixed set of named component fields on `World` (current direction) vs. fully generic runtime component registration via type-erased storage (a later, larger step, to be built on top of the current generic storage when many component types are needed).
- Gap-storage tradeoffs at scale — the `Vec<Option<T>>` layout leaves holes for despawned entities; sparse sets or similar may be worth revisiting as entity counts grow.

## Implemented so far

- **Fixed-timestep tick loop** (`core` concern, currently in `main.rs`): an accumulator-based loop running at a fixed `TICK_RATE` (30 ticks/sec). Decouples simulation speed from hardware speed — every tick advances the sim by an identical slice of time, so behaviour is deterministic across machines. A catch-up cap (`MAX_CATCHUP_TICKS`) prevents the spiral of death: after a hard stall, excess owed time is dropped rather than replayed.

- **Hand-rolled ECS world** (`world/`): entities are indices; component data lives in per-component lists indexed by entity ID (`Some` = entity has that component, `None` = it doesn't). Currently two component types — `Position` (x/y/z; the z axis is included from the start so verticality like floors, bunkers, and mining is native rather than bolted on later) and `Velocity` (dx/dy/dz). The world supports runtime **spawn** (create an entity) and **despawn** (clear an entity's slots; the index stays valid, preserving stable IDs).

- **Generic component storage** (`world/storage.rs`): a reusable `ComponentStorage<T>` type (a private `Vec<Option<T>>` plus `new`/`insert`/`get`/`remove`/`len` and `iter`/`iter_mut`) so each component type uses the same tested storage code instead of a hand-written `Vec<Option<...>>` per type. **Now wired into `World`** — both `positions` and `velocities` are `ComponentStorage<T>`, and systems and the renderer walk them through the storage's iteration methods rather than touching the underlying `Vec` directly (the inner `Vec` is private; access goes through a controlled API). Adding a new component type is now adding one storage field rather than repeating the storage pattern by hand. This is also the seam the editor's inspector will plug into.

- **Systems, in their own module** (`systems/`): simulation logic lives in named functions, not inline in the tick loop. The movement system reads each entity's velocity and applies it to its position, advancing every entity each tick. The tick loop *calls* systems rather than spelling out the logic — so adding behaviour means adding a system and a call, not touching the loop.

- **Debug renderer** (`render/`): a read-only ASCII view that draws the world as a grid each frame, mapping entity x/y onto grid cells. Read-only by design (takes `&World`, never `&mut`), enforcing the simulation/rendering separation. Render rate is decoupled from tick rate (the sim ticks every step; the view redraws every Nth tick) — a small first instance of the principle that simulation rate and render rate are independent.

- **Library / binary split** (`lib.rs` + `main.rs`): the engine is now both a library (reusable guts exposed via `pub mod world; pub mod systems; pub mod render;` in `lib.rs`) and a binary (a thin `main.rs` that runs the tick loop, consuming the library exactly as any external crate would). This is what makes the engine *importable* by other crates. The binary is the first consumer of the library; the editor is the second.

## Tooling

- **frame-editor** (`crates/frame-editor/`): a separate binary crate that depends on the engine library. It currently constructs an engine `World` from inside the editor (proving the cross-crate link works) and opens a native window via `winit`, using the modern `ApplicationHandler` event-loop model. The editor owns its application state (`App` holds the window and the engine `World`), and drives its own run loop — distinct from the engine's headless tick loop, since an editor wants to control *when* to step the simulation (play/pause/step) rather than tick forever.

- **Known issue:** on Wayland (Pop!_OS / COSMIC), the `winit` window is created but does not always become visible — a known winit-0.30-on-Wayland interaction, not specific to this code. To be resolved (X11 backend forcing, draw-surface setup, or a future winit fix). Window display is the current frontier of the editor work; once a window reliably shows, the next steps are drawing into it and building an inspector that reads the engine `World` through `ComponentStorage`.
