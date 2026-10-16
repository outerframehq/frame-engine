// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! `THIRD-PARTY-LICENSES.txt` for a game build: every library and font the
//! game program contains, with its licence.
//!
//! Almost every Rust library's licence (MIT, Apache-2.0, BSD, Zlib, the OFL
//! for fonts) asks that its copyright notice and licence text go with any
//! copy of the program, so a build carries them all in one file.
//!
//! Which libraries: `cargo tree -e normal --target <platform>` in the game's
//! workspace, so the list is exactly what that platform's program links,
//! without build tools or test-only libraries. The game's own crates (path
//! dependencies) are left out; the engine is credited by
//! `LICENSE-Frame-Engine.txt`.
//!
//! Which texts: each library's own licence files, found in Cargo's
//! downloaded sources (`$CARGO_HOME/registry/src`). Vendored C code keeps
//! its notices one or two folders down (`libgit2/COPYING`), so the search
//! goes two folders deep. Identical texts are written once, with every
//! library that uses them. A library that ships no licence file gets the
//! standard text of the licence it names, from `assets/licenses/`.
//!
//! The editor's three fonts are built into every program, so their OFL
//! texts are always included.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// One library the program contains, as `cargo tree` lists it.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Library {
    pub name: String,
    pub version: String,
    /// The SPDX licence expression from its Cargo.toml (`MIT OR Apache-2.0`).
    pub license: String,
    pub repository: String,
}

/// Standard licence texts, for libraries that name a licence but ship no
/// file for it. Generic texts with no copyright holder in them.
const STANDARD_TEXTS: &[(&str, &str)] = &[
    ("MIT", include_str!("../assets/licenses/MIT.txt")),
    (
        "Apache-2.0",
        include_str!("../assets/licenses/Apache-2.0.txt"),
    ),
    ("BSL-1.0", include_str!("../assets/licenses/BSL-1.0.txt")),
    ("CC0-1.0", include_str!("../assets/licenses/CC0-1.0.txt")),
];

/// The fonts built into every program made with the editor library.
const FONTS: &[(&str, &str)] = &[
    ("Fraunces", include_str!("../assets/fonts/OFL-Fraunces.txt")),
    (
        "JetBrains Mono",
        include_str!("../assets/fonts/OFL-JetBrainsMono.txt"),
    ),
    ("Manrope", include_str!("../assets/fonts/OFL-Manrope.txt")),
];

/// Read one `cargo tree --format "{p}|{l}|{r}"` line. `None` for the game's
/// own crates (a path or git source in brackets after the version) and for
/// anything that isn't a library line.
pub(crate) fn parse_tree_line(line: &str) -> Option<Library> {
    let line = line.trim().trim_end_matches(" (*)");
    let mut parts = line.splitn(3, '|');
    let package = parts.next()?.trim();
    let license = parts.next().unwrap_or("").trim().to_string();
    let repository = parts.next().unwrap_or("").trim().to_string();
    let (name, version) = package.split_once(" v")?;
    if version.contains(' ') || name.contains(' ') || name.is_empty() {
        return None; // "frame-engine v0.0.0 (/path)": our own
    }
    Some(Library {
        name: name.to_string(),
        version: version.to_string(),
        license,
        repository,
    })
}

/// Whether a file name looks like a licence or notice.
fn is_licence_file(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.starts_with("license")
        || lower.starts_with("licence")
        || lower.starts_with("copying")
        || lower.starts_with("notice")
        || lower.starts_with("copyright")
        || lower.contains("license")
        || lower == "ofl.txt"
        || lower == "ufl.txt"
}

/// A library's licence files: at its top level and up to two folders down,
/// sorted, skipping tests, examples and build output.
pub(crate) fn licence_files(source: &Path) -> Vec<PathBuf> {
    fn walk(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            match entry.file_type() {
                Ok(t) if t.is_dir() => {
                    let skip = matches!(
                        name.as_str(),
                        "target" | "tests" | "benches" | "examples" | ".git" | ".github"
                    );
                    if depth < 2 && !skip {
                        walk(&entry.path(), depth + 1, out);
                    }
                }
                Ok(t) if t.is_file() && is_licence_file(&name) => out.push(entry.path()),
                _ => {}
            }
        }
    }
    let mut out = Vec::new();
    walk(source, 0, &mut out);
    out.sort();
    out
}

