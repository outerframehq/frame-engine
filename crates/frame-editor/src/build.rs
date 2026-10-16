// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! File > Build Game: turn the open project into a folder and a zip a player
//! can download.
//!
//! A build is made from real source, not copied from something lying
//! around: the game program is compiled with `cargo build` in the game's
//! workspace, so a build always matches the code as it is now. That means
//! Build only works on a machine with Rust and the game's source, which is
//! where builds are made. The steps run on a background thread and report
//! progress to the Build window:
//!
//! 1. `cargo build --bin <program>` (with `--release` unless debug was
//!    chosen) in the game's workspace.
//! 2. A fresh `<Name>-<version>-linux` folder in the output folder. An
//!    existing one is replaced only if Build made it (it holds a
//!    `.frame-build` marker), so Build never deletes a folder it didn't make.
//! 3. The program, `game.pack` (`frame_engine::pack::write_pack`, sealed
//!    with the game's cipher if encryption is on) and the engine's licence.
//! 4. A zip of that folder next to it, for sending.
//!
//! Which program to compile, where its workspace is and how to encrypt are
//! the game's to say, through `set_game_builder`. Without one, the menu item
//! is not shown.

use std::io::{Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc;

use frame_engine::pack::PackCipher;
use serde::{Deserialize, Serialize};

/// What a game tells the editor so it can make builds of it.
#[derive(Clone)]
pub struct GameBuilder {
    /// The game program's binary name in its Cargo workspace (`ichor`).
    pub program: String,
    /// The Cargo workspace holding that program. A game program usually
    /// passes its own source location, known when it is compiled:
    /// `concat!(env!("CARGO_MANIFEST_DIR"), "/../..")`.
    pub workspace: PathBuf,
    /// Seals the pack when encryption is on. `None` means this game's
    /// builds can't be encrypted, and the option is turned off.
    pub cipher: Option<Arc<dyn PackCipher>>,
}

static GAME_BUILDER: std::sync::OnceLock<GameBuilder> = std::sync::OnceLock::new();

/// Let this editor build a game: File > Build Game appears, compiling
/// `builder.program`. Call before `run_as`. Only the first call counts.
pub fn set_game_builder(builder: GameBuilder) {
    let _ = GAME_BUILDER.set(builder);
}

pub(crate) fn game_builder() -> Option<&'static GameBuilder> {
    GAME_BUILDER.get()
}

/// A project's build choices, saved in its `project.ron` under `build`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct BuildSettings {
    /// Where builds go. Empty means `<project folder>-builds` beside the
    /// project, so builds never land inside the project (or its git repo).
    pub output: String,
    /// Release (smaller, faster, what testers get) or debug.
    pub release: bool,
    /// Seal `game.pack` with the game's cipher.
    pub encrypt: bool,
}

impl Default for BuildSettings {
    fn default() -> Self {
        BuildSettings {
            output: String::new(),
            release: true,
            encrypt: true,
        }
    }
}

impl BuildSettings {
    /// The output folder these settings point at for a project.
    pub fn output_folder(&self, project: &Path) -> PathBuf {
        if !self.output.trim().is_empty() {
            return PathBuf::from(self.output.trim());
        }
        let name = project
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("game");
        project
            .parent()
            .unwrap_or(project)
            .join(format!("{name}-builds"))
    }
}

/// Everything one build needs, copied so the background thread owns it.
pub(crate) struct BuildRequest {
    pub project: PathBuf,
    /// The game's name (the project's name), used for the folder and zip.
    pub name: String,
    /// The game's version from `project.ron`.
    pub version: String,
    pub settings: BuildSettings,
    pub builder: GameBuilder,
}

/// What the build thread reports.
pub(crate) enum BuildEvent {
    /// A step started or cargo printed a line worth showing.
    Progress(String),
    /// The build finished: the zip it made, or what went wrong.
    Done(Result<BuildOutput, BuildError>),
}

#[derive(Clone, Debug)]
pub(crate) struct BuildOutput {
    pub folder: PathBuf,
    pub zip: PathBuf,
    pub seconds: f32,
}

#[derive(Clone, Debug)]
pub(crate) struct BuildError {
    pub message: String,
    /// The last lines cargo printed, when compiling is what failed.
    pub details: Vec<String>,
}

