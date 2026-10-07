# Contributing to Frame Engine

Thanks for taking an interest. Frame Engine is built and maintained by one person, so the process is deliberately light. Please read this before opening a pull request.

## Before you start

- **Open an issue first** for anything bigger than a small fix. A short description of what you want to change and why saves you from writing something that doesn't fit the direction of the engine.
- **Small, focused pull requests** are much easier to review than large ones. One change per pull request.
- The engine and editor here are kept **generic**. Features that belong to one specific game or genre are built as separate crates on top of the engine's extension points rather than inside it. If you are unsure which side something belongs on, ask in the issue.

## Licence of your contributions

Frame Engine is licensed under the [Mozilla Public License 2.0](LICENSE). By submitting a contribution you agree that it is licensed under the same terms, and that you have the right to submit it.

To make that explicit, every commit must carry a sign-off line, which certifies the [Developer Certificate of Origin](https://developercertificate.org/) (the code is yours, or you have the right to contribute it under this licence):

```
Signed-off-by: Your Name <you@example.com>
```

Git adds it for you with `git commit -s`. Pull requests with unsigned commits will be asked to fix that before merging.

Please do not submit code you copied from somewhere else unless its licence is compatible with the MPL-2.0 and you say where it came from in the pull request. Do not submit code generated from, or derived from, a project under a licence that does not allow it.

New source files need the licence notice used in the rest of the project at the top of the file:

```
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
```

## Checks before you submit

Run these from the repository root and make sure they pass:

```
cargo fmt --all
cargo build
cargo test
```

- Add tests for new behaviour. Most of the engine is tested without a GPU, so logic should be testable without one.
- Keep changes documented: add a line under `[Unreleased]` in `CHANGELOG.md`, and update `DESIGN.md` if you change how something is built or why.
- Don't add new dependencies without discussing it in the issue first.

## Names and logos

The names "Frame Engine", "Frame Editor" and "Outer Frame Interactive" are covered by [TRADEMARKS.md](TRADEMARKS.md), not by the code licence.

## Reporting security problems

Please report security issues privately, as described in [SECURITY.md](SECURITY.md), rather than in a public issue.

## Conduct

Be respectful and keep discussion about the work. Contributors who are abusive or harassing will be removed from the project.
