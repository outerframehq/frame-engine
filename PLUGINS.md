# Plugins

A plugin bundles a set of Rhai scripts (see [SCRIPTING.md](SCRIPTING.md) for the
language itself) into something you can share, drop into any project, and use
straight from the editor. This page is the reference for building one.

## Why Rhai, not compiled Rust

Rust has no stable ABI, so a compiled plugin (a `.dll` or `.so` loaded at
runtime) can crash on load if it was built with a different compiler version,
with no warning. Rhai avoids that: a plugin runs in the exact same sandboxed
interpreter every hand-written script already uses, so a bad one gets
reported instead of crashing the editor. The tradeoff is real capability, a
plugin can only reach what Rhai and the script API expose, not full Rust.

## The folder format

A plugin is a folder under a project's `plugins/` directory. This folder is
created automatically, empty, the moment you have a project open, so it's
ready to drop something into:

```
<project>/
  plugins/
    mywander/
      plugin.ron
      scripts/
        patrol.rhai
        idle.rhai
```

Two things make a folder a plugin: a `plugin.ron` manifest right inside it,
and a `scripts/` folder next to that manifest holding one or more `.rhai`
files. A folder under `plugins/` with no manifest is skipped quietly, it just
isn't treated as a plugin.

## The manifest

`plugin.ron` describes the plugin. It's the same RON format a project's own
`project.ron` already uses:

```ron
(
    name: "mywander",
    version: "0.1.0",
    author: "your name",
    description: "A couple of simple wander behaviours.",
)
```

`name`, `version`, and `author` are required. `description` is optional and
defaults to an empty string if left out.

`name` matters beyond the manifest itself: it's also the prefix every one of
the plugin's scripts gets, covered next.

## How scripts show up

Every `.rhai` file in `scripts/` becomes a library script, named
`<plugin-name>/<file-stem>`. The `patrol.rhai` and `idle.rhai` files above, in
a plugin named `mywander`, become two scripts: `mywander/patrol` and
`mywander/idle`.

That name-spacing does two things at once:

- Two different plugins can never collide by name. If another plugin also
  ships a script called `patrol`, it becomes `<its-own-name>/patrol`, a
  different name entirely.
- It's visible at a glance, in the Inspector's script picker, which scripts
  came from a plugin and which you wrote yourself.

Once loaded and enabled, a plugin's scripts are ordinary library scripts.
There's no separate plugin UI to learn: open the Inspector, pick
`mywander/patrol` from the Script picker the same way you'd pick anything
else, and it runs.

## Enabling a plugin

A plugin dropped into `plugins/` isn't turned on by itself. Opening a project
scans the folder and lists whatever it finds, but a newly discovered plugin
starts **disabled**, the same "off until you turn it on" default a mod
manager uses, never auto-activated just because it's sitting there.

To turn one on, open **Edit > Editor settings…**. It lists every installed
plugin with a checkbox: name, version, author, and description, and a switch
next to it. Ticking it merges that plugin's scripts into the project so
they're assignable from the Inspector; unticking it removes them from the
library. An entity that already had one of those scripts assigned when you
disable it keeps a dangling reference, exactly the same safe handling a
deleted hand-written script already gets, the runtime skips it, the Inspector
flags it, nothing crashes.

A **Plugins** tab, next to Help in the toolbar, opens a panel showing only the
plugins currently enabled, a quick way to see what's actually active without
opening the full settings window.

Play mode respects the same enabled/disabled choices, and re-scans your
`plugins/` folder fresh every time you press Play, so if you edit a `.rhai`
file directly on disk, Play picks it up without needing a save first.

## Writing a plugin's scripts

A plugin script is exactly a normal script; nothing about how it's written
changes because it came from a plugin. See SCRIPTING.md for the full variable
reference, the `hit`/`hit_id`/`hit_point` collision variables, `spawn`/
`despawn_id`, and everything else a script can read and write.

A small worked example, the `patrol.rhai` from above, a simple back-and-forth
walk along the x axis:

```rust
if pos.x > 50.0 {
    vel.x = -1.0;
} else if pos.x < -50.0 {
    vel.x = 1.0;
}
```

## Loading

