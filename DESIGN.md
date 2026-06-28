# Frame Engine Design Notes

A living document for Frame Engine itself. It covers engine and editor concerns only.

## What Frame Engine is

Frame Engine is a simulation engine written in Rust. Its job is to advance a world state forward in fixed, deterministic time steps. It is not tied to any one project that uses it.

## Core principles

Simulation is separate from rendering. The simulation is pure logic over state. It knows nothing about windows, graphics, or how the world is drawn. A renderer is a separate layer that reads simulation state and draws it. That separation is what lets the simulation run headless on a server, and it lets the renderer change without touching the engine.

Headless by default. The simulation runs with no window. That is its primary home: authoritative servers. Rendering is an optional layer on top.

Deterministic, fixed-timestep simulation. One tick always represents the same slice of simulated time, regardless of how fast the hardware runs. The same inputs produce the same outputs on any machine. This is a requirement for networked simulation across machines.

Reusable. The engine is its own crate, kept separate from any tool or application that depends on it. Dependencies point inward. Tools and applications depend on the engine. The engine depends on nothing above it.

## Workspace layout

The repository is a Cargo workspace holding multiple crates under `crates/`:

- `crates/frame-engine/` is the engine. It is both a library (the reusable code, exposed through `lib.rs`) and a binary (a thin `main.rs` that drives the tick loop). Other crates import the library. The binary is one specific way of running it.
- `crates/frame-editor/` is a separate authoring tool. It depends on `frame-engine` through a path dependency and reaches into the engine's types to view, drive, and edit world state. The dependency runs one way. The engine knows nothing about the editor.

Keeping both in one repo means the engine and editor move together: one `Cargo.lock`, one history, a shared `target/`. They still build as separate targets (`cargo build -p frame-engine` or `cargo build -p frame-editor`), so a headless server build and an editor build stay cleanly separable.

### Engine modules (`crates/frame-engine/src/`)

- `core/` holds the fixed-timestep clock that decides when ticks are owed. It lives in `core/clock.rs` and is used by both the engine binary and the editor.
- `world/` holds simulation state: entities, their components, and the storage that holds them. Data, not logic, not drawing.
- `systems/` holds logic that runs each tick and changes world state, such as movement and input-driven movement.
- `input/` holds a logical input abstraction: which buttons are currently held, expressed as engine-neutral `Button` values with no dependency on any windowing library. The editor translates platform key events into these logical buttons; systems read them. This is what lets input drive the simulation without the engine knowing anything about keyboards or `winit`.
- `render/` holds a headless, read-only ASCII view of the world. It is kept separate from the simulation so it can be ignored or replaced. The editor has its own GPU renderer (see Tooling), so this module is now the engine's built-in, dependency-light debug view rather than the only way to see a world.

The flow each frame is: `world` holds state, `systems` change that state each tick (reading `input` where relevant), `render` draws it, and `core` drives the loop.

The folder structure is a convention, not a rule. Rust does not enforce it, so it can be reshaped as the real structure emerges.

## Tech decisions

Language: Rust, for the engine, the editor, and anything built on top of them. One language means no FFI seam, and compile-time safety lands on the riskiest code, which is the long-lived server simulation.

Architecture: a hand-rolled, ECS-style data layout. Component data is stored in parallel lists indexed by entity ID rather than as objects that own their data. An entity is just an index. Its data lives in per-component lists, and systems sweep those lists in bulk each tick. It is built by hand rather than with an ECS crate so the data layout stays under our own control and is fully understood. Some components are markers: zero-field tags whose mere presence is the data. An entity is "controlled" if it carries the `Controlled` marker, with no value attached. A system then operates on every entity that has a given combination of components, for example everything that has both a position and the controlled marker. That "query by component signature" is how behaviour attaches to entities here, in place of scripts bolted onto individual objects.

Build versus buy: hand-roll the parts that are the heart of the project and worth understanding deeply, which is the engine and its world, systems, and clock. Use existing libraries for solved problems that are not the heart. In the editor that means `winit` for windowing, `wgpu` for the GPU API, `glam` for linear algebra, `bytemuck` for casting plain structs into the byte slices the GPU expects, `pollster` for blocking on wgpu's async setup, `image` for decoding the embedded logo, and `egui` for the editor's panel UI. These are load-bearing, but they are not where the learning or the differentiation lives.

