# Getting started

This page takes you from a fresh build to your own game code: first a few
small scripts, then a plugin to share them, then a crate of your own in Rust, then the two joined up so you
can try ideas in a script and keep the parts that work in Rust.

It assumes you have built the editor (see "Building" in the [README](README.md)). If you have not opened `examples/first-steps` yet, do that first: it is this page's scripts already running, with a character to walk around in.
The full reference for scripts is [SCRIPTING.md](SCRIPTING.md), and for plugins
[PLUGINS.md](PLUGINS.md).

## 1. A first project

1. Run the editor: `cargo run -p frame-editor`.
2. On the launcher, type a name next to **New project**, press **Create…** and
   choose where it goes. The project gets a folder of its own.
3. The editor opens on the project's first scene: a flat ground, a sun and
   one cube. Press **N** to add an entity, and **P** to play and pause.

The ground has two ticks in the Inspector, and the difference matters once
you build your own: **Static** means the built-in collision never pushes it,
and **Physics** as well makes it solid to characters and falling physics
bodies. A floor or wall with only Static is a box things can overlap, not
something to stand on.

Everything is in metres, and the world runs at 30 ticks a second. Y is up and
-Z is forward.

## 2. A few scripts

A script is a small piece of [Rhai](https://rhai.rs) that runs once a tick on
every entity it is attached to. In the **Script Editor** tab, type a name in
the Scripts list and press **Add**, paste one of these in, then select an
entity and choose the script in the Inspector's **Script** picker. Press **P** to play. Edit the numbers while
it plays: a change takes effect at once, with no rebuild.

Spin on the spot:

```rust
yaw = yaw + 0.05;
```

Bob up and down, a quarter of a metre either way:

```rust
pos.y = 1.0 + sin(t / 30.0 * 3.0) * 0.25;
```

Walk back and forth along X, four metres either side of the middle. The
script places the entity itself, so it keeps the velocity at zero:

```rust
vel = vec3(0.0, 0.0, 0.0);
pos.x = sin(t / 30.0) * 4.0;
```

Glow while **W** is held:

```rust
if input_up {
    emissive = 1.0;
} else {
    emissive = 0.0;
}
```

Count seconds in a global, a number the whole game shares and that survives a
change of scene:

```rust
if t % 30.0 == 0.0 {
    set_global("seconds", get_global("seconds") + 1.0);
}
```

Make a new entity where this one stands, once a second. A spawn is a
request, and nothing limits how often you make it, so always guard it:

```rust
if t % 30.0 == 0.0 {
    spawn_at = pos;
    spawn_new = true;
}
```

`t` is the tick count, so `t / 30.0` is seconds. If a script has a mistake it
does nothing, and the Script Editor and the Output tab say why. It can't crash
the editor. [SCRIPTING.md](SCRIPTING.md) lists every variable a script can use.

## 3. Sharing scripts as a plugin

A plugin is a folder of scripts you can drop into any project. The example
project has one, `examples/first-steps/plugins/starter`:

```
plugins/
  starter/
    plugin.ron
    scripts/
      rainbow.rhai
      pulse.rhai
```

`plugin.ron` says what it is, and can add buttons to the editor:

```ron
(
    name: "starter",
    version: "0.1.0",
    author: "Frame Engine",
    description: "An example plugin: two scripts to put on any entity, and a button that recolours everything using one of them.",
    engine: "0.1.0",
    actions: [
        (label: "Recolour now", kind: RunScript(script: "starter/rainbow")),
    ],
)
```

Each `.rhai` file in `scripts/` becomes a script named after the plugin, so
`rainbow.rhai` is `starter/rainbow`. It is written like any other script:

```rust
color = rgb(0.5 + sin(t * 0.03) * 0.5, 0.5 + sin(t * 0.03 + 2.1) * 0.5, 0.5 + sin(t * 0.03 + 4.2) * 0.5);
```

To try it:

1. Open **Edit > Editor settings…** and tick **starter**. Plugins start
   switched off, so nothing runs until you choose it.
2. Select an entity, and in the Inspector's **Script** picker choose
   `starter/rainbow` or `starter/pulse`. Press **P** to watch it.
3. Open **Plugins** in the toolbar. The **Recolour now** button runs
   `starter/rainbow` once on every entity using it.

To make your own, copy the folder, give it a new `name` in `plugin.ron`, and
change the scripts. A plugin can also add its own Inspector fields and a
panel of settings, and to share it you send the folder.
[PLUGINS.md](PLUGINS.md) has the whole format and what plugins can and can't
do.

## 4. Rhai or Rust?

Use both, for different jobs.

- **Rhai** is for trying things. It reloads while the game plays, so you can
  tune a number, watch it, and tune again in seconds. It suits behaviour on a
  handful of entities and the rules you are still deciding on.
- **Rust** is for what you keep. It is checked by the compiler, can be tested,
  runs fast over thousands of entities, and can hold real data structures and
  draw things of its own. It takes a rebuild to change.

A good habit: start an idea in a script. When it settles, or gets slow, or
gets too big to hold in your head, move it into your crate.

## 5. A crate of your own

Your game lives in its own crate, next to the engine, and hands its features
to the editor. The editor itself does not change.

```
Dev/
  frame-engine/      this repository
  my-game/
    Cargo.toml
    src/
      lib.rs         your features
      main.rs        starts the editor with them
```

`my-game/Cargo.toml`:

```toml
[package]
name = "my-game"
version = "0.1.0"
edition = "2024"

[dependencies]
frame-engine = { path = "../frame-engine/crates/frame-engine" }
frame-editor = { path = "../frame-engine/crates/frame-editor" }

# The libraries underneath (wgpu, egui, rapier, rhai and the rest) run many
# times slower unoptimised. Optimise them once; your own code stays quick
# to rebuild.
[profile.dev.package."*"]
opt-level = 2
```

A path keeps the engine's source next to yours, to read and to change. To
follow a release instead, use git:
`frame-editor = { git = "https://github.com/outerframehq/frame-engine", tag = "v0.1.0" }`,
and the same for `frame-engine`.

A feature is an `EditorModule`. The editor calls its `tick` every tick while
the game plays, with the whole world to read and change. This one gives every
player a `hunger` value that runs down over a minute. `src/lib.rs`:

```rust
use frame_editor::module_api::{EditorModule, GpuContext, ModuleGpu, ModuleScene};
use frame_editor::wgpu;
use frame_engine::world::World;

/// Every player gets a `hunger` value, from 1 (full) down to 0 (starving).
/// It drains by `hunger_rate` a minute, a global a script can change.
pub struct Hunger;

impl EditorModule for Hunger {
    fn name(&self) -> &str {
        "hunger"
    }

    fn tick(&mut self, world: &mut World, dt: f32) {
        let rate = world.globals.get("hunger_rate").copied().unwrap_or(1.0);
        let players: Vec<usize> = world.controlled.entities().collect();
        for id in players {
            let now = world.get_dynamic::<f64>("hunger", id).copied().unwrap_or(1.0);
            let next = (now - rate * dt as f64 / 60.0).clamp(0.0, 1.0);
            world.insert_dynamic::<f64>("hunger", id, next);
        }
    }

    fn new_gpu(&self, _gpu: &GpuContext) -> Box<dyn ModuleGpu> {
        Box::new(NoGpu)
    }
}

/// Hunger draws nothing, so its GPU side is empty.
struct NoGpu;

impl ModuleGpu for NoGpu {
    fn sync(&mut self, _: &wgpu::Device, _: &wgpu::Queue, _: Option<&ModuleScene>) {}

    fn has_geometry(&self) -> bool {
        false
    }

    fn draw(&self, _pass: &mut wgpu::RenderPass<'_>) {}
}
```

Every module has a GPU side for the things it draws. A module that only
changes the world, like this one, returns an empty one.

`src/main.rs` starts the editor with your module, under your own name:

```rust
// Counts memory for the F3 overlay, the same way the plain editor does.
#[global_allocator]
static ALLOC: frame_editor::CountingAlloc = frame_editor::CountingAlloc;

fn main() {
    frame_editor::run_as(
        "My Game Editor",
        "my-game-editor",
        vec![Box::new(my_game::Hunger)],
    );
}
```

Run it from `my-game/` with `cargo run`. The first build compiles the engine
too; after that only your crate rebuilds. Open your project, tick **Controlled (WASD)**
on an entity in the Inspector, and press **P**.

`EditorModule` can do much more than tick: add Inspector sections, models,
collision, terrain, water, sky, sound, menus and panels. Every method is
documented in `crates/frame-editor/src/module_api.rs`, or run
`cargo doc -p frame-editor --open` to read them in a browser.

## 6. Joining Rust and Rhai

Rust and scripts meet in two places, both plain numbers:

- **An entity's own values.** A number Rust stores on an entity with
  `world.insert_dynamic::<f64>("hunger", id, value)` shows up in that entity's
  scripts as `custom_hunger`, and a script can change it. Rust reads it back
  with `world.get_dynamic::<f64>("hunger", id)`. It is saved with the scene.
- **Globals.** `world.globals` in Rust is `get_global` and `set_global` in a
  script. It is for numbers the whole game shares, and is saved with the
  project.

Each tick, scripts run first and modules after, so each sees what the other
did a tick before.

With the Hunger module running, attach this script to the player:

```rust
// Full is white, starving is red.
color = rgb(1.0, custom_hunger, custom_hunger);

// While testing: hunger runs ten times as fast, and Space is a free meal.
set_global("hunger_rate", 10.0);
if input_jump {
    custom_hunger = 1.0;
}
```

The rule (hunger drains over time) lives in Rust. The look, the tuning and a
test shortcut live in the script, and change while you play. The script has
no `custom_hunger` until the module has run once, so on the very first tick
it does nothing.

## 7. The loop

1. Try it in a script. Play, tweak, play again.
2. When the rule is right, move it into your module. Keep anything you still
   want to tune as a global or an entity value, so a script can still reach it.
3. Write a test for it. A module's `tick` needs no window, so a test can run
   it on a world it builds itself. In `src/lib.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use frame_engine::world::{Controlled, Position, Velocity};

    #[test]
    fn a_player_gets_hungry_and_a_global_speeds_it_up() {
        let mut world = World::new();
        let still = Velocity { dx: 0.0, dy: 0.0, dz: 0.0 };
        let player = world.spawn(Position { x: 0.0, y: 0.0, z: 0.0 }, still);
        world.controlled.insert(player, Controlled);
        let rock = world.spawn(Position { x: 5.0, y: 0.0, z: 0.0 }, still);

        // One minute at the normal rate empties a full player.
        for _ in 0..60 {
            Hunger.tick(&mut world, 1.0);
        }
        assert_eq!(world.get_dynamic::<f64>("hunger", player), Some(&0.0));
        assert_eq!(world.get_dynamic::<f64>("hunger", rock), None);

        // A script set the rate to 2: half a minute empties a full player.
        world.insert_dynamic::<f64>("hunger", player, 1.0);
        world.globals.insert("hunger_rate".into(), 2.0);
        for _ in 0..15 {
            Hunger.tick(&mut world, 1.0);
        }
        let left = *world.get_dynamic::<f64>("hunger", player).unwrap();
        assert!((left - 0.5).abs() < 1e-9, "{left}");
    }
}
```

`cargo test` runs it in a moment, with no editor open.

## Where next

- [SCRIPTING.md](SCRIPTING.md): every variable and function a script has.
- [PLUGINS.md](PLUGINS.md): sharing scripts as a plugin, with Inspector fields,
  menu actions and panels.
- [DESIGN.md](DESIGN.md): why the engine is built the way it is.
- `crates/frame-editor/src/module_api.rs`: everything a module can add.
