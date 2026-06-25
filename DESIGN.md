# Frame Engine — Design & Tech Notes

*A living document for Frame Engine itself. Engine concerns only — game-specific design lives with the game that uses the engine.*

---

## What Frame Engine is

A custom **simulation engine** written in Rust. Its job is to advance a world state forward in fixed time steps, deterministically. It is not tied to any one game.

## Core principles

- **Simulation is separate from rendering.** The simulation is pure logic over state — it knows nothing about windows, graphics, or how the world is drawn. A renderer is a separate layer that *reads* simulation state and draws it. This is what allows the simulation to run headless (on servers) and lets the renderer be swapped without touching the engine.
- **Headless by default.** The simulation must run with no window — that is its primary home (authoritative servers). Rendering is an optional layer added on top.
- **Deterministic, fixed-timestep simulation.** One tick always represents the same slice of simulated time, independent of how fast the hardware runs. Same inputs → same outputs on any machine. (Required for networked, multi-machine simulation.)
- **Reusable.** The engine is its own crate, kept structurally separate from any game or tool that depends on it. Dependencies point *inward*: tools and games depend on the engine; the engine depends on nothing above it.

## Workspace layout

The repository is a Cargo workspace (a monorepo) holding multiple crates under `crates/`:

- `crates/frame-engine/` — the engine itself. Both a **library** (its reusable guts, exposed via `lib.rs`) and a **binary** (a thin `main.rs` runner that drives the tick loop). The library is what other crates import; the binary is one specific way of running it.
- `crates/frame-editor/` — a separate authoring tool that **depends on** `frame-engine` (path dependency). It reaches into the engine's types to view, drive, and edit world state. The engine knows nothing about the editor — the dependency is one-directional.

Keeping everything in one repo means the engine and editor evolve in lockstep (one `Cargo.lock`, one history, shared `target/`), while still building as separate targets (`cargo build -p frame-engine` vs `-p frame-editor`) — so a headless server build and an editor build are cleanly separable.

### Engine module layout (`crates/frame-engine/src/`)

