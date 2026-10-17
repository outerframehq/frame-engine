// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! How a project lays out its scenes and the things every scene shares.
//!
//! ```text
//! project.ron           name, start scene, version (the editor's manifest)
//! project-data.ron      shared by every scene: materials of imported
//!                       models, which plugins are on, globals, substances
//! scenes/<name>.ron     one file per scene: its entities, terrain edits,
//!                       water and module data
//! scripts/<name>.rhai   one file per script, subfolders allowed
//!                       (scripts/enemies/wolf.rhai is "enemies/wolf")
//! assets/ plugins/ prefabs/
//! ```
//!
//! While a scene is open, the shared parts still live in the `World`
//! (`script_library`, `mesh_meta`, `installed_plugins`, `globals`,
//! `substances`), so everything that reads them is unchanged. Only where
//! they are saved differs: `save_scene` writes the scene without them and
//! writes them to the project's files, and `load_scene` puts them back.
//!
//! An older project keeps its one scene at the top of the folder with
//! everything inside it. `start_scene_path` still finds it, so it can be
//! played and converted (the editor converts it on open).
//!
//! Files are written to a temporary name and moved into place, so a crash
//! or full disk mid-save leaves the old file whole rather than half written.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::pack::ProjectFiles;
use crate::world::{InstalledPlugin, MeshMeta, PluginManifest, Substance, World};

pub const SCENES_DIR: &str = "scenes";
pub const SCRIPTS_DIR: &str = "scripts";
pub const PROJECT_FILE: &str = "project.ron";
pub const SHARED_FILE: &str = "project-data.ron";
const SCENE_EXT: &str = ".ron";
const SCRIPT_EXT: &str = ".rhai";

/// The parts of `project.ron` the engine needs to play a project. The editor
/// owns the whole file (version, description, build choices); anything else
/// in it is ignored here.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ProjectSettings {
    /// The project's name, shown on the launcher and in window titles.
    pub name: String,
    /// The scene a game starts on, by name (`World` for
    /// `scenes/World.ron`).
    pub start_scene: String,
}

impl ProjectSettings {
    /// Read `project.ron`, or defaults if it is missing or unreadable.
    pub fn read(files: &ProjectFiles) -> ProjectSettings {
        files
            .read_to_string(PROJECT_FILE)
            .ok()
            .and_then(|text| ron::from_str(&text).ok())
            .unwrap_or_default()
    }
}

/// What every scene of a project shares, saved in `project-data.ron`.
#[derive(Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SharedData {
    /// Materials and sizes of imported models, by model name.
    pub mesh_meta: BTreeMap<String, MeshMeta>,
    /// Which plugins are switched on, by plugin name.
    pub plugins: BTreeMap<String, bool>,
    /// Named numbers shared by the whole game (plugin settings, and what
    /// scripts keep with `set_global`).
    pub globals: BTreeMap<String, f64>,
    /// Physical substance definitions, by name.
    pub substances: BTreeMap<String, Substance>,
}

impl World {
    /// The parts of this world that belong to the whole project.
    pub fn shared_data(&self) -> SharedData {
        SharedData {
            mesh_meta: self.mesh_meta.clone(),
            plugins: self
                .installed_plugins
                .iter()
                .map(|(name, plugin)| (name.clone(), plugin.enabled))
                .collect(),
            globals: self.globals.clone(),
            substances: self.substances.clone(),
        }
    }

    /// Put a project's shared parts into this world, replacing its own. A
    /// plugin is recorded with just its name and whether it's on; the editor
    /// fills in its manifest when it scans the project's plugins.
    pub fn apply_shared(&mut self, shared: SharedData) {
        self.mesh_meta = shared.mesh_meta;
        self.globals = shared.globals;
        self.substances = shared.substances;
        let mut installed = BTreeMap::new();
        for (name, enabled) in shared.plugins {
            let mut plugin = self
                .installed_plugins
                .remove(&name)
                .unwrap_or(InstalledPlugin {
                    manifest: PluginManifest {
                        name: name.clone(),
                        ..Default::default()
                    },
                    enabled,
                });
            plugin.enabled = enabled;
            installed.insert(name, plugin);
        }
        self.installed_plugins = installed;
    }

