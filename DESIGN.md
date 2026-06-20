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
- `crates/frame-editor/` — a separate authoring tool that **depends on** `frame-engine` (path dependency). It reaches into the engine's types to view, drive, and (eventually) edit world state. The engine knows nothing about the editor — the dependency is one-directional.

Keeping everything in one repo means the engine and editor evolve in lockstep (one `Cargo.lock`, one history, shared `target/`), while still building as separate targets (`cargo build -p frame-engine` vs `-p frame-editor`) — so a headless server build and an editor build are cleanly separable when real builds arrive.

### Engine module layout (`crates/frame-engine/src/`)

- `core/` — the heartbeat: the tick loop and fixed-timestep clock. (Currently the loop still lives in `main.rs`; extracting it into `core/` is a pending refactor — see Open questions.)
- `world/` — simulation *state*: entities, their components, and the storage that holds them. Numbers, no logic, no drawing.
- `systems/` — logic that runs each tick and *changes* world state (movement, AI, etc.).
- `render/` — reads world state and draws it. Deliberately separated so it can be replaced freely.

Flow: `world` (state) → `systems` (change state each tick) → `render` (draw state), with `core` driving the loop.

*(Folder structure is conventional, not sacred — Rust doesn't enforce it, so it can be reshaped cheaply as the real structure emerges.)*

## Tech decisions

- **Language:** Rust, for engine, editor, and eventual game (one language, no FFI seam; compile-time safety lands on the riskiest code — the long-lived server simulation).
- **Architecture pattern:** Hand-rolled ECS-style data layout — component data stored in parallel lists indexed by entity ID, rather than as objects that own their data. An entity is just an index; its data lives in per-component lists, and systems sweep those lists in bulk each tick. Built by hand rather than using an ECS crate, to keep the data layout under our own control and understand it fully.
- **Build vs. buy:** hand-roll what is the heart of the project and worth understanding deeply (the engine — its world, systems, and clock). Use existing libraries for solved problems that aren't the heart: `winit` for cross-platform windowing in the editor, and `softbuffer` for getting a CPU-filled pixel buffer onto that window. These are load-bearing but not where the learning or the differentiation lives.
- **Editor viewport rendering:** the editor draws into a **CPU pixel buffer** (`softbuffer`) — it fills a flat `u32` framebuffer by hand and presents it. This is a deliberately simple, dependency-light choice for a *debug* viewport. It is not a GPU renderer; a real 3D renderer (likely `wgpu`) is a separate future chapter (see Open questions).
- **Editor:** Zed.

## Open questions / not yet decided

- **The real renderer.** The editor's current viewport is a 2D `softbuffer` debug view with a *fake-depth cue* (see Tooling) — not real 3D. A proper renderer with perspective projection, a depth buffer, and GPU pipeline (Bevy vs. a hand-rolled `wgpu` stack) is deferred to its own deliberate chapter. The engine is *ready* for it — the simulation is already full 3D data (`Position` carries z) — so the cost is entirely in the rendering stack, not engine rework.
- **Extracting the fixed-timestep clock into `core/`.** The accumulator-based clock now exists in **two** places — the engine binary (`main.rs`) and the editor — because the editor needs to own *when* it ticks. That duplication is the natural signal that the clock wants to become shared `core/` code both consume. Held deliberately: one duplicate isn't enough pain yet (per "no premature abstraction"); the refactor happens when a third consumer or real friction appears.
- **How far to take generic component storage** — a fixed set of named component fields on `World` (current direction) vs. fully generic runtime component registration via type-erased storage (a later, larger step, to be built on top of the current generic storage when many component types are needed).
- **Gap-storage tradeoffs at scale** — the `Vec<Option<T>>` layout leaves holes for despawned entities; sparse sets or similar may be worth revisiting as entity counts grow.
- Networking, persistence, and multi-zone server architecture — designed at a high level for the target game, but not yet engine code.

## Implemented so far (engine)

- **Fixed-timestep tick loop** (`core` concern, currently in `main.rs`): an accumulator-based loop running at a fixed `TICK_RATE` (30 ticks/sec). Decouples simulation speed from hardware speed — every tick advances the sim by an identical slice of time, so behaviour is deterministic across machines. A catch-up cap (`MAX_CATCHUP_TICKS`) prevents the spiral of death: after a hard stall, excess owed time is dropped rather than replayed.

- **Hand-rolled ECS world** (`world/`): entities are indices; component data lives in per-component lists indexed by entity ID (`Some` = entity has that component, `None` = it doesn't). Currently two component types — `Position` (x/y/z; the z axis is included from the start so verticality like floors, bunkers, and mining is native rather than bolted on later) and `Velocity` (dx/dy/dz). The world supports runtime **spawn** (create an entity) and **despawn** (clear an entity's slots; the index stays valid, preserving stable IDs).

- **Generic component storage** (`world/storage.rs`): a reusable `ComponentStorage<T>` type (a private `Vec<Option<T>>` plus `new`/`insert`/`get`/`remove`/`len` and `iter`/`iter_mut`) so each component type uses the same tested storage code instead of a hand-written `Vec<Option<...>>` per type. **Wired into `World`** — both `positions` and `velocities` are `ComponentStorage<T>`, and systems and renderers walk them through the storage's iteration methods rather than touching the underlying `Vec` directly (the inner `Vec` is private; access goes through a controlled API). Adding a new component type is now adding one storage field rather than repeating the storage pattern by hand. This is also the seam the editor's inspector will plug into (via `get`).

- **Systems, in their own module** (`systems/`): simulation logic lives in named functions, not inline in the tick loop. The movement system reads each entity's velocity and applies it to its position, advancing every entity each tick. The tick loop *calls* systems rather than spelling out the logic — so adding behaviour means adding a system and a call, not touching the loop.

- **Debug renderer** (`render/`): a read-only ASCII view that draws the world as a grid each frame, mapping entity x/y onto grid cells. Read-only by design (takes `&World`, never `&mut`), enforcing the simulation/rendering separation. Render rate is decoupled from tick rate (the sim ticks every step; the view redraws every Nth tick) — a small first instance of the principle that simulation rate and render rate are independent.

- **Library / binary split** (`lib.rs` + `main.rs`): the engine is both a library (reusable guts exposed via `pub mod world; pub mod systems; pub mod render;` in `lib.rs`) and a binary (a thin `main.rs` that runs the tick loop, consuming the library exactly as any external crate would). This is what makes the engine *importable* by other crates. The binary is the first consumer of the library; the editor is the second.

## Tooling

**frame-editor** (`crates/frame-editor/`) is a separate binary crate that depends on the engine library. It has grown from "proves the cross-crate link" into a small but real **viewer of a live world**: it opens a native window, runs the engine's simulation under its own control, and lets you watch and navigate it. It is still a one-way mirror — it shows state but does not yet read individual entity data back to the user (the inspector is the next step).

What the editor does today:

- **Native window via `winit`** using the modern `ApplicationHandler` event-loop model. The editor owns its application state (`App` holds the window, the draw surface, the engine `World`, the clock state, and the camera) and runs its own event-driven loop — distinct from the engine's headless tick loop, because an editor wants to control *when* to step the simulation rather than tick forever.

- **CPU pixel rendering via `softbuffer`.** The editor fills a flat `u32` framebuffer by hand (background, then each entity) and presents it. **The earlier Wayland "invisible window" issue is resolved:** its root cause was that `winit` on Wayland does not surface a window until something is actually drawn into it — no pixels, no window. Once `softbuffer` fills and presents the buffer, the window appears natively; no X11 backend workaround is needed.

- **Draws the engine's entities.** Each frame it walks the engine `World`'s positions and draws each entity as a small square, mapping world coordinates to screen pixels through the camera (below). This is the simulation/rendering separation showing up across a crate boundary: the editor *reads* `&World` and draws it; it never owns the state.

- **Runs the simulation live, on its own fixed-timestep clock.** The editor calls the engine's movement system to advance the world, driven by an accumulator-based clock that mirrors the engine binary's — so the sim runs at a true 30 ticks/sec **independent of the window's repaint rate**. This honours the engine's determinism principle inside the editor (a naive "tick once per rendered frame" approach would couple sim speed to framerate, which the fixed-timestep clock exists to forbid). Note: this clock is currently duplicated between engine and editor — see Open questions.

- **Playback controls: play / pause / step.** `Space` toggles pause; `S` steps the simulation exactly one tick while paused. This is the line between a viewer and a tool — the editor decides *when* time advances. A deliberate detail: **pausing stops the simulation but not the rendering.** The render loop keeps drawing the (now frozen) state every frame, so the window stays responsive and you can still navigate a paused world. Pause is a *simulation* concern; the renderer just draws whatever state currently exists — the same separation the whole engine is built on, surfaced as a feature.

- **Pan/zoom camera.** The viewport has a camera: a world point it looks at (its centre) and a zoom factor (pixels per world unit), both held as adjustable state on `App`. Projection is `screen = centre + (world − camera) × zoom`. Left-drag pans (the camera moves opposite the cursor, so the world follows the drag); the scroll wheel zooms multiplicatively about the screen centre. This is the same camera concept a 3D renderer needs, prototyped in 2D — and its inverse (`world = camera + (screen − centre) / zoom`) is what the coming inspector will use to turn a click into a world position for entity picking.

- **Fake-depth z cue.** The simulation has been full 3D since day one (`Position`/`Velocity` carry z/dz, and the movement system integrates z every tick) — but a flat 2D viewport couldn't *show* the z axis. The editor now treats the view as a camera looking *along* z: each entity's z modulates its square's **size and brightness** (nearer = bigger and brighter, farther = smaller and dimmer). This is honestly a *depth cue*, not real 3D: there is **no occlusion** (no depth buffer; entities draw in ID order, not z order) and z does **not** shift an entity's screen position (no perspective). It makes an axis we always had finally visible, cheaply, without committing to a GPU renderer. Real 3D (perspective projection, depth testing) remains the deferred `wgpu` chapter.

What's next for the editor (in rough order):

- **Inspector** (Tier 1): click an entity → select it → read its position/velocity back through `ComponentStorage::get`. First version: pick + highlight + console print. In-viewport text rendering (drawing the numbers on screen) is a genuinely new capability deferred to a follow-up.
- **Scene authoring with save/load** (Tier 2): serialize `World` to/from disk (`serde` + RON), so scenes can be authored and reloaded. Wants the inspector first — you want to see and select what you're authoring before you save it.
- Later (Tier 3): gizmos, undo/redo, prefabs — to accrete gradually.
