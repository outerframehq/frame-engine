// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! A project's scenes in the editor: converting a project from the old
//! layout, the scene file operations behind the Scenes tab (new, duplicate,
//! rename, delete, set as start) and the tab itself. The layout is
//! `frame_engine::project`'s.

use std::path::Path;

use frame_engine::pack::ProjectFiles;
use frame_engine::project::{self, SCENES_DIR};
use frame_engine::world::World;

use crate::{read_manifest, write_manifest};

/// Convert a project from the old layout (one scene at the top, holding the
/// project's scripts, materials and plugin settings) to the new one. The old
/// scene file is first copied to `<name>.ron.backup`; then the scene goes to
/// `scenes/<name>.ron`, the scripts to `scripts/`, the rest to
/// `project-data.ron`, and `project.ron` gets the project's name and start
/// scene. The original top-level file is removed only once the new scene has
/// been read back. Returns what was done, for the log.
pub(crate) fn convert_old_project(root: &Path) -> Result<Vec<String>, String> {
    let files = ProjectFiles::Folder(root.to_path_buf());
    let Some(old) = project::old_scene_file(&files) else {
        return Err("no old scene to convert".to_string());
    };
    let name = old.trim_end_matches(".ron").to_string();
    if !project::valid_scene_name(&name) {
        return Err(format!(
            "the scene name '{name}' can't be used as a file name"
        ));
    }
    let mut done = vec![format!(
        "Converting '{name}' to the scenes layout (scenes/, scripts/, project-data.ron)"
    )];

    // A plain copy first, so there is always the untouched original.
    let mut backup = root.join(format!("{old}.backup"));
    let mut n = 2;
    while backup.exists() {
        backup = root.join(format!("{old}.backup-{n}"));
        n += 1;
    }
    std::fs::copy(root.join(&old), &backup).map_err(|e| format!("couldn't back up {old}: {e}"))?;
    done.push(format!(
        "Backed up {old} to {}",
        backup.file_name().unwrap_or_default().to_string_lossy()
    ));

    let text = files
        .read_to_string(&old)
        .map_err(|e| format!("couldn't read {old}: {e}"))?;
    let world = World::load_from_str(&text).map_err(|e| e.to_string())?;
    let scripts = world.project_scripts().len();
    let changes = project::save_scene(root, &name, &world)?;
    done.extend(changes.into_iter().map(|c| format!("  {c}")));
    done.push(format!(
        "Moved the scene to {}, {scripts} script(s) to scripts/, materials, plugin settings and globals to project-data.ron",
        project::scene_file(&name)
    ));

    // Make sure the new scene reads back before the old file goes.
    let check = project::load_scene(&files, &name)?;
    if check.script_library.len() < scripts {
        return Err("the converted scene didn't read back with all its scripts; the old file was left in place".to_string());
    }
    let mut manifest = read_manifest(root);
    if manifest.name.trim().is_empty() {
        manifest.name = name.clone();
    }
    manifest.start_scene = name.clone();
    write_manifest(root, &manifest);
    std::fs::remove_file(root.join(&old))
        .map_err(|e| format!("couldn't remove the old {old}: {e}"))?;
    done.push(format!("Set '{name}' as the start scene in project.ron"));

    // Other scene files saved at the top with Save As in the old layout are
    // left where they are, and said so.
    for other in files.files_under("") {
        if !other.contains('/')
            && other.ends_with(".ron")
            && other != project::PROJECT_FILE
            && other != project::SHARED_FILE
        {
            done.push(format!(
                "Left {other} at the top of the project; move it into scenes/ to use it as a scene"
            ));
        }
    }
    Ok(done)
}

/// Make a new, empty scene file (not opened). Fails if the name is taken.
pub(crate) fn create_scene(root: &Path, name: &str, world: &World) -> Result<(), String> {
    check_new_name(root, name)?;
    project::write_scene_file(root, name, world)
}

/// Copy a scene's file under a new name.
pub(crate) fn duplicate_scene(root: &Path, from: &str, to: &str) -> Result<(), String> {
    check_new_name(root, to)?;
    std::fs::copy(
        root.join(project::scene_file(from)),
        root.join(project::scene_file(to)),
    )
    .map(|_| ())
    .map_err(|e| format!("couldn't copy '{from}': {e}"))
}