    /// The project's own scripts in this world's library: every script
    /// except those that came from a plugin (named `<plugin>/<script>`).
    pub fn project_scripts(&self) -> BTreeMap<String, String> {
        self.script_library
            .iter()
            .filter(|(name, _)| {
                name.split_once('/')
                    .is_none_or(|(owner, _)| !self.installed_plugins.contains_key(owner))
            })
            .map(|(name, source)| (name.clone(), source.clone()))
            .collect()
    }

    /// This world as a scene file holds it: without the shared parts and
    /// scripts, which the project's own files keep.
    pub fn scene_only(&self) -> World {
        let mut scene = self.clone();
        scene.mesh_meta.clear();
        scene.installed_plugins.clear();
        scene.globals.clear();
        scene.substances.clear();
        scene.script_library.clear();
        scene
    }
}

/// A scene's path inside the project (`scenes/World.ron`).
pub fn scene_file(name: &str) -> String {
    format!("{SCENES_DIR}/{name}{SCENE_EXT}")
}

/// Whether a scene name can be used as a file name: not empty, and no
/// slashes or characters some systems refuse.
pub fn valid_scene_name(name: &str) -> bool {
    let name = name.trim();
    !name.is_empty()
        && name.len() <= 100
        && !name.starts_with('.')
        && !name.chars().any(|c| {
            matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') || c.is_control()
        })
}

/// The project's scenes by name, sorted.
pub fn list_scenes(files: &ProjectFiles) -> Vec<String> {
    let prefix = format!("{SCENES_DIR}/");
    files
        .files_under(SCENES_DIR)
        .into_iter()
        .filter_map(|path| {
            let rest = path.strip_prefix(&prefix)?;
            let name = rest.strip_suffix(SCENE_EXT)?;
            (!name.contains('/') && !name.is_empty()).then(|| name.to_string())
        })
        .collect()
}

/// Whether this is a project from before scenes had their own folder: no
/// `scenes/` scenes, and a scene file at the top.
pub fn is_old_layout(files: &ProjectFiles) -> bool {
    list_scenes(files).is_empty() && old_scene_file(files).is_some()
}

/// An old project's scene: the first `.ron` at the top of the project, by
/// name, that isn't `project.ron` or `project-data.ron`.
pub fn old_scene_file(files: &ProjectFiles) -> Option<String> {
    files.files_under("").into_iter().find(|name| {
        !name.contains('/')
            && name.ends_with(SCENE_EXT)
            && name != PROJECT_FILE
            && name != SHARED_FILE
    })
}

/// The scene a game starts on: `project.ron`'s start scene if it exists,
/// else the first scene by name, else an old project's top-level scene.
/// Returns its path inside the project.
pub fn start_scene_path(files: &ProjectFiles) -> Option<String> {
    let scenes = list_scenes(files);
    let settings = ProjectSettings::read(files);
    if scenes.contains(&settings.start_scene) {
        return Some(scene_file(&settings.start_scene));
    }
    if let Some(first) = scenes.first() {
        return Some(scene_file(first));
    }
    old_scene_file(files)
}

/// The project's scripts from `scripts/`, by name (the path inside
/// `scripts/` without `.rhai`), and any that couldn't be read.
pub fn read_scripts(files: &ProjectFiles) -> (BTreeMap<String, String>, Vec<String>) {
    let prefix = format!("{SCRIPTS_DIR}/");
    let mut scripts = BTreeMap::new();
    let mut errors = Vec::new();
    for path in files.files_under(SCRIPTS_DIR) {
        let Some(name) = path
            .strip_prefix(&prefix)
            .and_then(|rest| rest.strip_suffix(SCRIPT_EXT))
            .filter(|name| !name.is_empty())
        else {
            continue;
        };
        match files.read_to_string(&path) {
            Ok(source) => {
                scripts.insert(name.to_string(), source);
            }
            Err(e) => errors.push(format!("{path}: {e}")),
        }
    }
    (scripts, errors)
}