impl BuildError {
    fn new(message: impl Into<String>) -> Self {
        BuildError {
            message: message.into(),
            details: Vec::new(),
        }
    }
}

/// A build running on its own thread.
pub(crate) struct BuildJob {
    events: mpsc::Receiver<BuildEvent>,
}

impl BuildJob {
    pub fn start(request: BuildRequest) -> BuildJob {
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let started = std::time::Instant::now();
            let result = run_build(&request, &tx).map(|(folder, zip)| BuildOutput {
                folder,
                zip,
                seconds: started.elapsed().as_secs_f32(),
            });
            let _ = tx.send(BuildEvent::Done(result));
        });
        BuildJob { events: rx }
    }

    /// Everything reported since the last call. A thread that stopped
    /// without saying it was done reports a failure. Nothing is read after
    /// `Done`: the thread ends straight after sending it, and that ending
    /// must not be read as a crash.
    pub fn poll(&self) -> Vec<BuildEvent> {
        let mut events = Vec::new();
        loop {
            match self.events.try_recv() {
                Ok(event) => {
                    let done = matches!(event, BuildEvent::Done(_));
                    events.push(event);
                    if done {
                        break;
                    }
                }
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    events.push(BuildEvent::Done(Err(BuildError::new(
                        "the build stopped unexpectedly",
                    ))));
                    break;
                }
            }
        }
        events
    }
}

/// The platform name used in build folder names. Only the platform the
/// editor runs on, for now.
fn platform() -> &'static str {
    if cfg!(target_os = "windows") {
        "windows"
    } else if cfg!(target_os = "macos") {
        "macos"
    } else {
        "linux"
    }
}

/// Keep a name usable as a file name on every system.
fn file_safe(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || matches!(c, '-' | '_' | '.' | ' ') {
                c
            } else {
                '-'
            }
        })
        .collect();
    let trimmed = cleaned.trim().trim_matches('.').to_string();
    if trimmed.is_empty() {
        "game".to_string()
    } else {
        trimmed
    }
}

/// The folder name a build gets: `<Name>-<version>-<platform>`.
pub(crate) fn build_name(name: &str, version: &str) -> String {
    let version = version.trim();
    if version.is_empty() {
        format!("{}-{}", file_safe(name), platform())
    } else {
        format!("{}-{}-{}", file_safe(name), file_safe(version), platform())
    }
}

const MARKER: &str = ".frame-build";

const ENGINE_LICENSE: &str = include_str!("../../../LICENSE");

fn licence_notice() -> String {
    format!(
        "This game is built with Frame Engine {}, which is licensed under the\n\
         Mozilla Public License 2.0, below. The engine's source code is\n\
         available at https://github.com/outerframehq/frame-engine\n\n{}",
        frame_engine::version::ENGINE,
        ENGINE_LICENSE
    )
}

/// The cargo to run: the one that started this editor if it was started by
/// `cargo run`, else the usual install place, else whatever `cargo` the
/// system finds. An editor opened from a desktop menu often doesn't have
/// `~/.cargo/bin` on its path, hence the middle one.
fn cargo_command() -> std::process::Command {
    if let Some(cargo) = std::env::var_os("CARGO") {
        return std::process::Command::new(cargo);
    }
    if let Some(home) = dirs::home_dir() {
        let installed =
            home.join(".cargo")
                .join("bin")
                .join(if cfg!(windows) { "cargo.exe" } else { "cargo" });
        if installed.is_file() {
            return std::process::Command::new(installed);
        }
    }
    std::process::Command::new("cargo")
}

