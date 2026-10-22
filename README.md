# Frame Engine

A simulation engine written in Rust, with a companion 3D editor.

Frame Engine advances a world state forward in fixed, deterministic time steps. It is a simulation engine, not a renderer. The simulation runs headless with no window required, and rendering is a separate, swappable layer on top. That separation lets the engine run on servers and be reused across different games and tools.

## Forking and contributing

Frame Engine is built in the open and meant to be learned from. If it looks useful, fork it, clone it, and bend it to whatever you are making. The Mozilla Public License 2.0 means you can use it freely, for any purpose, including in closed commercial games, with no permission asked. The one thing it asks is that if you change the engine's own files and share the result, you share those changes under the same licence.

A few things worth knowing before you dig in:

- **It is hand-rolled on purpose.** The engine's world, systems, and clock are written by hand rather than pulled from an ECS crate, so the code is meant to be read and understood top to bottom rather than treated as a black box. Reading [DESIGN.md](DESIGN.md) first is the fastest way in: it explains not just what each piece does but why it is shaped that way.
- **The engine stays graphics-free.** `crates/frame-engine` depends on `serde` and `ron` for scenes and `rapier3d` for physics, and on nothing else. All windowing, GPU, and UI code lives in `crates/frame-editor`. If you extend the engine, keeping that line clean is the one constraint worth respecting, because it is what lets the simulation run headless on a server.
- **Dependencies point one way.** The editor depends on the engine, never the reverse. The engine knows nothing about the editor or any application built on it.
- **`main` is the working branch.** It is where features land and can be unstable. Fork from `main` if you want to hack on the latest; depend on a tagged release if you want something steady (see below).

If you build something the engine is missing, a system, a component, a piece of the editor, and you think it fits, get in touch. Open an issue to talk it through, or a pull request if you have already built it. Nothing is guaranteed to land, but good work that fits the project's direction is genuinely welcome, and if it suits the engine it may well make it into the main public build.