- `core/` — the heartbeat: the fixed-timestep clock that decides when ticks are owed. Now its own module (`core/clock.rs`), consumed by both the engine binary and the editor.
- `world/` — simulation *state*: entities, their components, and the storage that holds them. Numbers, no logic, no drawing.
- `systems/` — logic that runs each tick and *changes* world state (movement, AI, etc.).
- `render/` — a headless, read-only ASCII debug view of the world. Deliberately separate from the simulation so it can be ignored or replaced freely. (The editor has its own, much richer GPU renderer — see Tooling — so this module is now the engine's *built-in, dependency-light* debug view rather than the only way to see a world.)

Flow: `world` (state) → `systems` (change state each tick) → `render` (draw state), with `core` driving the loop.

*(Folder structure is conventional, not sacred — Rust doesn't enforce it, so it can be reshaped cheaply as the real structure emerges.)*

## Tech decisions

- **Language:** Rust, for engine, editor, and eventual game (one language, no FFI seam; compile-time safety lands on the riskiest code — the long-lived server simulation).
- **Architecture pattern:** Hand-rolled ECS-style data layout — component data stored in parallel lists indexed by entity ID, rather than as objects that own their data. An entity is just an index; its data lives in per-component lists, and systems sweep those lists in bulk each tick. Built by hand rather than using an ECS crate, to keep the data layout under our own control and understand it fully.
- **Build vs. buy:** hand-roll what is the heart of the project and worth understanding deeply (the engine — its world, systems, and clock). Use existing libraries for solved problems that aren't the heart. In the editor that means `winit` (cross-platform windowing), `wgpu` (the GPU API), `glam` (linear algebra — matrices and vectors), `bytemuck` (casting plain structs to the byte slices the GPU wants), and `pollster` (blocking on wgpu's async initialisation). These are load-bearing but not where the learning or the differentiation lives.
- **Editor viewport rendering:** the editor renders on the **GPU via `wgpu`**, in real 3D — a perspective camera, instanced cube geometry, and a depth buffer for correct occlusion. This replaced an earlier `softbuffer` CPU-pixel debug view. That softbuffer viewport did its job as a dependency-light first window — it proved the cross-crate link and the camera/pick/inspector concepts in 2D — but real 3D (perspective projection plus depth testing) needs a GPU pipeline, so the editor migrated to wgpu once those concepts were proven. The engine stayed completely untouched by that migration: all of it lives in the editor crate, on the far side of the simulation/rendering line.
- **Engine purity:** the engine crate pulls in no rendering or windowing libraries at all. Everything graphics-related — `wgpu`, `glam`, `winit` — lives in the editor crate. The engine is plain data and logic.
- **Editor:** Zed.

## Open questions / not yet decided

- **Per-entity appearance.** Every entity currently renders as the same yellow cube (orange when selected). Per-entity mesh, colour, scale, or material is not yet a thing — entities carry simulation data, not appearance data. When the game needs distinct-looking entities, appearance becomes either component data on the world or editor-side metadata; which, is open.
- **Lighting / materials.** Shading is a single hard-coded directional term (one light vector, per-face brightness) purely so cubes read as solid. There is no real lighting model, no shadows, no textures. Fine for a debug viewport; a real look is a later, separate concern.
- **Entity picking is approximate.** Selection projects each entity's centre and a corner to the screen and does a rectangle hit-test against the cursor — a screen-space bounding box, not a true ray-vs-geometry pick. It's cheap and good enough while every entity is a uniform cube; it may want tightening (real ray casting, depth-aware pick) once geometry varies.
- **Two renderers now exist.** The engine's headless ASCII `render/` view and the editor's wgpu viewport. The ASCII view is still useful as a zero-dependency headless sanity check, but its long-term role (keep, or retire once the editor is the canonical viewer) is open.
- **How far to take generic component storage** — a fixed set of named component fields on `World` (current direction) vs. fully generic runtime component registration via type-erased storage (a later, larger step, built on top of the current generic storage when many component types are needed).
- **Gap-storage tradeoffs at scale** — the `Vec<Option<T>>` layout leaves holes for despawned entities; sparse sets or similar may be worth revisiting as entity counts grow.
- **Server-side persistence and networking** — scene serialization exists (see below), but multi-machine networking, authoritative-server persistence, and multi-zone architecture are designed only at a high level, not yet engine code.

## Implemented so far (engine)

- **Fixed-timestep clock** (`core/clock.rs`): an accumulator-based clock running at a fixed `TICK_RATE` (30 ticks/sec). It decouples simulation speed from hardware speed — every tick advances the sim by an identical slice of time, so behaviour is deterministic across machines. A catch-up cap (`MAX_CATCHUP_TICKS`) prevents the spiral of death: after a hard stall, excess owed time is dropped rather than replayed. The clock is its own type with an `advance(running) -> owed_ticks` method, so each consumer (the engine binary, the editor) just asks "how many ticks do I owe?" and runs that many. This used to be duplicated inline in two places; it moved into `core/` once a second consumer made the duplication real — "no premature abstraction" playing out exactly as intended.

- **Hand-rolled ECS world** (`world/`): entities are indices; component data lives in per-component lists indexed by entity ID (`Some` = entity has that component, `None` = it doesn't). Two component types — `Position` (x/y/z; the z axis is present from the start so verticality like floors, bunkers, and mining is native rather than bolted on later) and `Velocity` (dx/dy/dz). The world supports runtime **spawn** and **despawn** (clearing an entity's slots; the index stays valid, preserving stable IDs).

- **Generic component storage** (`world/storage.rs`): a reusable `ComponentStorage<T>` (a private `Vec<Option<T>>` plus `new`/`insert`/`get`/`remove`/`len` and `iter`/`iter_mut`), so each component type uses the same tested storage code. **Wired into `World`** — both `positions` and `velocities` are `ComponentStorage<T>`, and systems and renderers walk them through the storage's iteration methods rather than touching the underlying `Vec`. Adding a component type is now adding one storage field. This is also the seam the editor's inspector reads through (via `get`).

- **Systems, in their own module** (`systems/`): simulation logic lives in named functions, not inline in the tick loop. The movement system reads each entity's velocity and applies it to its position every tick. The tick loop *calls* systems rather than spelling out the logic — adding behaviour means adding a system and a call, not touching the loop.

- **Scene serialization** (`serde` + RON): the `World` can be serialized to and from a human-readable RON file on disk. This is the persistence primitive scene authoring is built on — wiring it into the editor as a save/load flow is the next editor step (see Tooling).

- **Read-only ASCII debug renderer** (`render/`): draws the world as a grid each frame, mapping entity x/y onto cells. Read-only by design (takes `&World`, never `&mut`), enforcing the simulation/rendering separation. Render rate is decoupled from tick rate.

- **Library / binary split** (`lib.rs` + `main.rs`): the engine is both a library (reusable guts exposed via `lib.rs`) and a binary (a thin `main.rs` that runs the tick loop, consuming the library exactly as any external crate would). This is what makes the engine *importable* by other crates. The editor is the second consumer.

## Tooling

**frame-editor** (`crates/frame-editor/`) is a separate binary crate that depends on the engine library. It is a real **3D viewer of a live world**: it opens a native window, runs the engine's simulation under its own control, renders the world on the GPU, and lets you navigate, inspect, and select entities.

What the editor does today:

- **Native window via `winit`** using the modern `ApplicationHandler` event-loop model. The editor owns its application state (`App` holds the window, the GPU state, the engine `World`, the clock, the camera, and the current selection) and runs its own event-driven loop — distinct from the engine's headless tick loop, because an editor wants to control *when* to step the simulation rather than tick forever.

- **GPU rendering via `wgpu`.** The viewport is drawn by the graphics card. Entities are drawn with **instanced cube geometry**: the cube's 36 vertices are generated in the shader from the vertex index, and a per-entity instance buffer (position + a selected flag) places one cube per entity — so all entities are one draw call. A second, screen-space pipeline draws the inspector overlay (below). Both pipelines run in a single render pass.

- **Real 3D: perspective + depth.** The camera builds a view-projection matrix with `glam` (`look_at_rh` × `perspective_rh`), so the scene has genuine perspective — nearer entities are larger, and the z axis the simulation always had is finally visible as real depth. A **depth buffer** (32-bit depth texture, recreated on resize) does per-pixel depth testing, so nearer geometry correctly occludes farther geometry regardless of draw order. This is what makes a cube look solid (its own front faces hide its back faces) and what makes overlapping entities sort correctly. Per-face directional shading gives each cube light and shadow so it reads as 3D.

- **Orbit / pan / zoom camera.** The camera orbits a focus point at an adjustable distance, with yaw and pitch angles:
  - **left-drag** pans the focus point,
  - **scroll** zooms (changes the orbit distance multiplicatively),
  - **middle-drag** orbits (sweeps yaw/pitch; pitch is clamped just short of the poles so the up vector never degenerates).

- **Click-to-pick selection + inspector.** Left-click selects an entity: the editor forward-projects each entity through the same view-projection matrix to screen pixels and hit-tests the cursor against its projected box. The selected entity is highlighted (drawn orange instead of yellow), its data printed to the console, and — the newer capability — drawn **on screen**. The inspector text uses a hand-rolled 5×7 bitmap font: each lit font pixel becomes a small screen-space quad positioned directly in NDC, fed through the overlay pipeline. That pipeline ignores the camera and ignores depth (always draws on top), so the inspector is screen-anchored furniture that stays put as you pan, zoom, and orbit. `Escape` clears the selection.

- **Runs the simulation live, on the engine's clock.** The editor advances the world by calling the engine's movement system, driven by the engine's own `core` clock — so the sim runs at a true 30 ticks/sec **independent of the window's repaint rate**, honouring the determinism principle inside the editor. **Pausing stops the simulation but not the rendering:** `Space` toggles pause, `S` steps exactly one tick while paused, and the viewport keeps redrawing the (frozen) state so you can still navigate a paused world. Pause is a *simulation* concern; the renderer just draws whatever state exists — the same separation the whole engine is built on.

What's next for the editor (in rough order):

- **Scene save/load wiring** (Tier 2): the engine can already serialize a `World` to RON; the editor needs the authoring flow on top — load a scene into the live world, edit it (move/spawn/despawn via the selection that already exists), save it back. The inspector and picking that this needs are now in place.
- **Richer authoring** (Tier 3): gizmos (drag an entity in the viewport), undo/redo, prefabs — to accrete gradually as the game that uses the engine demands them.
- **Per-entity appearance** so authored scenes can look like something other than identical cubes (see Open questions).