/// Load a scene by its path inside the project, with the project's shared
/// data and scripts put back in. A scene from an old project (with its own
/// shared parts inside, and no `project-data.ron` beside it) loads as it is.
pub fn load_scene_at(files: &ProjectFiles, path: &str) -> Result<World, String> {
    let text = files
        .read_to_string(path)
        .map_err(|e| format!("{path}: {e}"))?;
    let mut world = World::load_from_str(&text).map_err(|e| e.to_string())?;
    if files.is_file(SHARED_FILE) {
        let shared_text = files
            .read_to_string(SHARED_FILE)
            .map_err(|e| format!("{SHARED_FILE}: {e}"))?;
        let shared: SharedData =
            ron::from_str(&shared_text).map_err(|e| format!("{SHARED_FILE}: {e}"))?;
        world.apply_shared(shared);
    }
    let (scripts, errors) = read_scripts(files);
    if let Some(error) = errors.first() {
        return Err(format!("couldn't read a script: {error}"));
    }
    world.script_library.extend(scripts);
    Ok(world)
}

/// Load a scene by name (`World` for `scenes/World.ron`).
pub fn load_scene(files: &ProjectFiles, name: &str) -> Result<World, String> {
    load_scene_at(files, &scene_file(name))
}

/// Load the scene a game starts on, and its name.
pub fn load_start_scene(files: &ProjectFiles) -> Result<(String, World), String> {
    let path = start_scene_path(files).ok_or_else(|| "this project has no scene".to_string())?;
    let name = path
        .rsplit('/')
        .next()
        .and_then(|file| file.strip_suffix(SCENE_EXT))
        .unwrap_or("Game")
        .to_string();
    Ok((name, load_scene_at(files, &path)?))
}

/// Write a file by writing a temporary one beside it and moving it into
/// place, so the old file stays whole if writing fails part way.
/// Why `name` can't be a script's name, or `None` if it can. A script is
/// saved as `scripts/<name>.rhai`, so the name must work as a file path on
/// every system: `/` makes subfolders (`enemies/wolf`), but no part may be
/// empty, `.` or `..`, start or end with a space or dot, or use a character
/// Windows forbids.
pub fn script_name_problem(name: &str) -> Option<&'static str> {
    if name.trim().is_empty() {
        return Some("a name is needed");
    }
    if name.contains(['\\', '<', '>', ':', '"', '|', '?', '*'])
        || name.chars().any(char::is_control)
    {
        return Some("can't use \\ < > : \" | ? or *");
    }
    for part in name.split('/') {
        if part.is_empty() {
            return Some("a folder part is empty");
        }
        if part == "." || part == ".." {
            return Some("can't use . or .. as a part");
        }
        if part.starts_with(' ') || part.ends_with(' ') || part.ends_with('.') {
            return Some("parts can't start or end with a space, or end with a dot");
        }
    }
    None
}

pub fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut temp = path.as_os_str().to_owned();
    temp.push(".saving");
    let temp = std::path::PathBuf::from(temp);
    let result = (|| {
        use std::io::Write;
        let mut file = std::fs::File::create(&temp)?;
        file.write_all(bytes)?;
        file.sync_all()
    })();
    match result {
        Ok(()) => std::fs::rename(&temp, path),
        Err(e) => {
            let _ = std::fs::remove_file(&temp);
            Err(e)
        }
    }
}