fn run_build(
    request: &BuildRequest,
    tx: &mpsc::Sender<BuildEvent>,
) -> Result<(PathBuf, PathBuf), BuildError> {
    let say = |text: String| {
        let _ = tx.send(BuildEvent::Progress(text));
    };
    let builder = &request.builder;
    let settings = &request.settings;
    if settings.encrypt && builder.cipher.is_none() {
        return Err(BuildError::new(
            "this game has no cipher, so its pack can't be encrypted. Turn encryption off.",
        ));
    }
    if !builder.workspace.join("Cargo.toml").is_file() {
        return Err(BuildError::new(format!(
            "the game's source isn't at {} any more, so it can't be compiled",
            builder.workspace.display()
        )));
    }

    // 1. Compile the game program.
    let profile = if settings.release { "release" } else { "debug" };
    say(format!("Compiling {} ({profile})…", builder.program));
    let program = compile(builder, settings.release, &say)?;

    // 2. A fresh build folder.
    let output = settings.output_folder(&request.project);
    let name = build_name(&request.name, &request.version);
    let folder = output.join(&name);
    say(format!("Making {}…", folder.display()));
    std::fs::create_dir_all(&output)
        .map_err(|e| BuildError::new(format!("couldn't make {}: {e}", output.display())))?;
    if folder.exists() {
        if !folder.join(MARKER).is_file() {
            return Err(BuildError::new(format!(
                "{} already exists and wasn't made by Build Game. Move or delete it, or choose another output folder.",
                folder.display()
            )));
        }
        std::fs::remove_dir_all(&folder).map_err(|e| {
            BuildError::new(format!(
                "couldn't replace the old {}: {e}",
                folder.display()
            ))
        })?;
    }
    std::fs::create_dir_all(&folder)
        .map_err(|e| BuildError::new(format!("couldn't make {}: {e}", folder.display())))?;
    std::fs::write(
        folder.join(MARKER),
        "Made by Frame Editor's Build Game. Build replaces this folder each time.\n",
    )
    .map_err(|e| BuildError::new(format!("couldn't write in {}: {e}", folder.display())))?;

    // 3. The program, the pack and the licence.
    let program_name = program
        .file_name()
        .map(|n| n.to_owned())
        .unwrap_or_else(|| builder.program.clone().into());
    let program_dest = folder.join(&program_name);
    say("Copying the game program…".to_string());
    std::fs::copy(&program, &program_dest)
        .map_err(|e| BuildError::new(format!("couldn't copy {}: {e}", program.display())))?;

    say(if settings.encrypt {
        "Packing and encrypting the project…".to_string()
    } else {
        "Packing the project…".to_string()
    });
    let cipher = if settings.encrypt {
        builder.cipher.as_deref()
    } else {
        None
    };
    let summary =
        frame_engine::pack::write_pack(&request.project, folder.join("game.pack"), cipher)
            .map_err(|e| BuildError::new(format!("couldn't pack the project: {e}")))?;
    say(format!("Packed {} files.", summary.files));

    std::fs::write(folder.join("LICENSE-Frame-Engine.txt"), licence_notice())
        .map_err(|e| BuildError::new(format!("couldn't write the licence: {e}")))?;

    // 4. Zip it.
    say("Zipping…".to_string());
    let zip = output.join(format!("{name}.zip"));
    let files = [
        (
            program_name.to_string_lossy().to_string(),
            program_dest,
            true,
        ),
        ("game.pack".to_string(), folder.join("game.pack"), false),
        (
            "LICENSE-Frame-Engine.txt".to_string(),
            folder.join("LICENSE-Frame-Engine.txt"),
            false,
        ),
    ];
    write_zip(&zip, &name, &files)
        .map_err(|e| BuildError::new(format!("couldn't write {}: {e}", zip.display())))?;
    Ok((folder, zip))
}

/// Run cargo, passing its progress on, and return the built program's path.
fn compile(
    builder: &GameBuilder,
    release: bool,
    say: &dyn Fn(String),
) -> Result<PathBuf, BuildError> {
    let mut command = cargo_command();
    command
        .current_dir(&builder.workspace)
        .arg("build")
        .arg("--bin")
        .arg(&builder.program)
        .arg("--color")
        .arg("never")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped());
    if release {
        command.arg("--release");
    }
    let mut child = command
        .spawn()
        .map_err(|e| BuildError::new(format!("couldn't run cargo (is Rust installed?): {e}")))?;
    let mut last_lines: Vec<String> = Vec::new();
    if let Some(stderr) = child.stderr.take() {
        let reader = std::io::BufReader::new(stderr);
        for line in std::io::BufRead::lines(reader).map_while(Result::ok) {
            let trimmed = line.trim();
            if trimmed.starts_with("Compiling")
                || trimmed.starts_with("Finished")
                || trimmed.starts_with("Blocking")
            {
                // "Compiling wgpu v29.0.3 (/long/path)" -> "Compiling wgpu v29.0.3"
                let short = trimmed.split(" (").next().unwrap_or(trimmed);
                say(short.to_string());
            }
            last_lines.push(line);
            if last_lines.len() > 40 {
                last_lines.remove(0);
            }
        }
    }
    let status = child
        .wait()
        .map_err(|e| BuildError::new(format!("cargo didn't finish: {e}")))?;
    if !status.success() {
        return Err(BuildError {
            message: format!("compiling {} failed", builder.program),
            details: last_lines,
        });
    }
    let target = std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .map(|dir| {
            if dir.is_absolute() {
                dir
            } else {
                builder.workspace.join(dir)
            }
        })
        .unwrap_or_else(|| builder.workspace.join("target"));
    let file = if cfg!(windows) {
        format!("{}.exe", builder.program)
    } else {
        builder.program.clone()
    };
    let program = target
        .join(if release { "release" } else { "debug" })
        .join(file);
    if !program.is_file() {
        return Err(BuildError::new(format!(
            "cargo finished but {} isn't there",
            program.display()
        )));
    }
    Ok(program)
}

