// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! A project's files, read either from its folder (while editing) or from
//! one pack file (in a game build).
//!
//! A pack holds a project's scene, `project.ron`, and its `assets/`,
//! `plugins/` and `prefabs/` folders in one file. Each file inside is
//! compressed (when that makes it smaller) and, if the game gives a
//! cipher, encrypted.
//!
//! Layout, all numbers little-endian:
//!
//! ```text
//! header   8 bytes  "FRAMEPAK"
//!          u32      PACK_FORMAT the pack was written with
//!          u32      flags (bit 0: sealed by the game's cipher)
//!          u64      where the index starts
//!          u64      the index's stored length
//! files    one stored block per file, back to back
//! index    the list of files (RON), stored like a file
//! ```
//!
//! A stored block is the file's bytes, deflated if `compressed` says so, and
//! then, in a sealed pack, passed through the game's `PackCipher`. The engine
//! has no encryption of its own: a game that wants its pack encrypted brings
//! a cipher, and the engine only says where it is used. Each file is sealed
//! with its own path as extra data, and the index with the header's first 16
//! bytes, so with a cipher that checks them (any authenticated one does), a
//! block moved to another name or a changed header fails to open instead of
//! reading as the wrong file.

use std::collections::BTreeMap;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};

/// The pack layout this engine writes. Raised when the layout changes, the
/// same way `SCENE_FORMAT` is for scenes. A pack from a newer engine is
/// refused with a message rather than misread.
pub const PACK_FORMAT: u32 = 1;

/// How a game seals its pack's files and opens them again. The game
/// supplies this; the engine never encrypts on its own. `aad` is extra data
/// the sealed bytes are tied to (a file's path, the pack header): an
/// authenticated cipher should refuse to open a block with different `aad`.
/// Whatever key the cipher uses lives in the game program, so it keeps a
/// pack's contents from being browsed casually; someone willing to take the
/// program apart can still find it.
pub trait PackCipher: Send + Sync {
    /// Seal `plain`, tied to `aad`.
    fn seal(&self, plain: &[u8], aad: &[u8]) -> std::io::Result<Vec<u8>>;
    /// Open what `seal` produced. An error if the key is wrong, the bytes
    /// were changed, or `aad` differs.
    fn open(&self, sealed: &[u8], aad: &[u8]) -> std::io::Result<Vec<u8>>;
}

const MAGIC: &[u8; 8] = b"FRAMEPAK";
const HEADER_LEN: u64 = 32;
const FLAG_SEALED: u32 = 1;

/// The top-level folders a pack takes from a project. Everything else in
/// the project folder (source control, notes, build output) is left out.
pub const PACKED_FOLDERS: &[&str] = &["assets", "plugins", "prefabs"];

#[derive(Clone, Debug, Serialize, Deserialize)]
struct PackEntry {
    offset: u64,
    stored: u64,
    size: u64,
    compressed: bool,
}

#[derive(Serialize, Deserialize)]
struct PackIndex {
    files: BTreeMap<String, PackEntry>,
}

/// An open pack file. Only the list of files is held in memory; each file
/// is read from disk when asked for.
pub struct Pack {
    path: PathBuf,
    cipher: Option<Arc<dyn PackCipher>>,
    files: BTreeMap<String, PackEntry>,
}

fn bad_data(message: impl Into<String>) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, message.into())
}

fn deflate(bytes: &[u8]) -> std::io::Result<Vec<u8>> {
    let mut encoder =
        flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(bytes)?;
    encoder.finish()
}

fn inflate(bytes: &[u8], size: u64) -> std::io::Result<Vec<u8>> {
    let mut out = Vec::with_capacity(size.min(64 << 20) as usize);
    // One byte past the expected size, so a block that inflates to more
    // than the index says is caught rather than read in full.
    flate2::read::DeflateDecoder::new(bytes)
        .take(size + 1)
        .read_to_end(&mut out)?;
    if out.len() as u64 != size {
        return Err(bad_data("a file in the pack is damaged"));
    }
    Ok(out)
}