/// Rename a scene's file, and the start scene if it was this one.
pub(crate) fn rename_scene(root: &Path, from: &str, to: &str) -> Result<(), String> {
    check_new_name(root, to)?;
    std::fs::rename(
        root.join(project::scene_file(from)),
        root.join(project::scene_file(to)),
    )
    .map_err(|e| format!("couldn't rename '{from}': {e}"))?;
    let mut manifest = read_manifest(root);
    if manifest.start_scene == from {
        manifest.start_scene = to.to_string();
        write_manifest(root, &manifest);
    }
    Ok(())
}

/// Delete a scene's file. The last scene can't be deleted. If it was the
/// start scene, the first remaining scene becomes the start.
pub(crate) fn delete_scene(root: &Path, name: &str) -> Result<(), String> {
    let files = ProjectFiles::Folder(root.to_path_buf());
    let scenes = project::list_scenes(&files);
    if scenes.len() <= 1 {
        return Err("a project needs at least one scene".to_string());
    }
    std::fs::remove_file(root.join(project::scene_file(name)))
        .map_err(|e| format!("couldn't delete '{name}': {e}"))?;
    let mut manifest = read_manifest(root);
    if manifest.start_scene == name {
        manifest.start_scene = project::list_scenes(&files)
            .into_iter()
            .next()
            .unwrap_or_default();
        write_manifest(root, &manifest);
    }
    Ok(())
}

/// Make a scene the one the game starts on.
pub(crate) fn set_start_scene(root: &Path, name: &str) {
    let mut manifest = read_manifest(root);
    manifest.start_scene = name.to_string();
    write_manifest(root, &manifest);
}

fn check_new_name(root: &Path, name: &str) -> Result<(), String> {
    if !project::valid_scene_name(name) {
        return Err(format!(
            "'{name}' can't be a scene name: use letters, numbers, spaces, - or _"
        ));
    }
    if root.join(project::scene_file(name)).exists() {
        return Err(format!("there is already a scene called '{name}'"));
    }
    std::fs::create_dir_all(root.join(SCENES_DIR)).map_err(|e| e.to_string())
}

/// What the Scenes tab shows this frame.
pub(crate) struct ScenesView {
    pub scenes: Vec<String>,
    /// The scene open in the editor.
    pub open: Option<String>,
    /// The scene the game starts on.
    pub start: String,
    /// The open scene has unsaved changes.
    pub unsaved: bool,
}

/// The Scenes tab's own fields: a new scene's name, and the scene being
/// renamed, duplicated or deleted.
#[derive(Default)]
pub(crate) struct ScenesTabState {
    new_name: String,
    renaming: Option<(String, String)>,
    duplicating: Option<(String, String)>,
    confirm_delete: Option<String>,
}

/// What the user asked for in the Scenes tab.
pub(crate) enum SceneAction {
    Open(String),
    New(String),
    Duplicate(String, String),
    Rename(String, String),
    Delete(String),
    SetStart(String),
}