/// The time now in the old DOS format zip files use.
fn dos_time() -> (u16, u16) {
    use chrono::{Datelike, Timelike};
    let now = chrono::Local::now();
    let time =
        ((now.hour() as u16) << 11) | ((now.minute() as u16) << 5) | (now.second() as u16 / 2);
    let year = (now.year().clamp(1980, 2107) - 1980) as u16;
    let date = (year << 9) | ((now.month() as u16) << 5) | now.day() as u16;
    (time, date)
}

/// Counts what passes through, so a compressed size is known without
/// holding the compressed file in memory.
struct Counting<W: Write> {
    inner: W,
    count: u64,
}

impl<W: Write> Write for Counting<W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let n = self.inner.write(buf)?;
        self.count += n as u64;
        Ok(n)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

/// Write a zip holding `files` inside one folder called `folder`. Each file
/// is `(name in the zip, file on disk, executable)`. Deflated with flate2,
/// streamed from disk, so a large program never sits in memory. A plain zip
/// without the 64-bit extension: every file and the whole zip must stay under
/// 4 GB, far above a game build today. The executable bit is kept, so the
/// program runs straight after unzipping on Linux.
///
/// Written by hand rather than with a zip library: writing (not reading) a
/// zip is three small record types, and flate2, already here for packs,
/// does the compressing and the checksum.
pub(crate) fn write_zip(
    path: &Path,
    folder: &str,
    files: &[(String, PathBuf, bool)],
) -> std::io::Result<()> {
    const LIMIT: u64 = u32::MAX as u64;
    let too_big = || std::io::Error::other("too big for a zip (over 4 GB)");
    let temp = path.with_extension("zip-writing");
    let result = (|| {
        let mut out = std::io::BufWriter::new(std::fs::File::create(&temp)?);
        let (time, date) = dos_time();
        // (name, crc, compressed, size, offset, mode, deflated)
        let mut entries: Vec<(String, u32, u64, u64, u64, u32, bool)> = Vec::new();
        let mut offset = 0u64;
        let local_header = |out: &mut dyn Write,
                            name: &str,
                            method: u16,
                            crc: u32,
                            compressed: u32,
                            size: u32|
         -> std::io::Result<()> {
            out.write_all(&0x0403_4b50u32.to_le_bytes())?;
            out.write_all(&20u16.to_le_bytes())?; // version needed
            out.write_all(&0x0800u16.to_le_bytes())?; // names are UTF-8
            out.write_all(&method.to_le_bytes())?;
            out.write_all(&time.to_le_bytes())?;
            out.write_all(&date.to_le_bytes())?;
            out.write_all(&crc.to_le_bytes())?;
            out.write_all(&compressed.to_le_bytes())?;
            out.write_all(&size.to_le_bytes())?;
            out.write_all(&(name.len() as u16).to_le_bytes())?;
            out.write_all(&0u16.to_le_bytes())?; // no extra field
            out.write_all(name.as_bytes())
        };

        // The folder itself, so unzipping makes it even with no files.
        let dir = format!("{folder}/");
        local_header(&mut out, &dir, 0, 0, 0, 0)?;
        entries.push((dir.clone(), 0, 0, 0, offset, 0o040755, false));
        offset += 30 + dir.len() as u64;

        for (name, source, executable) in files {
            let name = format!("{folder}/{name}");
            let header_at = offset;
            local_header(&mut out, &name, 8, 0, 0, 0)?;
            offset += 30 + name.len() as u64;
            let mut reader = flate2::CrcReader::new(std::fs::File::open(source)?);
            let mut encoder = flate2::write::DeflateEncoder::new(
                Counting {
                    inner: &mut out,
                    count: 0,
                },
                flate2::Compression::default(),
            );
            let size = std::io::copy(&mut reader, &mut encoder)?;
            let compressed = encoder.finish()?.count;
            let crc = reader.crc().sum();
            if size > LIMIT || compressed > LIMIT {
                return Err(too_big());
            }
            // Go back and fill in the checksum and sizes, now they're known.
            out.flush()?;
            let file = out.get_mut();
            file.seek(SeekFrom::Start(header_at + 14))?;
            file.write_all(&crc.to_le_bytes())?;
            file.write_all(&(compressed as u32).to_le_bytes())?;
            file.write_all(&(size as u32).to_le_bytes())?;
            file.seek(SeekFrom::End(0))?;
            let mode = if *executable { 0o100755 } else { 0o100644 };
            entries.push((name, crc, compressed, size, header_at, mode, true));
            offset += compressed;
            if offset > LIMIT {
                return Err(too_big());
            }
        }

        let directory_at = offset;
        let mut directory_len = 0u64;
        for (name, crc, compressed, size, at, mode, deflated) in &entries {
            let mut record = Vec::new();
            record.extend_from_slice(&0x0201_4b50u32.to_le_bytes());
            record.extend_from_slice(&((3u16 << 8) | 20).to_le_bytes()); // made on Unix
            record.extend_from_slice(&20u16.to_le_bytes());
            record.extend_from_slice(&0x0800u16.to_le_bytes());
            record.extend_from_slice(&(if *deflated { 8u16 } else { 0 }).to_le_bytes());
            record.extend_from_slice(&time.to_le_bytes());
            record.extend_from_slice(&date.to_le_bytes());
            record.extend_from_slice(&crc.to_le_bytes());
            record.extend_from_slice(&(*compressed as u32).to_le_bytes());
            record.extend_from_slice(&(*size as u32).to_le_bytes());
            record.extend_from_slice(&(name.len() as u16).to_le_bytes());
            record.extend_from_slice(&[0; 8]); // extra, comment, disk, internal attributes
            record.extend_from_slice(&(mode << 16).to_le_bytes());
            record.extend_from_slice(&(*at as u32).to_le_bytes());
            record.extend_from_slice(name.as_bytes());
            out.write_all(&record)?;
            directory_len += record.len() as u64;
        }
        if directory_at + directory_len > LIMIT {
            return Err(too_big());
        }
        out.write_all(&0x0605_4b50u32.to_le_bytes())?;
        out.write_all(&[0; 4])?; // this disk, directory's disk
        out.write_all(&(entries.len() as u16).to_le_bytes())?;
        out.write_all(&(entries.len() as u16).to_le_bytes())?;
        out.write_all(&(directory_len as u32).to_le_bytes())?;
        out.write_all(&(directory_at as u32).to_le_bytes())?;
        out.write_all(&0u16.to_le_bytes())?; // no comment
        out.flush()?;
        out.get_ref().sync_all()
    })();
    match result {
        Ok(()) => std::fs::rename(&temp, path),
        Err(e) => {
            let _ = std::fs::remove_file(&temp);
            Err(e)
        }
    }
}