/// Where Cargo keeps downloaded library sources: one folder per registry
/// under `$CARGO_HOME/registry/src`.
pub(crate) fn registry_sources() -> Vec<PathBuf> {
    let home = std::env::var_os("CARGO_HOME")
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|h| h.join(".cargo")));
    let Some(src) = home.map(|h| h.join("registry").join("src")) else {
        return Vec::new();
    };
    let Ok(entries) = std::fs::read_dir(src) else {
        return Vec::new();
    };
    let mut dirs: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    dirs.sort();
    dirs
}

/// The SPDX licence ids an expression names (`MIT OR Apache-2.0` -> MIT,
/// Apache-2.0). Older crates write `MIT/Apache-2.0`.
fn licence_ids(expression: &str) -> Vec<String> {
    expression
        .split(|c: char| c.is_whitespace() || matches!(c, '/' | '(' | ')'))
        .filter(|w| !w.is_empty() && !matches!(*w, "OR" | "AND" | "WITH"))
        .map(String::from)
        .collect()
}

/// Write the whole notices file's text for these libraries. `sources` are
/// the registry folders to look for each library's files in.
pub(crate) fn notices_text(game: &str, libraries: &[Library], sources: &[PathBuf]) -> String {
    let mut libraries = libraries.to_vec();
    libraries.sort();
    libraries.dedup();
    // Text -> the libraries that ship it, so a text shared by many
    // libraries (the plain Apache-2.0 text) is written once.
    let mut texts: BTreeMap<String, Vec<String>> = BTreeMap::new();
    // Licence id -> libraries that ship no file and need the standard text.
    let mut standard: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for library in &libraries {
        let label = format!("{} {}", library.name, library.version);
        let folder = format!("{}-{}", library.name, library.version);
        let files = sources
            .iter()
            .map(|s| s.join(&folder))
            .find(|p| p.is_dir())
            .map(|dir| licence_files(&dir))
            .unwrap_or_default();
        let mut found = false;
        for file in files {
            if let Ok(text) = std::fs::read_to_string(&file) {
                let text = text.trim().to_string();
                if !text.is_empty() {
                    texts.entry(text).or_default().push(label.clone());
                    found = true;
                }
            }
        }
        if !found {
            for id in licence_ids(&library.license) {
                standard.entry(id).or_default().push(label.clone());
            }
        }
    }

    let rule = "=".repeat(78);
    let line = "-".repeat(78);
    let mut out = String::new();
    out.push_str(&format!(
        "Third-party software in {game}\n\n\
         {game} is built with Frame Engine (see LICENSE-Frame-Engine.txt), and\n\
         contains the fonts and libraries below. Each is listed with its licence;\n\
         the licence texts and copyright notices follow.\n\n"
    ));
    out.push_str(&format!("{rule}\nFonts\n{rule}\n\n"));
    for (name, _) in FONTS {
        out.push_str(&format!("{name}: SIL Open Font License 1.1\n"));
    }
    out.push_str(&format!(
        "\n{rule}\nLibraries ({})\n{rule}\n\n",
        libraries.len()
    ));
    for library in &libraries {
        let licence = if library.license.is_empty() {
            "licence not stated, see its files below"
        } else {
            library.license.as_str()
        };
        out.push_str(&format!(
            "{} {}: {}",
            library.name, library.version, licence
        ));
        if !library.repository.is_empty() {
            out.push_str(&format!(" ({})", library.repository));
        }
        out.push('\n');
    }
    out.push_str(&format!("\n{rule}\nLicence texts and notices\n{rule}\n"));
    for (name, text) in FONTS {
        out.push_str(&format!(
            "\n{line}\nFont: {name}\n{line}\n\n{}\n",
            text.trim()
        ));
    }
    for (text, users) in &texts {
        out.push_str(&format!(
            "\n{line}\nUsed by: {}\n{line}\n\n{text}\n",
            users.join(", ")
        ));
    }
    for (id, users) in &standard {
        let body = STANDARD_TEXTS
            .iter()
            .find(|(known, _)| known == id)
            .map(|(_, text)| text.trim().to_string())
            .unwrap_or_else(|| {
                format!("The standard text of {id} is at https://spdx.org/licenses/{id}.html")
            });
        out.push_str(&format!(
            "\n{line}\n{id}, for libraries that ship no licence file of their own: {}\n{line}\n\n{body}\n",
            users.join(", ")
        ));
    }
    out
}