Built something with it, or forked it into your own thing? Tag me on X at [@OuterFrameInter](https://x.com/OuterFrameInter), I would love to see it, and that is also the easiest way to reach me if GitHub is not your preferred route.

There is also a Discord server for bug reports, showing off what you have built, and following development: https://discord.gg/ufXDNJnzgf

Frame Engine is a learning project as much as a tool: I am still growing my own Rust as I build the engine and editor in the open, which is part of why the code is written to be read rather than to show off. Considered feedback, on the engine, the editor, or the overall approach, is always welcome.

## Releases and stability

`main` is the working branch, where in-progress features land. It is under constant change and can be incomplete or unstable at any moment.

**For a stable build, download a tagged release rather than cloning `main`.** Releases are cut at points where the project is known to build and run, so a release is your dependable copy. Tagged releases are on the project's Releases page.

This repo restarted fresh with no tagged releases yet (see [SECURITY.md](SECURITY.md) for why). `Cargo.toml` currently sits at `0.0.0`, a placeholder for this pre-release baseline. The first tagged release will be `0.1.0`, planned for **Friday 30 October 2026 at 10pm UK time**. It will be a source release (a tag, release notes and build instructions), so you build it yourself with Cargo. The capabilities listed below reflect current `main`.

Releases come at most once a week. A crash, data loss or security bug gets a patch release (`0.1.1` and so on) as soon as it is fixed, without waiting. What has been tested, the known bugs and how to report a security issue are in [SECURITY.md](SECURITY.md).

## Status

Early development, past the toy stage: a working engine and a usable editor that a small game is being built on. The [CHANGELOG](CHANGELOG.md) has every change in detail; this is the short version of what is here and what is not yet.

### What it has

**Engine** (`crates/frame-engine`, no graphics)

- A deterministic fixed-timestep clock, 30 ticks a second, shared by everything that runs the world.
- A hand-rolled ECS: `ComponentStorage<T>` with a small query API, built-in components (position, velocity, colour, scale, full yaw/pitch/roll rotation, mesh, material, lights, sounds, cameras, parents, health), components a game adds by name (extension components), and world-wide resources.
- Everything in metres, with world positions in `f64`, so a world can be planet-sized without jitter far from the origin.
- Physics through `rapier3d`: static and dynamic bodies, fixed mesh colliders, and characters with their own body (`CharacterBody`: height, speeds, jump, step height, slope, crouch), a view that turns them, and swimming in liquids a game describes.
- Scenes, prefabs and projects saved as readable RON, with save-format and version checks so an older editor warns about newer files instead of breaking them.
- Rhai scripting per entity from a shared library: movement, physics characters, spawning, scene changes, globals across scenes.
- Models: a hand-rolled OBJ reader, plus models a game builds in code or reads itself. A model can carry shape keys and a skeleton with clips (`assets::skin`); each entity has its own shape key weights and its own playing clips (`Animation`).
- Pack files for game builds, with optional encryption a game provides.
- A first slice of multiplayer in `net/`: an authoritative server, clients that upload input, and a version handshake.

**Editor** (`crates/frame-editor`)

- A project launcher, scenes, a dockable panel layout (Scene tree, Inspector, Script Editor, Assets, Prefabs, Source Control), undo and redo, a translate gizmo, and unsaved-changes protection.
- A GPU renderer (`wgpu`): textured models with roughness and metalness, coloured point and directional lights, shadows from the sun, a sky with sun, moon, stars and moving clouds, distance fog and haze, and no far plane.
- Shape keys and skinned animation drawn on the graphics card, so many characters can share one model and each look and move their own way. Characters pick idle, walk, run, jump, fall and crouch clips from how they move.
- A Play window, and File > Build Game for Linux and Windows builds with the third-party licences included.
- A module interface (`module_api`) for a game to add its own systems, components, Inspector sections, models, collision, terrain or water drawing, menus and panels, without changing the editor.
- Plugins (Rhai scripts, menu actions and panels per project) and a performance overlay (F3).

### What it does not have yet

- **Releases you can just download.** 0.1.0 is a source release: build it with cargo. No installers.
- **Testing beyond one machine.** Built and tested on Linux (Pop!_OS, NVIDIA, Wayland). Windows lightly, with a known crash when docking a popped-out panel back. macOS not at all. See [SECURITY.md](SECURITY.md).
- **A glTF reader in the editor itself.** The editor reads OBJ. The model hooks are ready for `.glb` with shape keys and skeletons, but reading the file is up to a game's own module for now.
- **More animation:** clips that change shape keys (facial animation), turning the body to face the way it moves, aim and look, foot placement, and blending by body part.
- **Lighting:** shadows from point lights, and a fuller material model (normal maps, true view-dependent specular). One texture per model.
- **Terrain, water and grass built in.** The editor can draw them for a game that brings its own (through `module_api`), but has none of its own.
- **Multiplayer beyond a first slice:** no interest management or reconnecting yet.
- **Collision refinements:** true oriented boxes and joints.
- **Its own UI system** beyond on-screen text and images, and visual scripting.

## Principles

- **Simulation is separate from rendering.** The simulation knows nothing about how it is drawn. The editor reads the world and draws it across a crate boundary, and never owns the state. The engine crate pulls in no graphics libraries, and no script interpreter: it holds script source as data and leaves running it to the host, just as it leaves drawing to the host.
- **Headless by default.** Runs with no window. Rendering is optional and added on top.
- **Deterministic, fixed-timestep.** One tick is always the same slice of simulated time, so behaviour is identical across machines. The editor honours this with the engine's own clock rather than ticking once per rendered frame.
- **Reusable.** The engine is its own library crate, so it can power more than one game or tool. Dependencies point inward: tools depend on the engine, never the reverse.
- **Hand-roll the heart, buy the rest.** The engine is built by hand to be understood deeply. Solved problems that are not the heart (windowing, the GPU API, linear algebra, UI, file dialogs, config paths, dates, git, audio, scripting, physics) use existing libraries: `winit`, `wgpu`, `glam`, `egui` and `egui_dock`, `rfd`, `dirs`, `chrono`, `git2`, `kira`, `rhai`, `rapier3d` (the one such library the engine itself depends on), and `serde` with `ron` for scenes and the project manifest.
- **No premature abstraction.** Machinery is built when the pain is real, not before. The fixed-timestep clock stayed duplicated inline until a second consumer made the duplication real, then moved into `core/`.

## Workspace structure

This repository is a Cargo workspace holding multiple crates:

```
crates/
├── frame-engine/        the simulation engine (library and binary), no graphics
│   └── src/
│       ├── core/        the fixed-timestep clock
│       ├── world/       simulation state: entities, components, storage
│       ├── systems/     what runs each tick
│       ├── input/       graphics-free input (which buttons are held)
│       ├── net/         multiplayer: an authoritative Server and a Client
│       ├── assets/      models: the OBJ reader, shape keys, and skeletons
│       │                and clips (skin.rs)
│       ├── physics.rs   rapier3d bodies, characters and swimming
│       ├── project.rs   a project's scenes and the things they share
│       ├── pack.rs      a project's files, from its folder or a game's pack
│       ├── version.rs   the engine's version and comparing versions
│       ├── render/      read-only ASCII debug view
│       ├── lib.rs       library root, exposes the engine to other crates
│       └── main.rs      binary runner, drives the tick loop
└── frame-editor/        the editor (library and binary); depends on frame-engine
    ├── assets/
    │   ├── frame-editor.png   the editor's logo
    │   └── fonts/             Manrope, Fraunces and JetBrains Mono, embedded
    └── src/
        ├── lib.rs           the editor: app state, panels, camera, picking,
        │                    entity editing, and the wgpu renderer
        ├── main.rs          the public editor's program
        ├── module_api.rs    how a crate outside the editor adds a feature
        ├── locomotion.rs    characters picking clips from how they move
        ├── build.rs         File > Build Game
        ├── licences.rs      the third-party licences a game build carries
        ├── scenes.rs        a project's scenes in the editor
        ├── script.rs        the Rhai script runtime
        ├── game_audio.rs    sound a game's modules ask for in play
        ├── game_menu.rs     a game's own menu before play
        ├── sky.rs, sun.rs   the sky, fog, sun, moon and wind
        ├── grass.rs         dense grass made on the graphics card
        ├── origin.rs        drawing a world far from (0, 0, 0) precisely
        ├── perf.rs          the performance overlay
        ├── font.rs          a small bitmap font for overlays
        ├── scene.wgsl       what every pass drawing the world shares
        ├── shader.wgsl      the entity shader (shape keys, skinning, lighting)
        ├── shadow.wgsl      the shadow map pass
        ├── reflection.wgsl  what still water mirrors of the sky
        └── text.wgsl        the screen-space overlay shader
```

The engine and editor live in one repo so they evolve together, but build as separate targets.

## Building

You need [Rust](https://rustup.rs) 1.92 or newer. If you already have Rust, `rustup update` brings it up to date.

On Linux the editor also needs a C compiler, `pkg-config` and the ALSA headers for sound:

```
# Debian, Ubuntu, Pop!_OS
sudo apt install build-essential pkg-config libasound2-dev

# Fedora
sudo dnf install gcc pkgconf-pkg-config alsa-lib-devel
```

The launcher's Create and Open buttons use your desktop's own file picker, through `xdg-desktop-portal`. Most desktops have it already; if nothing opens when you press them, install `xdg-desktop-portal` and the one for your desktop (`xdg-desktop-portal-gtk`, `-gnome` or `-kde`).

The first build compiles every library the editor uses, so expect it to take around 10 to 15 minutes and about 3.5 GB of disk in `target/`. Later builds only rebuild what changed and take seconds.

New to Frame Engine? [GETTING_STARTED.md](GETTING_STARTED.md) goes from a first project to a few scripts, then to a game crate of your own in Rust, joined up with its scripts.

Run the editor (opens the project launcher):

```
cargo run -p frame-editor
```

On the launcher, type a name next to **New project**, press **Create…** and choose where to put it. The project gets a folder of its own with that name, holding `project.ron` and a first scene. **Open existing project…** picks a project's folder, and projects you have opened before are listed below.

Run the engine on its own (no window: a headless tick loop with an ASCII view in the terminal, **Ctrl+C** stops it):

```
cargo run -p frame-engine
```

### Editor controls

- **P** play or pause in the editor, **.** step one tick while paused
- **Left-click** select an entity, **Esc** clear selection
- **Arrow keys** move the selection on X and Y, **Page Up / Page Down** move it on Z
- **Drag a gizmo arm** move the selected entity along that axis
- **N** spawn an entity, **Delete** despawn the selection
- **Ctrl+Z** undo, **Ctrl+Y** (or **Ctrl+Shift+Z**) redo
- **Ctrl+S** (or **F5**) save the scene, **F9** reload it (asks first if there are unsaved changes)
- **Middle-drag** orbit, **Shift+Middle-drag** pan, **Scroll** zoom
- **Hold right mouse** flythrough camera: the mouse looks, **WASD** flies, **Scroll** sets fly speed, left-click picks the centred entity as the orbit pivot
- **WASD** drives a `Controlled` entity while playing in the editor
- **H** show or hide the controls overlay

### Play window controls

The Play window runs the game as a player would see it.

- **Click** the window to take the mouse: moving it turns a character's view, and a dot in the middle shows where you are looking
- **WASD** move, **Space** jump (a character with Gravity, standing on something)
- **Shift** run, **C** or **Ctrl** crouch (a character with a `CharacterBody`)
- **Left** and **right mouse buttons** are `Button::Use` and `Button::UseAlt`, **E** is `Button::Interact` and **Q** is `Button::Swap`: the game decides what each does
- In water a game says is deep (`World.liquids`), a character swims: **Space** swims up, **Ctrl** or **C** dives
- **F1** opens the game's own panels, when a module has one (`EditorModule::play_panel`), and frees the mouse while they are open
- **F3** shows the game's numbers: frame rate, memory, where you are, and each module's own
- **Esc** frees the mouse, and **Esc** again closes the window

## License

Frame Engine is licensed under the [Mozilla Public License 2.0](LICENSE) (MPL-2.0). In plain terms:

- You can use it, modify it and ship games or tools built on it, including closed-source and commercial ones.
- Your own game code, scripts, assets and separate crates stay yours, under whatever licence you choose.
- If you modify files that are part of Frame Engine and distribute the result, those modified files must be shared under the MPL-2.0 too.
- The names "Frame Engine" and "Outer Frame Interactive" are not covered by the licence. See [TRADEMARKS.md](TRADEMARKS.md).

Versions published before the licence changed to MPL-2.0 remain available under the MIT licence they were released with. The embedded fonts below are the exception to all of this, each under its own licence.

## Fonts

The editor embeds three fonts from `crates/frame-editor/assets/fonts/`: [Manrope](https://fonts.google.com/specimen/Manrope), [Fraunces](https://fonts.google.com/specimen/Fraunces), and [JetBrains Mono](https://www.jetbrains.com/lp/mono/). All three are released under the SIL Open Font License 1.1, which allows use, modification, and redistribution, including inside software, as long as the licence travels with the fonts. The licence texts, with each font's copyright notice, sit next to the fonts: [Manrope](crates/frame-editor/assets/fonts/OFL-Manrope.txt), [Fraunces](crates/frame-editor/assets/fonts/OFL-Fraunces.txt) and [JetBrains Mono](crates/frame-editor/assets/fonts/OFL-JetBrainsMono.txt). They are not covered by this repository's MPL-2.0 licence.

`Manrope-Regular.ttf` is the regular weight cut from the Manrope variable font. egui reads a variable font at its lightest weight, which looked far too thin, so a fixed weight is used instead.

## Design notes

See [GETTING_STARTED.md](GETTING_STARTED.md) for a first walk through scripts and your own Rust crate, [DESIGN.md](DESIGN.md) for architecture decisions and reasoning, [SCRIPTING.md](SCRIPTING.md) for the guide to writing entity scripts, and [PLUGINS.md](PLUGINS.md) for the guide to building a plugin.