/// The Scenes tab: every scene in the project, the open one and the start
/// one marked, with buttons for each.
pub(crate) fn scenes_tab_ui(
    ui: &mut egui::Ui,
    view: &ScenesView,
    state: &mut ScenesTabState,
) -> Option<SceneAction> {
    let mut action = None;
    // The new-scene row sits above the list so it's in view however short
    // the console is.
    ui.horizontal(|ui| {
        ui.label("New scene");
        let response = ui.text_edit_singleline(&mut state.new_name);
        let enter = response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        let name = state.new_name.trim().to_string();
        if (ui
            .add_enabled(!name.is_empty(), egui::Button::new("Create"))
            .clicked()
            || enter)
            && !name.is_empty()
        {
            action = Some(SceneAction::New(name));
            state.new_name.clear();
        }
        if view.unsaved {
            ui.weak("Duplicate copies the scene as last saved.");
        }
    });
    ui.separator();
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            if view.scenes.is_empty() {
                ui.weak("No scenes yet.");
            }
            for name in &view.scenes {
                let is_open = view.open.as_deref() == Some(name.as_str());
                let is_start = view.start == *name;
                ui.horizontal(|ui| {
                    if let Some((target, text)) = state.renaming.as_mut().filter(|(t, _)| t == name) {
                        let response = ui.text_edit_singleline(text);
                        let enter = response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                        if ui.button("Rename").clicked() || enter {
                            action = Some(SceneAction::Rename(target.clone(), text.trim().to_string()));
                            state.renaming = None;
                        } else if ui.button("Cancel").clicked() {
                            state.renaming = None;
                        }
                        return;
                    }
                    if let Some((source, text)) = state.duplicating.as_mut().filter(|(s, _)| s == name) {
                        ui.label(format!("Copy of {name} as"));
                        let response = ui.text_edit_singleline(text);
                        let enter = response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                        if ui.button("Duplicate").clicked() || enter {
                            action = Some(SceneAction::Duplicate(source.clone(), text.trim().to_string()));
                            state.duplicating = None;
                        } else if ui.button("Cancel").clicked() {
                            state.duplicating = None;
                        }
                        return;
                    }
                    let label = if is_open {
                        egui::RichText::new(name).strong()
                    } else {
                        egui::RichText::new(name)
                    };
                    ui.label(label);
                    if is_start {
                        ui.weak("start scene");
                    }
                    if is_open {
                        ui.weak(if view.unsaved { "open, unsaved" } else { "open" });
                    }
                    // Plain left-to-right: a right-to-left layout here grows
                    // to the scroll area's height and pushes the row out of view.
                    ui.add_space(12.0);
                    if state.confirm_delete.as_deref() == Some(name.as_str()) {
                        ui.colored_label(egui::Color32::from_rgb(0xE0, 0x6C, 0x60), "Delete this scene?");
                        if ui.button("Delete file").clicked() {
                            action = Some(SceneAction::Delete(name.clone()));
                            state.confirm_delete = None;
                        }
                        if ui.button("Cancel").clicked() {
                            state.confirm_delete = None;
                        }
                        return;
                    }
                    if !is_open && ui.button("Open").clicked() {
                        action = Some(SceneAction::Open(name.clone()));
                    }
                    if !is_start && ui.button("Set as start").clicked() {
                        action = Some(SceneAction::SetStart(name.clone()));
                    }
                    if ui.button("Duplicate").clicked() {
                        state.duplicating = Some((name.clone(), format!("{name} copy")));
                    }
                    if ui
                        .add_enabled(!is_open, egui::Button::new("Rename"))
                        .on_disabled_hover_text("Open another scene first, or save this one under a new name with Duplicate")
                        .clicked()
                    {
                        state.renaming = Some((name.clone(), name.clone()));
                    }
                    let can_delete = !is_open && view.scenes.len() > 1;
                    if ui
                        .add_enabled(can_delete, egui::Button::new("Delete"))
                        .on_disabled_hover_text(if is_open {
                            "Open another scene first"
                        } else {
                            "A project needs at least one scene"
                        })
                        .clicked()
                    {
                        state.confirm_delete = Some(name.clone());
                    }
                });
            }
        });
    action
}