impl Pack {
    /// Open a pack and read its list of files. A sealed pack needs the
    /// cipher it was written with; a plain pack ignores `cipher`.
    pub fn open(
        path: impl AsRef<Path>,
        cipher: Option<Arc<dyn PackCipher>>,
    ) -> std::io::Result<Pack> {
        let path = path.as_ref().to_path_buf();
        let mut file = std::fs::File::open(&path)?;
        let mut header = [0u8; HEADER_LEN as usize];
        file.read_exact(&mut header)
            .map_err(|_| bad_data("not a Frame Engine pack"))?;
        if &header[0..8] != MAGIC {
            return Err(bad_data("not a Frame Engine pack"));
        }
        let word = |at: usize| u32::from_le_bytes(header[at..at + 4].try_into().unwrap());
        let long = |at: usize| u64::from_le_bytes(header[at..at + 8].try_into().unwrap());
        let format = word(8);
        if format > PACK_FORMAT {
            return Err(bad_data(format!(
                "this pack was made by a newer Frame Engine (pack format {format}, this one reads up to {PACK_FORMAT})"
            )));
        }
        let sealed = word(12) & FLAG_SEALED != 0;
        let (index_at, index_len) = (long(16), long(24));
        let file_len = file.metadata()?.len();
        if index_at < HEADER_LEN || index_at.checked_add(index_len) != Some(file_len) {
            return Err(bad_data("the pack is damaged or cut short"));
        }
        let cipher = match (sealed, cipher) {
            (true, None) => return Err(bad_data("the pack is encrypted and no key was given")),
            (true, Some(cipher)) => Some(cipher),
            (false, _) => None,
        };
        file.seek(SeekFrom::Start(index_at))?;
        let mut stored = vec![0u8; index_len as usize];
        file.read_exact(&mut stored)?;
        let compressed = match &cipher {
            Some(cipher) => cipher.open(&stored, &header[0..16])?,
            None => stored,
        };
        let mut text = String::new();
        flate2::read::DeflateDecoder::new(compressed.as_slice())
            .read_to_string(&mut text)
            .map_err(|_| bad_data("the pack's file list is damaged"))?;
        let index: PackIndex =
            ron::from_str(&text).map_err(|_| bad_data("the pack's file list is damaged"))?;
        for entry in index.files.values() {
            if entry.offset < HEADER_LEN
                || entry
                    .offset
                    .checked_add(entry.stored)
                    .is_none_or(|end| end > index_at)
            {
                return Err(bad_data("the pack is damaged"));
            }
        }
        Ok(Pack {
            path,
            cipher,
            files: index.files,
        })
    }

    /// Whether the pack's files are encrypted.
    pub fn is_encrypted(&self) -> bool {
        self.cipher.is_some()
    }

    /// The pack file on disk.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Read one file from the pack, by its path inside the project
    /// (`assets/rock.obj`).
    pub fn read(&self, name: &str) -> std::io::Result<Vec<u8>> {
        let entry = self.files.get(name).ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("{name} is not in the pack"),
            )
        })?;
        let mut file = std::fs::File::open(&self.path)?;
        file.seek(SeekFrom::Start(entry.offset))?;
        let mut stored = vec![0u8; entry.stored as usize];
        file.read_exact(&mut stored)?;
        let plain = match &self.cipher {
            Some(cipher) => cipher.open(&stored, name.as_bytes())?,
            None => stored,
        };
        if entry.compressed {
            inflate(&plain, entry.size)
        } else if plain.len() as u64 == entry.size {
            Ok(plain)
        } else {
            Err(bad_data("a file in the pack is damaged"))
        }
    }
}

/// What `write_pack` put in a pack.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PackSummary {
    pub files: usize,
    /// Total size of the files before packing.
    pub original_bytes: u64,
    /// Size of the pack file.
    pub pack_bytes: u64,
}

/// Write a project folder into one pack file at `out`: the scene and any
/// other `.ron` files at the top of the project, and everything in
/// `PACKED_FOLDERS`. With a cipher, every file and the index are sealed. The pack is
/// written beside `out` first and moved into place at the end, so a failed
/// write never leaves half a pack behind under the real name.
pub fn write_pack(
    project: impl AsRef<Path>,
    out: impl AsRef<Path>,
    cipher: Option<&dyn PackCipher>,
) -> std::io::Result<PackSummary> {
    let project = ProjectFiles::Folder(project.as_ref().to_path_buf());
    let names: Vec<String> = project
        .files_under("")
        .into_iter()
        .filter(|name| match name.split_once('/') {
            None => name.ends_with(".ron"),
            Some((top, _)) => PACKED_FOLDERS.contains(&top),
        })
        .collect();
    if project.scene().is_none() {
        return Err(bad_data("that project has no scene to pack"));
    }
    let out = out.as_ref();
    let temp = out.with_extension("pack-writing");
    let result = write_pack_to(&project, &names, &temp, cipher);
    match result {
        Ok(summary) => {
            std::fs::rename(&temp, out)?;
            Ok(summary)
        }
        Err(e) => {
            let _ = std::fs::remove_file(&temp);
            Err(e)
        }
    }
}