/// The libraries a platform's game program links, from `cargo tree` in its
/// workspace.
pub(crate) fn program_libraries(
    mut cargo: std::process::Command,
    workspace: &Path,
    target: &str,
) -> Result<Vec<Library>, String> {
    let output = cargo
        .current_dir(workspace)
        .args([
            "tree",
            "--workspace",
            "-e",
            "normal",
            "--prefix",
            "none",
            "--color",
            "never",
            "--format",
            "{p}|{l}|{r}",
            "--target",
            target,
        ])
        .output()
        .map_err(|e| format!("couldn't run cargo tree: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "cargo tree failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    // A library used by several crates is listed under each of them.
    let mut libraries: Vec<Library> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(parse_tree_line)
        .collect();
    libraries.sort();
    libraries.dedup();
    Ok(libraries)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tree_lines_give_libraries_and_skip_our_own_crates() {
        assert_eq!(
            parse_tree_line(
                "serde v1.0.229|MIT OR Apache-2.0|https://github.com/serde-rs/serde (*)"
            ),
            Some(Library {
                name: "serde".to_string(),
                version: "1.0.229".to_string(),
                license: "MIT OR Apache-2.0".to_string(),
                repository: "https://github.com/serde-rs/serde".to_string(),
            })
        );
        assert_eq!(
            parse_tree_line("spirv v0.4.0+sdk-1.4.341.0|Apache-2.0|").map(|l| l.version),
            Some("0.4.0+sdk-1.4.341.0".to_string())
        );
        assert_eq!(
            parse_tree_line("frame-engine v0.0.0 (/home/me/frame-engine)|MPL-2.0|"),
            None
        );
        assert_eq!(parse_tree_line(""), None);
        assert_eq!(
            licence_ids("(MIT OR Apache-2.0) AND Unicode-3.0"),
            ["MIT", "Apache-2.0", "Unicode-3.0"]
        );
        assert_eq!(licence_ids("MIT/Apache-2.0"), ["MIT", "Apache-2.0"]);
    }

    #[test]
    fn notices_hold_each_text_once_and_fall_back_to_standard_texts() {
        let registry = std::env::temp_dir().join(format!("frame-licences-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&registry);
        let write = |path: &str, text: &str| {
            let path = registry.join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        };
        write("alpha-1.0.0/LICENSE-MIT", "Copyright Alpha\nMIT terms");
        write("alpha-1.0.0/LICENSE-APACHE", "Apache terms");
        write("beta-2.0.0/LICENSE", "Apache terms");
        write("beta-2.0.0/vendor/c-lib/COPYING", "C library notice");
        write("beta-2.0.0/tests/LICENSE", "test file, not shipped");
        write("beta-2.0.0/src/a/b/LICENSE", "too deep");
        write("gamma-3.0.0/src/lib.rs", "// no licence file");
        let lib = |name: &str, version: &str, license: &str| Library {
            name: name.to_string(),
            version: version.to_string(),
            license: license.to_string(),
            repository: String::new(),
        };
        let libraries = [
            lib("alpha", "1.0.0", "MIT OR Apache-2.0"),
            lib("beta", "2.0.0", "Apache-2.0"),
            lib("gamma", "3.0.0", "BSL-1.0 OR Weird-1.0"),
            lib("alpha", "1.0.0", "MIT OR Apache-2.0"),
        ];
        let text = notices_text("Game", &libraries, std::slice::from_ref(&registry));
        assert!(text.contains("Libraries (3)"));
        assert!(text.contains("alpha 1.0.0: MIT OR Apache-2.0"));
        assert_eq!(text.matches("Apache terms").count(), 1);
        assert!(text.contains("Used by: alpha 1.0.0, beta 2.0.0"));
        assert!(text.contains("C library notice"));
        assert!(!text.contains("test file"));
        assert!(!text.contains("too deep"));
        assert!(text.contains("Boost Software License"));
        assert!(text.contains("https://spdx.org/licenses/Weird-1.0.html"));
        for (font, _) in FONTS {
            assert!(text.contains(&format!("Font: {font}")));
        }
        assert!(text.contains("SIL OPEN FONT LICENSE"));
        std::fs::remove_dir_all(&registry).ok();
    }
}