/// Bring in script files that changed outside the editor (in another text
/// editor, or from git) since the last load or save. `library` is what the
/// editor holds now, `saved` what it last read or wrote, `disk` what the
/// `scripts/` folder holds now. `skip` names scripts that aren't the
/// project's own (plugin scripts), which are left alone.
///
/// A script changed only outside is taken in. A script changed both outside
/// and in the editor is left as the editor has it, with a warning, since
/// saving will write over the outside change. Returns the lines to log.
pub(crate) fn merge_outside_edits(
    library: &mut std::collections::BTreeMap<String, String>,
    saved: &mut std::collections::BTreeMap<String, String>,
    disk: &std::collections::BTreeMap<String, String>,
    skip: impl Fn(&str) -> bool,
) -> Vec<String> {
    let mut log = Vec::new();
    let mut names: Vec<String> = disk.keys().chain(saved.keys()).cloned().collect();
    names.sort();
    names.dedup();
    for name in names.into_iter().filter(|n| !skip(n)) {
        let on_disk = disk.get(&name);
        let last = saved.get(&name);
        if on_disk == last {
            continue;
        }
        let edited_here = library.get(&name) != last;
        if edited_here {
            log.push(format!(
                "Script '{name}' changed outside the editor and here too. Kept the editor's version; saving will write over the outside change."
            ));
            continue;
        }
        match on_disk {
            Some(source) => {
                log.push(if last.is_some() {
                    format!("Reloaded script '{name}' (changed outside the editor)")
                } else {
                    format!("Added script '{name}' (new file outside the editor)")
                });
                library.insert(name.clone(), source.clone());
                saved.insert(name, source.clone());
            }
            None => {
                log.push(format!(
                    "Removed script '{name}' (file deleted outside the editor)"
                ));
                library.remove(&name);
                saved.remove(&name);
            }
        }
    }
    log
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("frame-scenes-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn an_old_project_converts_with_a_backup_and_everything_kept() {
        let root = temp("convert");
        let mut world = World::new();
        world
            .script_library
            .insert("door".into(), "let open = true;".into());
        world.globals.insert("difficulty".into(), 3.0);
        world.save_to_file(root.join("Ichor.ron")).unwrap();
        std::fs::write(
            root.join("project.ron"),
            "(description: \"A game\", version: \"0.1.0\")",
        )
        .unwrap();

        let done = convert_old_project(&root).unwrap();
        assert!(
            done.iter().any(|l| l.contains("Backed up Ichor.ron")),
            "{done:?}"
        );
        assert!(root.join("Ichor.ron.backup").is_file());
        assert!(!root.join("Ichor.ron").exists());
        assert!(root.join("scenes/Ichor.ron").is_file());
        assert_eq!(
            std::fs::read_to_string(root.join("scripts/door.rhai")).unwrap(),
            "let open = true;"
        );

        let manifest = read_manifest(&root);
        assert_eq!(manifest.name, "Ichor");
        assert_eq!(manifest.start_scene, "Ichor");
        assert_eq!(manifest.description, "A game");

        let files = ProjectFiles::Folder(root.clone());
        assert!(!project::is_old_layout(&files));
        let back = project::load_scene(&files, "Ichor").unwrap();
        assert_eq!(back.globals.get("difficulty"), Some(&3.0));
        assert_eq!(back.script_library["door"], "let open = true;");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn scene_files_are_created_copied_renamed_and_deleted_safely() {
        let root = temp("ops");
        create_scene(&root, "World", &World::new()).unwrap();
        set_start_scene(&root, "World");
        assert!(create_scene(&root, "World", &World::new()).is_err());
        assert!(create_scene(&root, "bad/name", &World::new()).is_err());

        duplicate_scene(&root, "World", "Cave").unwrap();
        rename_scene(&root, "World", "Overworld").unwrap();
        assert_eq!(read_manifest(&root).start_scene, "Overworld");

        let files = ProjectFiles::Folder(root.clone());
        assert_eq!(project::list_scenes(&files), ["Cave", "Overworld"]);
        delete_scene(&root, "Overworld").unwrap();
        assert_eq!(read_manifest(&root).start_scene, "Cave");
        assert!(delete_scene(&root, "Cave").is_err(), "the last scene stays");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn outside_script_edits_come_in_unless_edited_here_too() {
        use std::collections::BTreeMap;
        let map = |pairs: &[(&str, &str)]| -> BTreeMap<String, String> {
            pairs
                .iter()
                .map(|(a, b)| (a.to_string(), b.to_string()))
                .collect()
        };
        let mut saved = map(&[("a", "1"), ("b", "1"), ("c", "1"), ("p/x", "1")]);
        // b edited in the editor; everything else as saved.
        let mut library = map(&[("a", "1"), ("b", "2"), ("c", "1"), ("p/x", "1")]);
        // Outside: a and b changed, c deleted, d added, plugin script changed.
        let disk = map(&[("a", "9"), ("b", "9"), ("d", "1"), ("p/x", "9")]);
        let log = merge_outside_edits(&mut library, &mut saved, &disk, |n| n.starts_with("p/"));
        assert_eq!(
            library,
            map(&[("a", "9"), ("b", "2"), ("d", "1"), ("p/x", "1")])
        );
        assert_eq!(saved["a"], "9");
        assert_eq!(
            saved["b"], "1",
            "a conflict keeps the old saved copy, so it still shows unsaved"
        );
        assert!(!saved.contains_key("c"));
        assert_eq!(log.len(), 4);
        assert!(
            log.iter()
                .any(|l| l.contains("'b'") && l.contains("here too"))
        );
        // A second look only repeats the unresolved conflict.
        let log = merge_outside_edits(&mut library, &mut saved, &disk, |n| n.starts_with("p/"));
        assert_eq!(log.len(), 1);
    }
}
