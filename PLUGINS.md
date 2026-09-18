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

## What plugins can't do yet

Right now a plugin only supplies scripts, and Editor Settings only offers an
on/off switch for each one. A plugin has no way to add its own Inspector
fields, menu items, or panels to the editor itself. Giving plugins that kind
of real editor-extension surface is a genuine, intended direction, not
something the current format rules out, just not built yet.

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