/// Open a folder in the system's file manager.
pub(crate) fn open_folder(folder: &Path) {
    let program = if cfg!(target_os = "windows") {
        "explorer"
    } else if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    let _ = std::process::Command::new(program).arg(folder).spawn();
}

/// What the Build window shows, copied out of the editor for one frame.
pub(crate) struct BuildView {
    pub name: String,
    pub version: String,
    pub program: String,
    pub can_encrypt: bool,
    /// The open scene has unsaved changes; a build uses what's on disk.
    pub unsaved: bool,
    pub output_folder: PathBuf,
    pub running: bool,
    pub progress: Vec<String>,
    pub result: Option<Result<BuildOutput, BuildError>>,
}

/// What the user asked for in the Build window this frame.
pub(crate) enum BuildUiAction {
    Start,
    ChooseFolder,
    OpenFolder(PathBuf),
    SaveScene,
}

/// The File > Build Game window.
pub(crate) fn build_window_ui(
    ctx: &egui::Context,
    open: &mut bool,
    settings: &mut BuildSettings,
    view: &BuildView,
) -> Option<BuildUiAction> {
    let mut action = None;
    egui::Window::new("Build Game")
        .collapsible(false)
        .open(open)
        .default_width(460.0)
        .show(ctx, |ui| {
            egui::Grid::new("build-game-grid")
                .num_columns(2)
                .spacing([12.0, 8.0])
                .show(ui, |ui| {
                    ui.label("Game");
                    ui.label(format!(
                        "{} {}",
                        view.name,
                        if view.version.is_empty() { "(no version)" } else { &view.version }
                    ));
                    ui.end_row();
                    ui.label("");
                    ui.weak("Name and version come from the project. Change the version in the launcher's project Settings.");
                    ui.end_row();

                    ui.label("Platform");
                    ui.horizontal(|ui| {
                        let _ = ui.add_enabled(false, egui::RadioButton::new(true, platform_label()));
                        ui.weak("(more platforms later)");
                    });
                    ui.end_row();

                    ui.label("Build");
                    ui.horizontal(|ui| {
                        ui.add_enabled_ui(!view.running, |ui| {
                            ui.radio_value(&mut settings.release, true, "Release");
                            ui.radio_value(&mut settings.release, false, "Debug");
                        });
                    });
                    ui.end_row();
                    ui.label("");
                    ui.weak(if settings.release {
                        "Smaller and fast: what testers should get. The first release build takes several minutes."
                    } else {
                        "Much larger and slower, with readable crash messages. For chasing a tester's crash."
                    });
                    ui.end_row();

                    ui.label("Encrypt pack");
                    ui.add_enabled_ui(!view.running && view.can_encrypt, |ui| {
                        let mut on = settings.encrypt && view.can_encrypt;
                        if ui.checkbox(&mut on, "Encrypt game.pack").changed() {
                            settings.encrypt = on;
                        }
                    });
                    ui.end_row();

                    ui.label("Output folder");
                    ui.horizontal(|ui| {
                        ui.add_enabled_ui(!view.running, |ui| {
                            ui.add_sized(
                                [300.0, ui.spacing().interact_size.y],
                                egui::Label::new(short_path(&view.output_folder)).truncate(),
                            )
                            .on_hover_text(view.output_folder.display().to_string());
                            if ui.button("Choose…").clicked() {
                                action = Some(BuildUiAction::ChooseFolder);
                            }
                            if !settings.output.is_empty() && ui.button("Default").clicked() {
                                settings.output.clear();
                            }
                        });
                    });
                    ui.end_row();
                });

            ui.add_space(6.0);
            ui.weak(format!(
                "Makes {}/{} with {}, game.pack and the licence, and a zip of it to send.",
                short_path(&view.output_folder),
                build_name(&view.name, &view.version),
                view.program
            ));
            if view.unsaved {
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.colored_label(
                        egui::Color32::from_rgb(0xE0, 0xB0, 0x50),
                        "The scene has unsaved changes. A build uses what's saved.",
                    );
                    if !view.running && ui.button("Save scene").clicked() {
                        action = Some(BuildUiAction::SaveScene);
                    }
                });
            }
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(!view.running, egui::Button::new("Build"))
                    .clicked()
                {
                    action = Some(BuildUiAction::Start);
                }
                if view.running {
                    ui.spinner();
                    if let Some(line) = view.progress.last() {
                        ui.add(egui::Label::new(line).truncate());
                    }
                }
            });
            match &view.result {
                Some(Ok(output)) if !view.running => {
                    ui.add_space(6.0);
                    ui.colored_label(
                        egui::Color32::from_rgb(0x7F, 0xC0, 0x7A),
                        format!("Built in {:.0} s: {}", output.seconds, short_path(&output.zip)),
                    );
                    if ui.button("Open output folder").clicked() {
                        let folder = output.zip.parent().unwrap_or(&output.folder).to_path_buf();
                        action = Some(BuildUiAction::OpenFolder(folder));
                    }
                }
                Some(Err(error)) if !view.running => {
                    ui.add_space(6.0);
                    ui.colored_label(
                        egui::Color32::from_rgb(0xE0, 0x6C, 0x60),
                        format!("Build failed: {}", error.message),
                    );
                    if !error.details.is_empty() {
                        egui::ScrollArea::vertical()
                            .max_height(220.0)
                            .show(ui, |ui| {
                                ui.add(
                                    egui::Label::new(
                                        egui::RichText::new(error.details.join("\n")).monospace(),
                                    )
                                    .wrap(),
                                );
                            });
                    }
                }
                _ => {}
            }
        });
    action
}