/// Save a scene into a project folder: the scene to `scenes/<name>.ron`
/// without the shared parts, the shared parts to `project-data.ron`, and the
/// project's scripts to `scripts/`. A script file whose script is gone is
/// removed; one whose text hasn't changed is left untouched. Returns what
/// changed in `scripts/`, for the log.
pub fn save_scene(root: &Path, name: &str, world: &World) -> Result<Vec<String>, String> {
    write_scene_file(root, name, world)?;
    let shared =
        ron::ser::to_string_pretty(&world.shared_data(), ron::ser::PrettyConfig::default())
            .map_err(|e| e.to_string())?;
    write_atomic(&root.join(SHARED_FILE), shared.as_bytes()).map_err(|e| e.to_string())?;
    write_scripts(root, &world.project_scripts()).map_err(|e| e.to_string())
}

/// Write only a scene's own file (`scenes/<name>.ron`), leaving the
/// project's shared data and scripts alone: for a new scene.
pub fn write_scene_file(root: &Path, name: &str, world: &World) -> Result<(), String> {
    if !valid_scene_name(name) {
        return Err(format!("'{name}' can't be used as a scene name"));
    }
    let text = world
        .scene_only()
        .to_scene_text()
        .map_err(|e| e.to_string())?;
    write_atomic(&root.join(scene_file(name)), text.as_bytes()).map_err(|e| e.to_string())
}