Editor viewport rendering: the editor renders on the GPU through `wgpu`, in real 3D, with a perspective camera, instanced cube geometry, and a depth buffer for correct occlusion. This replaced an earlier `softbuffer` CPU-pixel view. That early viewport did its job as a dependency-light first window and proved the cross-crate link and the camera, pick, and inspector concepts in 2D. Real 3D needs a GPU pipeline, so the editor moved to wgpu once those concepts held up. The engine was untouched by that migration. All of it lives in the editor crate, on the far side of the simulation and rendering line.

Engine purity: the engine crate pulls in no rendering or windowing libraries. Everything graphics-related (`wgpu`, `glam`, `winit`, `egui`, `image`) lives in the editor. The engine is plain data and logic. Note that "graphics-free" is the line, not "no appearance data": the engine carries per-entity colour as component data because colour is part of the simulated world state, but it still owns no code that draws.

Editor of choice: Zed.

## Open questions

Per-entity appearance. Per-entity colour and scale now exist as component data: each entity carries a `Color` and a uniform `Scale`, both edited in the inspector and serialized with the scene. Appearance that varies per entity lives as component data on the world, not as editor-side metadata, and `Scale` followed exactly that path: a component field on the world, a per-entity input to the shader, and a matching adjustment in picking so the hit-box grows with the cube. Mesh and material are still open, and would follow the same path if they become per-entity data.

Lighting and materials. Shading is a single hard-coded directional term: one light vector, per-face brightness, just so cubes read as solid. There is no real lighting model, no shadows, no textures. That is fine for a debug viewport. A real look is a later, separate concern.

Picking is approximate. Selection projects each entity's centre and a corner to the screen and does a rectangle hit-test against the cursor. It is a screen-space bounding box, not a true ray-versus-geometry pick. The box now scales with the entity's `Scale`, so a resized cube stays clickable, but it is still cheap and good enough while every entity is a cube. It may want tightening once geometry varies beyond scaled cubes.

Two renderers now exist: the engine's headless ASCII view and the editor's wgpu viewport. The ASCII view is still useful as a zero-dependency headless check. Whether it stays long-term or retires once the editor is the canonical viewer is open.

How far to take component storage. The current direction is a fixed set of named component fields on `World`. A later, larger step is fully generic runtime component registration through type-erased storage, built on top of the current storage when many component types are needed.

Gap-storage tradeoffs at scale. The `Vec<Option<T>>` layout leaves holes for despawned entities. Spawn now reuses the first freed hole rather than always growing, which keeps the lists from running away, but iteration still visits holes. Sparse sets or a similar approach may be worth revisiting as entity counts grow.

Persistence and networking. Scene serialization exists (below). Multi-machine networking, authoritative-server persistence, and multi-zone architecture are designed at a high level only, not yet in engine code.

## Implemented so far (engine)

Fixed-timestep clock (`core/clock.rs`). An accumulator-based clock running at a fixed tick rate of 30 ticks per second. It separates simulation speed from hardware speed: every tick advances the sim by the same slice of time, so behaviour is deterministic across machines. A catch-up cap prevents the spiral of death, where after a hard stall the excess owed time is dropped rather than replayed. The clock is its own type with an `advance(running) -> owed_ticks` method, so each consumer (the engine binary, the editor) asks how many ticks it owes and runs that many. The logic used to be duplicated inline in two places. It moved into `core/` once a second consumer made the duplication real.

Hand-rolled ECS world (`world/`). Entities are indices. Component data lives in per-component lists indexed by entity ID, where `Some` means the entity has that component and `None` means it does not. There are five component types: `Position` (x, y, z) and `Velocity` (dx, dy, dz), both plain data; `Color` (r, g, b) and `Scale` (a uniform size factor), both per-entity appearance; and `Controlled`, a zero-field marker whose presence flags an entity as driven by input. The z axis is present from the start, so verticality such as floors and height is native rather than bolted on later. The world supports runtime spawn and despawn. Spawn reuses the first freed slot rather than always growing the lists, so ids stay stable and freed slots are reclaimed; despawn clears an entity's slots while the index stays valid.