/// A path with the home folder shown as `~`, to keep it short.
fn short_path(path: &Path) -> String {
    if let Some(home) = dirs::home_dir()
        && let Ok(rest) = path.strip_prefix(&home)
    {
        return format!("~/{}", rest.display());
    }
    path.display().to_string()
}

fn platform_label() -> &'static str {
    match platform() {
        "windows" => "Windows",
        "macos" => "macOS",
        _ => "Linux",
    }
}

/// Read a whole zip's file list back, for tests: (name, mode, contents).
#[cfg(test)]
fn read_zip(path: &Path) -> Vec<(String, u32, Vec<u8>)> {
    use std::io::Read;
    let data = std::fs::read(path).unwrap();
    let word = |at: usize| u32::from_le_bytes(data[at..at + 4].try_into().unwrap());
    let half = |at: usize| u16::from_le_bytes(data[at..at + 2].try_into().unwrap());
    let end = data.len() - 22;
    assert_eq!(word(end), 0x0605_4b50);
    let count = half(end + 10) as usize;
    let mut at = word(end + 16) as usize;
    let mut out = Vec::new();
    for _ in 0..count {
        assert_eq!(word(at), 0x0201_4b50);
        let method = half(at + 10);
        let crc = word(at + 16);
        let compressed = word(at + 20) as usize;
        let size = word(at + 24) as usize;
        let name_len = half(at + 28) as usize;
        let mode = word(at + 38) >> 16;
        let local = word(at + 42) as usize;
        let name = String::from_utf8(data[at + 46..at + 46 + name_len].to_vec()).unwrap();
        let start = local + 30 + half(local + 26) as usize + half(local + 28) as usize;
        let stored = &data[start..start + compressed];
        let contents = if method == 8 {
            let mut plain = Vec::new();
            flate2::read::DeflateDecoder::new(stored)
                .read_to_end(&mut plain)
                .unwrap();
            plain
        } else {
            stored.to_vec()
        };
        assert_eq!(contents.len(), size);
        let mut check = flate2::Crc::new();
        check.update(&contents);
        assert_eq!(check.sum(), crc, "{name}");
        out.push((name, mode, contents));
        at += 46 + name_len;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_zip_holds_the_folder_with_its_files_and_keeps_the_program_runnable() {
        let dir = std::env::temp_dir().join(format!("frame-build-zip-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let program: Vec<u8> = (0..300_000u32).map(|i| (i % 251) as u8).collect();
        std::fs::write(dir.join("game"), &program).unwrap();
        std::fs::write(dir.join("notes.txt"), "hello").unwrap();
        let zip = dir.join("out.zip");
        write_zip(
            &zip,
            "Game-1.0-linux",
            &[
                ("game".to_string(), dir.join("game"), true),
                ("notes.txt".to_string(), dir.join("notes.txt"), false),
            ],
        )
        .unwrap();
        let entries = read_zip(&zip);
        let names: Vec<&str> = entries.iter().map(|e| e.0.as_str()).collect();
        assert_eq!(
            names,
            [
                "Game-1.0-linux/",
                "Game-1.0-linux/game",
                "Game-1.0-linux/notes.txt"
            ]
        );
        assert_eq!(entries[1].1, 0o100755);
        assert_eq!(entries[1].2, program);
        assert_eq!(entries[2].1, 0o100644);
        assert_eq!(entries[2].2, b"hello");
        assert!(!dir.join("out.zip-writing").exists());
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A whole build of a tiny real program: compile, folder, pack, licence,
    /// zip; a second build replaces the first; a folder Build didn't make is
    /// left alone.
    #[test]
    fn a_build_compiles_packs_and_zips_and_only_replaces_its_own_folder() {
        let base = std::env::temp_dir().join(format!("frame-build-run-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let workspace = base.join("game-src");
        std::fs::create_dir_all(workspace.join("src")).unwrap();
        std::fs::write(
            workspace.join("Cargo.toml"),
            "[package]\nname = \"tiny\"\nversion = \"0.1.0\"\nedition = \"2021\"\n[workspace]\n",
        )
        .unwrap();
        std::fs::write(workspace.join("src/main.rs"), "fn main() {}\n").unwrap();
        let project = base.join("Tiny");
        std::fs::create_dir_all(project.join("assets")).unwrap();
        std::fs::write(project.join("Tiny.ron"), "()").unwrap();
        std::fs::write(project.join("assets/a.txt"), "asset").unwrap();
        let request = || BuildRequest {
            project: project.clone(),
            name: "Tiny".to_string(),
            version: "0.1.0".to_string(),
            settings: BuildSettings {
                output: String::new(),
                release: false,
                encrypt: false,
            },
            builder: GameBuilder {
                program: "tiny".to_string(),
                workspace: workspace.clone(),
                cipher: None,
            },
        };
        let (tx, rx) = mpsc::channel();
        let (folder, zip) = run_build(&request(), &tx).unwrap_or_else(|e| {
            panic!("{}: {}", e.message, e.details.join("\n"));
        });
        let name = build_name("Tiny", "0.1.0");
        assert_eq!(folder, base.join("Tiny-builds").join(&name));
        assert!(folder.join("tiny").is_file());
        assert!(folder.join(MARKER).is_file());
        let pack = frame_engine::pack::ProjectFiles::open(folder.join("game.pack"), None).unwrap();
        assert_eq!(pack.read("assets/a.txt").unwrap(), b"asset");
        let notice = std::fs::read_to_string(folder.join("LICENSE-Frame-Engine.txt")).unwrap();
        assert!(notice.contains("Mozilla Public License"));
        let names: Vec<String> = read_zip(&zip).into_iter().map(|e| e.0).collect();
        assert_eq!(
            names,
            [
                format!("{name}/"),
                format!("{name}/tiny"),
                format!("{name}/game.pack"),
                format!("{name}/LICENSE-Frame-Engine.txt"),
            ]
        );
        let said: Vec<String> = rx
            .try_iter()
            .filter_map(|e| match e {
                BuildEvent::Progress(line) => Some(line),
                _ => None,
            })
            .collect();
        assert!(
            said.iter().any(|l| l.starts_with("Compiling tiny")),
            "{said:?}"
        );

        // Again: the old build is replaced.
        std::fs::write(folder.join("stale.txt"), "old").unwrap();
        run_build(&request(), &tx).unwrap();
        assert!(!folder.join("stale.txt").exists());

        // A folder of the same name that Build didn't make is not touched.
        std::fs::remove_file(folder.join(MARKER)).unwrap();
        std::fs::write(folder.join("mine.txt"), "keep").unwrap();
        let err = run_build(&request(), &tx).err().unwrap();
        assert!(
            err.message.contains("wasn't made by Build Game"),
            "{}",
            err.message
        );
        assert!(folder.join("mine.txt").exists());

        // Encryption asked for with no cipher.
        let mut sealed = request();
        sealed.settings.encrypt = true;
        assert!(run_build(&sealed, &tx).is_err());
        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn a_finished_build_is_not_read_as_a_crash() {
        let (tx, rx) = mpsc::channel();
        tx.send(BuildEvent::Progress("Zipping…".to_string()))
            .unwrap();
        tx.send(BuildEvent::Done(Err(BuildError::new("real"))))
            .unwrap();
        drop(tx);
        let job = BuildJob { events: rx };
        let events = job.poll();
        assert_eq!(events.len(), 2);
        let BuildEvent::Done(Err(error)) = &events[1] else {
            panic!("expected the real result")
        };
        assert_eq!(error.message, "real");
    }

    #[test]
    fn build_names_are_safe_and_settings_default_to_release_and_encrypted() {
        let platform = platform();
        assert_eq!(build_name("Ichor", "0.1"), format!("Ichor-0.1-{platform}"));
        assert_eq!(build_name("My/Game?", ""), format!("My-Game--{platform}"));
        let old: BuildSettings = ron::from_str("()").unwrap();
        assert!(old.release && old.encrypt && old.output.is_empty());
        let project = Path::new("/home/me/Dev/Ichor");
        assert_eq!(
            old.output_folder(project),
            Path::new("/home/me/Dev/Ichor-builds")
        );
    }
}