Opening a project scans its `plugins/` folder the same way it already scans
`assets/` for imported models: every plugin found is loaded, its manifest
recorded (keeping whatever enabled/disabled choice you'd already made for it),
and an enabled plugin's scripts merged into the project's `script_library`.
This happens automatically, there's nothing to trigger by hand.

Reloading a project's assets re-scans and merges again. This is a real,
worth-knowing limitation: it adds and updates, but it doesn't currently remove
a script whose plugin, or whose specific script file, has been deleted from
disk since the last load. If you remove a plugin, the scripts it added will
still be listed until the project is closed and reopened fresh.

## Custom Inspector fields

A plugin can add its own field to the Inspector, a slider or a drag box tied
to one of its own values. Declare it in `plugin.ron`:

```ron
(
    name: "mywander",
    version: "0.1.0",
    author: "your name",
    description: "A couple of simple wander behaviours.",
    fields: [
        (name: "mywander/speed", label: "Wander speed", min: Some(0.0), max: Some(5.0)),
    ],
)
```

`name` must match a dynamic component name your own scripts already use with
`insert_dynamic`/`get_dynamic` (see SCRIPTING.md's `custom_<name>` variables).
`label` is what shows next to the control in the Inspector. `min`/`max` are
both optional; giving both draws a slider, leaving either out (or both) falls
back to a plain, unbounded drag box.

A field only ever appears for an entity that **already has a value** under
that name. It can't originate one. If nothing on the entity has ever called
`insert_dynamic("mywander/speed", …)`, the field simply doesn't show up, the
same rule the `custom_<name>` script variables already follow, and for the
same reason: a plugin describes a field, it doesn't get to decide an entity
suddenly has data it never had.

Values are always `f64`, the same type the script bridge uses. That's what
lets a script and the Inspector genuinely share one value rather than two
different types quietly registered under the same name.

This is deliberately data, not code. Rhai has no bindings to the editor's UI
library, and a plugin field is never plugin code running during rendering;
it's a description the editor itself reads and draws.

## Menu actions

A plugin can also add buttons to its own card in the Plugins panel (the tab
next to Help). Declare them in `plugin.ron`:

```ron
(
    name: "mywander",
    version: "0.1.0",
    author: "your name",
    description: "A couple of simple wander behaviours.",
    actions: [
        (label: "Run patrol now", kind: RunScript(script: "mywander/patrol")),
        (label: "Toggle alert mode", kind: ToggleValue(name: "mywander/alert")),
    ],
)
```

Two kinds exist:

- **`RunScript(script: "…")`**: runs the named script (its full name,
  including the `<plugin-name>/` prefix) once, immediately, for every entity
  that currently has it assigned. The same thing a normal tick already does
  for that entity, just triggered right now instead of waiting for the next
  one.
- **`ToggleValue(name: "…")`**: flips a dynamic value between `0.0` and `1.0`
  (above `0.5` becomes `0.0`, otherwise `1.0`) for every entity that already
  has a value under that name. Same "can't originate a value" rule as fields.

A third kind, `ToggleGlobal`, exists too, covered in Panels below, since it's
the one that makes sense there rather than on its own.

That's the complete list. A menu action is not a callback and can't run
arbitrary plugin code, it names one of these kinds, and the editor is the
only thing that ever decides what happens and does it.

## Panels

Fields are tied to an entity. Actions show up per plugin, but a plugin might
want a section that's genuinely **not** about any one entity, a settings
panel, a status display, a set of toggles that apply globally. That's what a
panel is: a titled section in the Plugins panel, shown regardless of what's
selected, or whether anything is.

```ron
(
    name: "mywander",
    version: "0.1.0",
    author: "your name",
    description: "A couple of simple wander behaviours.",
    panels: [
        (
            title: "Wander settings",
            fields: [
                (name: "mywander/global_speed", label: "Global speed", min: Some(0.0), max: Some(5.0)),
            ],
            actions: [
                (label: "Toggle wandering", kind: ToggleGlobal(name: "mywander/enabled")),
            ],
        ),
    ],
)
```

A panel is self-contained: its own `fields` and its own `actions`, not
references into the plugin's top-level lists. That's deliberate, so
reordering or editing one list can never silently break the other.

Panel fields work like Inspector fields, a slider or a drag box, `min`/`max`
optional, but read from a **global value** (a flat, world-wide store keyed by
name) instead of a per-entity one. The same "can't originate a value" rule
still applies: a panel field only shows up once something has already set
that global value, the field itself can't invent one. `ToggleGlobal` is the
global-value equivalent of `ToggleValue`, same 0.0/1.0 flip, just not tied to
any entity.

Global values are always `f64`, the only type used anywhere in this system.
There's no reason for anything reading or writing a panel value to be more
complicated than that.

## What plugins can do now

Fields, actions, and panels together cover the full editor-extension surface
this format set out to build: a plugin can add its own Inspector control, its
own buttons, and its own titled, global section, all without running any code
of its own inside the editor. What it still can't do: draw truly custom
layout, arbitrary widgets beyond a slider/drag-box and a button, or anything
outside this fixed, small set of primitives. That's a deliberate boundary, not
a gap waiting to be filled; going further would mean giving Rhai real
bindings into the editor's UI library, a much bigger trust boundary than
anything this system does today.

## Sharing a plugin

A plugin is just a folder. Sharing one means sharing that folder, the manifest
plus the scripts, for someone else to drop into their own project's `plugins/`
directory. There's no package registry or in-editor browser for this yet;
whether one gets built depends on whether people actually want it, not
something planned ahead of real demand.

Plugins are currently free to share and use. Charging for one isn't supported,
enforcing a paid one-time-use license on something that's just files a person
could otherwise freely copy is a real problem this project hasn't solved, and
isn't trying to yet.