Generic component storage (`world/storage.rs`). A reusable `ComponentStorage<T>`, a private `Vec<Option<T>>` with `new`, `insert`, `get`, `get_mut`, `remove`, `len`, `iter`, and `iter_mut`, so every component type uses the same tested storage code. It is wired into `World`: all five component lists are `ComponentStorage<T>`, and systems and the editor walk them through the storage's methods rather than touching the underlying `Vec`. `ComponentStorage<T>` implements `Default`, and `World` derives `Default`, so a fresh, empty world is constructed in exactly one place. Adding a component type is adding one storage field, and nothing about construction has to change. This is also the seam the editor's inspector reads and writes through.

Systems in their own module (`systems/`). Simulation logic lives in named functions rather than inline in the tick loop. The movement system reads each entity's velocity and applies it to its position every tick. The input system (`input_movement`) drives every entity that has both a position and the `Controlled` marker, moving it according to the currently held buttons. Both follow the same shape: walk the entities that match a component signature, and act on them. The tick loop calls systems rather than spelling out the logic, so adding behaviour means adding a system and a call, not editing the loop.

Logical input (`input/`). A graphics-free `InputState` records which logical `Button`s (up, down, left, right) are currently held. It depends on no windowing library: the editor maps platform key events onto these neutral buttons, and the engine reads them. This keeps input on the engine side of the simulation/rendering line without the engine knowing anything about keyboards or `winit`, and it is what the `input_movement` system consumes each tick.

Scene serialization (`serde` and RON). The `World` serializes to and from a human-readable RON file on disk, including per-entity colour and scale. Scenes saved before a field existed still load: the missing field defaults rather than failing, so older files stay readable. This is the persistence primitive that scene authoring builds on, and it is now wired into the editor as save and load.

Read-only ASCII debug renderer (`render/`). Draws the world as a grid each frame, mapping entity x and y onto cells. It is read-only by design, taking `&World` and never `&mut`, which enforces the simulation and rendering separation. Render rate is independent of tick rate.

Library and binary split (`lib.rs` and `main.rs`). The engine is both a library (the reusable code exposed through `lib.rs`) and a binary (a thin `main.rs` that runs the tick loop and consumes the library the same way any external crate would). This is what makes the engine importable. The editor is the second consumer.

`Position`, `Velocity`, `Color`, and `Scale` derive `Copy`. They are plain value types, so the editor can read a component out of the world, edit a copy in a panel, and write it back without holding a borrow on the world across the UI code.

## Tooling

`frame-editor` (`crates/frame-editor/`) is a separate binary crate that depends on the engine library. It is a 3D viewer and editor of a live world. It opens a native window, runs the engine's simulation under its own control, renders the world on the GPU, and lets you navigate, inspect, select, and edit entities through a docked panel layout.

What the editor does today:

Native window via `winit`, using the modern `ApplicationHandler` event-loop model. The editor owns its application state (an `App` holding the window, the GPU state, the engine `World`, the clock, the camera, the selection, the held input, and the panel state) and runs its own event-driven loop. That loop is distinct from the engine's headless tick loop, because an editor controls when to step the simulation rather than ticking forever.

Clean shutdown. GPU and window resources are released while winit's platform connection is still alive, on the way out, rather than being dropped after the event loop has already torn down. On Wayland the wgpu surface's destructor touches platform objects, so dropping it too late was a segfault on close. Releasing in order (GPU state, then egui, then the window) fixes it.

GPU rendering via `wgpu`. The viewport is drawn by the graphics card. Entities use instanced cube geometry: the cube's 36 vertices are generated in the shader from the vertex index, and a per-entity instance buffer (position, colour, a selected flag, and a scale factor) places one cube per entity, so all entities draw in one call. A second, screen-space pipeline draws the controls overlay. Both pipelines run in a single render pass, with egui's own render pass layered on top.