fn write_pack_to(
    project: &ProjectFiles,
    names: &[String],
    path: &Path,
    cipher: Option<&dyn PackCipher>,
) -> std::io::Result<PackSummary> {
    let mut file = std::io::BufWriter::new(std::fs::File::create(path)?);
    let mut header = [0u8; HEADER_LEN as usize];
    header[0..8].copy_from_slice(MAGIC);
    header[8..12].copy_from_slice(&PACK_FORMAT.to_le_bytes());
    let flags = if cipher.is_some() { FLAG_SEALED } else { 0 };
    header[12..16].copy_from_slice(&flags.to_le_bytes());
    // The index position is filled in at the end.
    file.write_all(&header)?;
    let mut offset = HEADER_LEN;
    let mut files = BTreeMap::new();
    let mut original_bytes = 0;
    for name in names {
        let bytes = project.read(name)?;
        original_bytes += bytes.len() as u64;
        // Images and sounds are usually compressed already; keep a file as
        // it is when deflating would not make it smaller.
        let squeezed = deflate(&bytes)?;
        let compressed = squeezed.len() < bytes.len();
        let plain = if compressed { squeezed } else { bytes.clone() };
        let stored = match cipher {
            Some(cipher) => cipher.seal(&plain, name.as_bytes())?,
            None => plain,
        };
        file.write_all(&stored)?;
        files.insert(
            name.clone(),
            PackEntry {
                offset,
                stored: stored.len() as u64,
                size: bytes.len() as u64,
                compressed,
            },
        );
        offset += stored.len() as u64;
    }
    let count = files.len();
    let text = ron::to_string(&PackIndex { files }).map_err(|e| bad_data(e.to_string()))?;
    let squeezed = deflate(text.as_bytes())?;
    let index = match cipher {
        Some(cipher) => cipher.seal(&squeezed, &header[0..16])?,
        None => squeezed,
    };
    file.write_all(&index)?;
    let index_at = offset;
    let pack_bytes = index_at + index.len() as u64;
    let mut file = file.into_inner().map_err(|e| e.into_error())?;
    file.seek(SeekFrom::Start(16))?;
    file.write_all(&index_at.to_le_bytes())?;
    file.write_all(&(index.len() as u64).to_le_bytes())?;
    file.sync_all()?;
    Ok(PackSummary {
        files: count,
        original_bytes,
        pack_bytes,
    })
}

/// Where a project's files come from: its folder while editing, or a pack
/// in a game build. Everything that reads a project to play it goes through
/// this, so the same code runs either way. Paths are relative to the
/// project and always use `/` (`assets/sounds/step.wav`). Cheap to clone.
#[derive(Clone)]
pub enum ProjectFiles {
    Folder(PathBuf),
    Pack(Arc<Pack>),
}

impl ProjectFiles {
    /// Open a project: a folder is read as it is, a file is opened as a
    /// pack, with `cipher` if it is sealed.
    pub fn open(
        path: impl AsRef<Path>,
        cipher: Option<Arc<dyn PackCipher>>,
    ) -> std::io::Result<ProjectFiles> {
        let path = path.as_ref();
        if path.is_dir() {
            Ok(ProjectFiles::Folder(path.to_path_buf()))
        } else {
            Ok(ProjectFiles::Pack(Arc::new(Pack::open(path, cipher)?)))
        }
    }

    /// The folder or pack file, for messages.
    pub fn location(&self) -> &Path {
        match self {
            ProjectFiles::Folder(root) => root,
            ProjectFiles::Pack(pack) => pack.path(),
        }
    }

    /// The project folder, when the files come from one.
    pub fn folder(&self) -> Option<&Path> {
        match self {
            ProjectFiles::Folder(root) => Some(root),
            ProjectFiles::Pack(_) => None,
        }
    }

    /// Read a file's bytes.
    pub fn read(&self, name: &str) -> std::io::Result<Vec<u8>> {
        match self {
            ProjectFiles::Folder(root) => std::fs::read(root.join(name)),
            ProjectFiles::Pack(pack) => pack.read(name),
        }
    }

    /// Read a text file.
    pub fn read_to_string(&self, name: &str) -> std::io::Result<String> {
        String::from_utf8(self.read(name)?).map_err(|_| bad_data(format!("{name} is not text")))
    }

    /// Whether a file exists.
    pub fn is_file(&self, name: &str) -> bool {
        match self {
            ProjectFiles::Folder(root) => root.join(name).is_file(),
            ProjectFiles::Pack(pack) => pack.files.contains_key(name),
        }
    }