/// Make `scripts/` hold exactly these scripts. Returns what changed.
pub fn write_scripts(
    root: &Path,
    scripts: &BTreeMap<String, String>,
) -> std::io::Result<Vec<String>> {
    let folder = root.join(SCRIPTS_DIR);
    let on_disk = read_scripts(&ProjectFiles::Folder(root.to_path_buf())).0;
    let mut changes = Vec::new();
    if let Some((name, problem)) = scripts
        .keys()
        .find_map(|name| script_name_problem(name).map(|p| (name, p)))
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("script name '{name}' can't be a file name ({problem}); rename it"),
        ));
    }
    for (name, source) in scripts {
        if on_disk.get(name) == Some(source) {
            continue;
        }
        let path = folder.join(format!("{name}{SCRIPT_EXT}"));
        write_atomic(&path, source.as_bytes())?;
        changes.push(if on_disk.contains_key(name) {
            format!("saved script {name}")
        } else {
            format!("added script {name}")
        });
    }
    for name in on_disk.keys() {
        if !scripts.contains_key(name) {
            std::fs::remove_file(folder.join(format!("{name}{SCRIPT_EXT}")))?;
            changes.push(format!("removed script {name}"));
        }
    }
    Ok(changes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("frame-project-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn world_with_everything() -> World {
        let mut world = World::new();
        world.spawn(
            crate::world::Position {
                x: 1.0,
                y: 2.0,
                z: 3.0,
            },
            crate::world::Velocity {
                dx: 0.0,
                dy: 0.0,
                dz: 0.0,
            },
        );
        world
            .script_library
            .insert("door".into(), "let a = 1;".into());
        world
            .script_library
            .insert("enemies/wolf".into(), "let b = 2;".into());
        world
            .script_library
            .insert("weather/rain".into(), "let c = 3;".into());
        world.installed_plugins.insert(
            "weather".into(),
            InstalledPlugin {
                manifest: PluginManifest {
                    name: "weather".into(),
                    ..Default::default()
                },
                enabled: true,
            },
        );
        world.globals.insert("difficulty".into(), 2.0);
        world
    }

    #[test]
    fn a_scene_saves_without_shared_parts_and_loads_with_them() {
        let root = temp("save");
        let world = world_with_everything();
        let changes = save_scene(&root, "World", &world).unwrap();
        assert_eq!(changes, ["added script door", "added script enemies/wolf"]);

        let scene_text = std::fs::read_to_string(root.join("scenes/World.ron")).unwrap();
        assert!(!scene_text.contains("difficulty"));
        assert!(!scene_text.contains("let a"));
        assert_eq!(
            std::fs::read_to_string(root.join("scripts/enemies/wolf.rhai")).unwrap(),
            "let b = 2;"
        );
        // A plugin's script stays in the plugin, not scripts/.
        assert!(!root.join("scripts/weather").exists());

        let files = ProjectFiles::Folder(root.clone());
        assert_eq!(list_scenes(&files), ["World"]);
        let loaded = load_scene(&files, "World").unwrap();
        assert_eq!(loaded.positions.len(), world.positions.len());
        assert_eq!(loaded.globals.get("difficulty"), Some(&2.0));
        assert!(loaded.installed_plugins["weather"].enabled);
        assert_eq!(loaded.script_library["door"], "let a = 1;");
        assert_eq!(loaded.script_library["enemies/wolf"], "let b = 2;");

        // Saving again with a script gone and one changed.
        let mut next = loaded;
        next.script_library.remove("door");
        next.script_library
            .insert("enemies/wolf".into(), "let b = 3;".into());
        let changes = save_scene(&root, "World", &next).unwrap();
        assert_eq!(
            changes,
            ["saved script enemies/wolf", "removed script door"]
        );
        assert!(!root.join("scripts/door.rhai").exists());
        // No temporary files left behind.
        let leftovers: Vec<String> = files
            .files_under("")
            .into_iter()
            .filter(|f| f.ends_with(".saving"))
            .collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn the_start_scene_comes_from_project_ron_then_the_first_scene_then_an_old_layout() {
        let root = temp("start");
        let files = ProjectFiles::Folder(root.clone());
        assert_eq!(start_scene_path(&files), None);

        // Old layout: a scene at the top.
        World::new().save_to_file(root.join("Ichor.ron")).unwrap();
        std::fs::write(root.join("project.ron"), "(version: \"0.1.0\")").unwrap();
        assert!(is_old_layout(&files));
        assert_eq!(start_scene_path(&files).as_deref(), Some("Ichor.ron"));
        let (name, _) = load_start_scene(&files).unwrap();
        assert_eq!(name, "Ichor");

        // New layout wins once there are scenes.
        save_scene(&root, "Cave", &World::new()).unwrap();
        save_scene(&root, "World", &World::new()).unwrap();
        assert!(!is_old_layout(&files));
        assert_eq!(start_scene_path(&files).as_deref(), Some("scenes/Cave.ron"));
        std::fs::write(
            root.join("project.ron"),
            "(name: \"Ichor\", start_scene: \"World\", version: \"0.1.0\")",
        )
        .unwrap();
        assert_eq!(
            start_scene_path(&files).as_deref(),
            Some("scenes/World.ron")
        );
        assert_eq!(ProjectSettings::read(&files).name, "Ichor");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn scene_names_must_be_usable_file_names() {
        assert!(valid_scene_name("World"));
        assert!(valid_scene_name("Cave 2"));
        for bad in ["", "  ", "a/b", "a\\b", ".hidden", "what?", "a:b"] {
            assert!(!valid_scene_name(bad), "{bad}");
        }
    }

    #[test]
    fn a_failed_write_leaves_the_old_file_whole() {
        let root = temp("atomic");
        let path = root.join("scene.ron");
        write_atomic(&path, b"old").unwrap();
        // Writing into a folder that is a file fails before the move.
        let blocked = root.join("scene.ron").join("inside.ron");
        assert!(write_atomic(&blocked, b"new").is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"old");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn script_names_must_work_as_file_paths() {
        for good in ["door", "enemies/wolf", "a b", "v1.2"] {
            assert_eq!(script_name_problem(good), None, "{good}");
        }
        for bad in [
            "", " ", "../x", "a/../b", "a//b", "/a", "a/", "x:y", "a\\b", "end.", " lead", "q?",
        ] {
            assert!(script_name_problem(bad).is_some(), "{bad:?}");
        }
        let root = temp("badname");
        let mut scripts = BTreeMap::new();
        scripts.insert("../escape".to_string(), "x".to_string());
        assert!(write_scripts(&root, &scripts).is_err());
        assert!(!root.join("escape.rhai").exists());
    }
}