Real 3D with perspective and depth. The camera builds a view-projection matrix with `glam` (`look_at_rh` composed with `perspective_rh`), so the scene has true perspective: nearer entities are larger, and the z axis the simulation always had becomes visible depth. A 32-bit depth buffer, recreated on resize, does per-pixel depth testing, so nearer geometry occludes farther geometry regardless of draw order. That is what makes a cube look solid, and it is what makes overlapping entities sort correctly. Per-face directional shading gives each cube light and shadow so it reads as 3D.

Orbit, pan, and zoom camera. The camera orbits a focus point at an adjustable distance, with yaw and pitch. Left-drag pans the focus point. Scroll zooms by changing the orbit distance. Middle-drag orbits, sweeping yaw and pitch, with pitch clamped just short of the poles so the up vector never degenerates.

Click-to-pick selection. Left-click selects an entity. The editor forward-projects each entity through the same view-projection matrix to screen pixels and hit-tests the cursor against its projected box. The selected entity is highlighted by brightening it toward white rather than recolouring it, so its own colour stays readable while you edit it. `Escape` clears the selection.

Live entity editing. The selection can be moved, spawned, and despawned. Arrow keys nudge the selected entity on X and Y, Page Up and Page Down on Z, `N` spawns a new entity at the camera focus and selects it, and `Delete` despawns the selection. While the sim is playing, an entity carrying the `Controlled` marker is driven directly by the WASD keys, through the engine's input system. These run against the engine's world directly.

Scene save and load (`F5` and `F9`). The editor loads a scene from a RON file on startup, and falls back to a default scene if none exists. `F5` saves the current world and `F9` reloads it. This is the engine's serialization wired into an authoring flow.

Docked panel UI via `egui`. The editor uses a docked layout rather than a bare viewport:

- A top toolbar showing the editor's logo, with placeholder File, Edit, View, and Help menus. The logo is the embedded PNG, uploaded once as an egui texture. The editor also sets a window and taskbar icon from the same image, which is honoured on X11 and Windows; on Wayland the desktop reads the icon from a matching `.desktop` file instead.
- A right inspector dock with two tabs. The Scene tab lists every live entity and lets you select one by clicking it. The Inspector tab shows the selected entity's position and velocity as draggable number fields, a colour picker for its colour, a draggable field for its scale, and a checkbox for whether it is `Controlled` (WASD-driven). Every edit writes straight back into the world.
- A bottom console dock with an Output tab that shows a live log of editor actions, plus a Terminal placeholder for later.
- Panels are solid but resizable. Dragging a panel's inner edge resizes it, and the viewport reflows around it.

The viewport fills the space the panels leave. Input is routed so the camera and picking only respond when the cursor is over the viewport, not when it is over a panel. WASD is read regardless of panel focus, so driving a `Controlled` entity keeps working even while a panel widget holds keyboard focus.

Controls overlay. A bottom-left list of keybindings, drawn with a hand-rolled 5x7 bitmap font through the screen-space overlay pipeline. `H` toggles it. This is the last remaining hand-rolled text overlay. The older on-screen readout of the selected entity's values was retired once the Inspector panel took over that job.

Runs the simulation live on the engine's clock. The editor advances the world by calling the engine's systems, driven by the engine's `core` clock, so the sim runs at a true 30 ticks per second independent of the window's repaint rate. Pausing stops the simulation but not the rendering. `Space` toggles pause, `.` steps one tick while paused (kept off `S` so it does not clash with the WASD drive keys), and the viewport keeps redrawing the frozen state so you can navigate a paused world. Pause is a simulation concern. The renderer draws whatever state exists.

What's next for the editor, in rough order:

- Wire the toolbar and File menu to the actions that already work by keyboard, so save, load, play, pause, and step have a visible counterpart.
- Richer authoring: gizmos to drag an entity in the viewport, undo and redo, and prefabs, added as real use of the engine demands them.
- Further per-entity appearance. Colour and scale exist now; mesh and material are the next steps so authored scenes can look like more than coloured, resized cubes (see Open questions).
- Movable, dockable panels. The current panels are fixed in place. A later step is letting them be rearranged and split, the way a full IDE layout works.