    /// Every file under `folder` (`""` for the whole project), at any
    /// depth, sorted. Names starting with `.` are skipped in a folder
    /// (`.git`, editor backups); a pack never holds them.
    pub fn files_under(&self, folder: &str) -> Vec<String> {
        let prefix = if folder.is_empty() {
            String::new()
        } else {
            format!("{}/", folder.trim_end_matches('/'))
        };
        let mut names = match self {
            ProjectFiles::Folder(root) => {
                let mut names = Vec::new();
                walk(&root.join(&prefix), &prefix, &mut names);
                names
            }
            ProjectFiles::Pack(pack) => pack
                .files
                .keys()
                .filter(|name| name.starts_with(&prefix))
                .cloned()
                .collect(),
        };
        names.sort();
        names
    }

    /// The project's scene: the first `.ron` file at the top of the project,
    /// by name, that isn't the `project.ron` manifest.
    pub fn scene(&self) -> Option<String> {
        self.files_under("")
            .into_iter()
            .find(|name| !name.contains('/') && name.ends_with(".ron") && name != "project.ron")
    }

    /// Find a file under `folder` by name. A path that exists as given
    /// (`audio/step.wav` under `assets`) wins; otherwise the first file with
    /// that file name anywhere under `folder`, in sorted order so the answer
    /// is the same every time. This is how sounds, textures and images are
    /// found, so they can be moved between asset folders freely.
    pub fn find_by_name(&self, folder: &str, name: &str) -> Option<String> {
        if name.is_empty() {
            return None;
        }
        let direct = if folder.is_empty() {
            name.replace('\\', "/")
        } else {
            format!(
                "{}/{}",
                folder.trim_end_matches('/'),
                name.replace('\\', "/")
            )
        };
        if self.is_file(&direct) {
            return Some(direct);
        }
        let file_name = name.rsplit(['/', '\\']).next()?;
        self.files_under(folder)
            .into_iter()
            .find(|path| path.rsplit('/').next() == Some(file_name))
    }
}

