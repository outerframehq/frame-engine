# Scripting in Frame Engine

Any entity can carry a **script**: a small piece of code that runs every tick and
changes that entity. Scripts live in a shared **library**: you write one in the
editor's **Script Editor** tab, then attach it to an entity from the
**Inspector** — select an entity, open its **Script** picker, and choose the
script by name. Because scripts are shared, one script can drive many entities,
and editing it in the Script Editor updates every entity that uses it at once.
Changes take effect live, with no rebuild. This page is the reference for what
you can write.

## The language

Scripts are written in [Rhai](https://rhai.rs), a small scripting language made
for embedding in Rust. If you've seen Rust or JavaScript it will look familiar:

```rust
let speed = 2.0;          // variables
if px > 100.0 { ... }     // conditionals
while n > 0 { n -= 1; }   // loops
// line comments
```

Numbers are floating point. The usual maths is available — `+ - * / %`, and
functions like `sin`, `cos`, `sqrt`, `abs`, `min`, `max`, `floor`, `ceil`. Full
language details are in the [Rhai book](https://rhai.rs/book/).

You don't need most of it. Most useful scripts are one or two lines.

## How a script runs

- It runs **once per tick**, and the simulation runs at **30 ticks per second**.
- It runs for **every entity that carries a script**, independently. The same
  library script can be attached to many entities; each runs it on its own state.
- Each tick the entity's current state is handed to your script as variables,
  your code runs, and any variables you changed are written back into the world.
- It's **deterministic**: the same tick produces the same result every time.

So a script isn't a one-off — it's a rule that re-runs 30 times a second. That's
why time-based maths (below) makes things move.

## Variables you can use

These are set for you every tick. Read them, write them, or both.

| Variable       | Meaning                        | Access     | Notes                              |
|----------------|--------------------------------|------------|------------------------------------|
| `t`            | Tick counter                   | read-only  | Increases by 1 each tick. `t / 30.0` is seconds. |
| `px` `py` `pz` | Position (x, y, z)             | read/write | Where the entity is.               |
| `dx` `dy` `dz` | Velocity (per tick)            | read/write | Added to position every tick.      |
| `sx` `sy` `sz` | Scale (per axis)               | read/write | `1.0` is normal size.              |
| `cr` `cg` `cb` | Colour (red, green, blue)      | read/write | Each `0.0`–`1.0`.                  |

Anything you don't set keeps its current value. `t` is read-only — writing to it
does nothing.

## Position vs velocity — the one thing to understand

There are two ways to make something move, and they behave differently:

- **Set velocity** (`dx`, `dy`, `dz`) and let it accumulate. `dx = 0.5;` means
  "drift east forever." You set it once and motion continues on its own.
- **Set position** (`px`, `py`, `pz`) directly. `px = cos(t * 0.05) * 50.0;`
  means "be exactly here this tick." You're placing the entity yourself every
  tick, so you have total control of the path.

If you drive **position** from a script, keep **velocity at zero**, or the two
will fight (the script places the entity, then velocity nudges it off again).

## Examples

**Orbit** the origin in the XY plane:

```rust
px = cos(t * 0.08) * 50.0;
py = sin(t * 0.08) * 50.0;
```

**Bob** up and down on the Z axis:

```rust
pz = sin(t * 0.1) * 20.0;
```

**Pulse** — breathe in and out by scaling:

```rust
let s = 1.0 + sin(t * 0.1) * 0.5;
sx = s;
sy = s;
sz = s;
```

**Climb** steadily using velocity (set once, keeps going):

```rust
dz = 0.5;
```

**Throb** the colour between dim and bright red:

```rust
cr = 0.5 + sin(t * 0.1) * 0.5;
cg = 0.1;
cb = 0.1;
```

**React** to position — fall until low, then rise (a rough bounce):

```rust
if pz > 60.0 {
    dz = -0.5;
} else if pz < 0.0 {
    dz = 0.5;
}
```

Change any number and watch it update while the simulation plays. The `* 0.08`
kind of number is speed; the `* 50.0` kind is size or distance.

## When a script has a mistake

A script that doesn't compile (a typo, an unfinished line) simply does nothing —
the entity keeps whatever state it already had, and the error is reported once in
the editor's Output console. Fix the text and it picks up again automatically. You
can't crash the editor with a bad script, so experiment freely.

## A note on the vocabulary

The variables above are what Frame Engine currently exposes to scripts. It's a
deliberately small set, and it will grow over time. If there's world state you
want to reach that isn't listed here, that's a missing feature, not something
you're doing wrong.
