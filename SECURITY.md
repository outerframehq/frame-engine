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

The latest release always gets fixes. The previous couple are maintained on a
best-effort basis while they're still close to current; older ones become
snapshots that stay downloadable but no longer receive updates.

| Version | Supported | Stability | Last updated |
| ------- | --------- | --------- | ------------ |
| 0.3.0  | yes       | stable    | 2026-07-14   |
| 0.2.0  | yes       | stable    | 2026-07-05   |
| 0.1.0  | no        | snapshot  | 2026-06-27   |

"Best effort" means I'll try to push fixes while a version is still recent, but
it's an intention, not a guaranteed support window. The reliable way to stay
current is to move to the latest release.

## Known issues

Anything worth flagging in a supported release is listed here. If a version
isn't listed, there's nothing currently logged against it.

- (none currently)

## Reporting a vulnerability

If you find a security issue, please report it privately rather than opening a
public issue, so it isn't disclosed before there's a fix. The preferred route
is GitHub's private reporting:

1. Go to the **Security** tab of this repository.
2. Choose **Report a vulnerability** to open a private advisory.

As a solo project there is no guaranteed response time, but reports are read and
genuine issues will be addressed in a reasonable timeframe. Please give enough
detail to reproduce the problem.
