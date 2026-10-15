# Security policy

Frame Engine is an early-stage, solo-developed project. It is pre-1.0 and under
active development: breaking changes are expected between versions, and `main`
is a working branch, not a guaranteed-stable one.

## Use a release, not a fresh clone

The `main` branch is where in-progress work lands, so at any moment it may not
build or may behave in half-finished ways. **If you just want to run the tool,
download a tagged release rather than cloning `main`.** Releases are cut at
points where the project is known to build and run, so a release is your best
bet for a stable copy. Tagged releases are on the project's Releases page.

## Supported versions

This repo restarted fresh with no tagged releases yet. `Cargo.toml` currently
sits at `0.0.0`, a placeholder for this pre-release baseline, not a real
version number. The first tagged release will be `0.1.0`, planned for Friday
30 October 2026 at 10pm UK time. It is a source release: a tag, release notes
and build instructions, with no installers.

| Version | Supported | Stability | Last updated |
| ------- | --------- | --------- | ------------ |
| (none yet) | --        | --        | --           |

Once releases start, the latest one always gets fixes. The previous couple
are maintained on a best-effort basis while they're still close to current;
older ones become snapshots that stay downloadable but no longer receive
updates. "Best effort" means I'll try to push fixes while a version is still
recent, but it's an intention, not a guaranteed support window. The reliable
way to stay current is to move to the latest release.

## Patch releases and hotfixes

New features ship at most once a week, so each release has a clear reason to
upgrade. Fixes for serious problems do not wait for that. A crash, a bug that
loses or corrupts your work, or a security problem gets a patch release
(`0.1.1`, `0.1.2` and so on) as soon as the fix is ready and tested. A patch
release only contains fixes, never new features, so it is always safe to move
to the latest patch of the version you are on.

Smaller bugs are fixed on `main` and go out with the next regular release.

## Known issues

Anything worth flagging in a supported release is listed here. If a version
isn't listed, there's nothing currently logged against it.

### What has been tested

Frame Engine is built and tested by one person on one machine, so coverage is
narrow:

- **Linux:** tested day to day on Pop!_OS with an NVIDIA GPU, on Wayland. This
  is the setup everything is built on.
- **Windows:** run only a little. Expect rough edges, and see the docking
  crash below.
- **macOS:** not tested at all. It may build and run, but nobody has checked.
- **AMD and Intel GPUs, X11 and other Linux desktops:** not tested.

If something breaks on a setup outside that list, please open an issue with
your OS, GPU and driver version. Reports from other machines are the main way
these gaps get closed.

### Open bugs

- **Windows: docking a popped-out panel back crashes the editor.** Popping a panel (Scene, Inspector, Script Editor or Source Control) out into its own window and then docking it back into the main window crashes the editor on Windows. The cause is not found yet. Until it is fixed, avoid popping panels out on Windows, and save first (Ctrl+S) if you do try it, because the crash loses any unsaved changes. The same feature works on Linux (tested on Pop!_OS with an NVIDIA GPU). Nothing here affects saved scenes.

## Reporting a vulnerability

If you find a security issue, please report it privately rather than opening a
public issue, so it isn't disclosed before there's a fix. The preferred route
is GitHub's private reporting:

1. Go to the **Security** tab of this repository.
2. Choose **Report a vulnerability** to open a private advisory.

As a solo project there is no guaranteed response time, but reports are read and
genuine issues will be addressed in a reasonable timeframe. Please give enough
detail to reproduce the problem.
