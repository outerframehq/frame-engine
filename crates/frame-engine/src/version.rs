// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Which version of the engine this is, and how to compare it with a version
//! written in a file (a project's manifest, a plugin's `plugin.ron`).
//!
//! Every check that guards against a mismatch has to exist in the older
//! build, since that is the one opening the newer file. So these are small,
//! forgiving and never panic: a version that can't be read compares as
//! unknown rather than as an error.

use std::cmp::Ordering;

/// This build's version, from `Cargo.toml` (`major.minor.patch`). The engine
/// and the editor share one workspace version, so this is the editor's too.
pub const ENGINE: &str = env!("CARGO_PKG_VERSION");

/// Read `major.minor.patch` from a version string. Missing parts count as 0
/// (`"0.2"` is `0.2.0`), a leading `v` is allowed, and anything after a `-`
/// or `+` (a pre-release or build tag) is ignored. `None` for an empty or
/// unreadable string.
pub fn parse(text: &str) -> Option<(u64, u64, u64)> {
    let text = text.trim().trim_start_matches('v');
    let core = text.split(['-', '+']).next()?;
    if core.is_empty() {
        return None;
    }
    let mut parts = core.split('.');
    let mut next = || -> Option<u64> {
        match parts.next() {
            None => Some(0),
            Some(p) => p.trim().parse().ok(),
        }
    };
    let version = (next()?, next()?, next()?);
    if parts.next().is_some() {
        return None;
    }
    Some(version)
}

/// How `text` compares with this build. `Some(Greater)` means the file asks
/// for, or was written by, a newer engine than this one. `None` when either
/// side can't be read.
pub fn compare_with_this(text: &str) -> Option<Ordering> {
    Some(parse(text)?.cmp(&parse(ENGINE)?))
}

/// True if `text` names a newer engine than this build.
pub fn is_newer_than_this(text: &str) -> bool {
    compare_with_this(text) == Some(Ordering::Greater)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_versions_forgivingly() {
        assert_eq!(parse("0.1.0"), Some((0, 1, 0)));
        assert_eq!(parse("v0.2"), Some((0, 2, 0)));
        assert_eq!(parse("1"), Some((1, 0, 0)));
        assert_eq!(parse("0.3.1-beta.2"), Some((0, 3, 1)));
        assert_eq!(parse(""), None);
        assert_eq!(parse("soon"), None);
        assert_eq!(parse("1.2.3.4"), None);
    }

    #[test]
    fn compares_with_this_build() {
        assert!(parse(ENGINE).is_some(), "Cargo.toml version reads");
        assert!(is_newer_than_this("999.0.0"));
        assert!(!is_newer_than_this(ENGINE));
        assert!(!is_newer_than_this("0.0.0"));
        assert!(!is_newer_than_this("not a version"));
    }
}