fn walk(dir: &Path, prefix: &str, names: &mut Vec<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let Some(name) = entry.file_name().to_str().map(String::from) else {
            continue;
        };
        if name.starts_with('.') {
            continue;
        }
        match entry.file_type() {
            Ok(t) if t.is_dir() => walk(&entry.path(), &format!("{prefix}{name}/"), names),
            Ok(t) if t.is_file() => names.push(format!("{prefix}{name}")),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stand-in cipher for the tests: XOR with one byte, plus a checksum
    /// of the key, `aad` and contents, so a wrong key, a changed byte or a
    /// different `aad` is caught the way a real authenticated cipher would.
    struct TestCipher(u8);

    impl TestCipher {
        fn check(&self, plain: &[u8], aad: &[u8]) -> u64 {
            let mut hash = 0xcbf2_9ce4_8422_2325u64;
            for b in [self.0].iter().chain(aad).chain(plain) {
                hash = (hash ^ *b as u64).wrapping_mul(0x100_0000_01b3);
            }
            hash
        }
    }

    impl PackCipher for TestCipher {
        fn seal(&self, plain: &[u8], aad: &[u8]) -> std::io::Result<Vec<u8>> {
            let mut out = self.check(plain, aad).to_le_bytes().to_vec();
            out.extend(plain.iter().map(|b| b ^ self.0));
            Ok(out)
        }
        fn open(&self, sealed: &[u8], aad: &[u8]) -> std::io::Result<Vec<u8>> {
            let (check, body) = sealed
                .split_at_checked(8)
                .ok_or_else(|| bad_data("short"))?;
            let plain: Vec<u8> = body.iter().map(|b| b ^ self.0).collect();
            if check != self.check(&plain, aad).to_le_bytes() {
                return Err(bad_data(
                    "the pack's key does not match, or the pack is damaged",
                ));
            }
            Ok(plain)
        }
    }

    fn cipher(key: u8) -> Arc<dyn PackCipher> {
        Arc::new(TestCipher(key))
    }

    fn project(tag: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("frame-pack-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for (name, bytes) in [
            ("Game.ron", b"(entities: [])".to_vec()),
            ("project.ron", b"(name: \"Game\")".to_vec()),
            ("notes.txt", b"left out".to_vec()),
            (
                "assets/models/rock.obj",
                "v 0 0 0\n".repeat(200).into_bytes(),
            ),
            ("assets/sounds/step.wav", (0..=255u8).collect()),
            (
                "plugins/weather/plugin.ron",
                b"(name: \"weather\")".to_vec(),
            ),
            (".git/HEAD", b"ref".to_vec()),
            ("target/big.bin", vec![7; 10]),
        ] {
            let path = root.join(name);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, bytes).unwrap();
        }
        root
    }

    #[test]
    fn a_pack_reads_back_the_same_files_as_the_folder() {
        let root = project("plain");
        let out = root.with_extension("pack");
        let summary = write_pack(&root, &out, None).unwrap();
        assert_eq!(summary.files, 5);
        let folder = ProjectFiles::open(&root, None).unwrap();
        let pack = ProjectFiles::open(&out, None).unwrap();
        let expected = vec![
            "Game.ron",
            "assets/models/rock.obj",
            "assets/sounds/step.wav",
            "plugins/weather/plugin.ron",
            "project.ron",
        ];
        assert_eq!(pack.files_under(""), expected);
        for name in &expected {
            assert_eq!(
                pack.read(name).unwrap(),
                folder.read(name).unwrap(),
                "{name}"
            );
        }
        assert_eq!(pack.scene().as_deref(), Some("Game.ron"));
        assert_eq!(pack.files_under("assets").len(), 2);
        assert_eq!(
            pack.find_by_name("assets", "step.wav").as_deref(),
            Some("assets/sounds/step.wav")
        );
        assert_eq!(
            folder.find_by_name("assets", "sounds/step.wav").as_deref(),
            Some("assets/sounds/step.wav")
        );
        assert!(pack.find_by_name("assets", "missing.wav").is_none());
        assert!(pack.read("notes.txt").is_err());
        // The repeated OBJ text is squeezed; the pack is smaller than its files.
        assert!(summary.pack_bytes < summary.original_bytes);
        std::fs::remove_dir_all(&root).ok();
        std::fs::remove_file(&out).ok();
    }

    #[test]
    fn an_encrypted_pack_needs_its_key_and_hides_its_contents() {
        let root = project("sealed");
        let out = root.with_extension("pack");
        write_pack(&root, &out, Some(&TestCipher(42))).unwrap();
        let raw = std::fs::read(&out).unwrap();
        let visible = |needle: &[u8]| raw.windows(needle.len()).any(|w| w == needle);
        assert!(!visible(b"rock.obj"));
        assert!(!visible(b"entities"));

        let pack = ProjectFiles::open(&out, Some(cipher(42))).unwrap();
        assert_eq!(pack.read("Game.ron").unwrap(), b"(entities: [])");
        let ProjectFiles::Pack(inner) = &pack else {
            panic!()
        };
        assert!(inner.is_encrypted());

        let no_key = Pack::open(&out, None).err().unwrap().to_string();
        assert!(no_key.contains("no key"), "{no_key}");
        let wrong = Pack::open(&out, Some(cipher(1))).err().unwrap().to_string();
        assert!(wrong.contains("key does not match"), "{wrong}");
        std::fs::remove_dir_all(&root).ok();
        std::fs::remove_file(&out).ok();
    }

    #[test]
    fn a_damaged_or_newer_pack_is_refused() {
        let root = project("damaged");
        let out = root.with_extension("pack");
        write_pack(&root, &out, Some(&TestCipher(9))).unwrap();
        let good = std::fs::read(&out).unwrap();

        // One byte changed inside a stored file: that file fails, others read.
        let mut flipped = good.clone();
        flipped[HEADER_LEN as usize + 20] ^= 1;
        std::fs::write(&out, &flipped).unwrap();
        let pack = Pack::open(&out, Some(cipher(9))).unwrap();
        let failures = pack
            .files
            .keys()
            .filter(|name| pack.read(name).is_err())
            .count();
        assert_eq!(failures, 1);

        // Cut short.
        std::fs::write(&out, &good[..good.len() - 5]).unwrap();
        assert!(Pack::open(&out, Some(cipher(9))).is_err());

        // From a newer engine.
        let mut newer = good.clone();
        newer[8..12].copy_from_slice(&(PACK_FORMAT + 1).to_le_bytes());
        std::fs::write(&out, &newer).unwrap();
        let err = Pack::open(&out, Some(cipher(9))).err().unwrap().to_string();
        assert!(err.contains("newer Frame Engine"), "{err}");

        // Not a pack at all.
        std::fs::write(&out, b"hello").unwrap();
        assert!(Pack::open(&out, None).is_err());
        std::fs::remove_dir_all(&root).ok();
        std::fs::remove_file(&out).ok();
    }

    #[test]
    fn a_project_without_a_scene_is_not_packed() {
        let root = std::env::temp_dir().join(format!("frame-pack-empty-{}", std::process::id()));
        std::fs::create_dir_all(root.join("assets")).unwrap();
        let out = root.with_extension("pack");
        assert!(write_pack(&root, &out, None).is_err());
        assert!(!out.exists());
        std::fs::remove_dir_all(&root).ok();
    }
}
