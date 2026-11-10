//! Files dragged from one computer to the other: the transfer, and where the files land.
//!
//! **The flow** (the hub drives it, `hub/drag.rs`): the pointer crosses with files held in a drag. The computer they
//! come from announces them (`StreamMsg::DragStart`, top-level names only) and at once opens a transfer stream on the
//! same link and sends them. The other computer stages them in a folder of its own under the system temp folder. When
//! they are all there and the button is still held, it starts a real drag of the staged files under the pointer
//! (Windows) and the person drops them where they like; released early, refused, or on a computer that cannot start
//! a drag (macOS for now), they are moved to `Downloads/My Computer` and shown in the file manager.
//!
//! **The stream**: one kind byte (`KIND_TRANSFER`), a little-endian u32 length and the manifest as JSON (the token,
//! then every directory and file with its size, paths relative to the dragged items and joined by `/`), then the
//! bytes of each file in manifest order, nothing between them. QUIC already guarantees order and integrity.
//!
//! **Rules** (each has a test): a name from the other computer never leaves the staging folder (no `..`, no root, no
//! drive, characters Windows refuses become `_`); the receiver takes a stream only for the token it was told about
//! by the computer that is using it; at most `MAX_ENTRIES` entries and `MAX_TOTAL` bytes; a manifest that says more
//! than arrives, or that lies about a size, fails and its staging folder is removed.

use std::collections::BTreeSet;
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// The most entries (files and folders) one drag may carry.
pub const MAX_ENTRIES: usize = 20_000;
/// The most bytes one drag may carry.
pub const MAX_TOTAL: u64 = 64 * 1024 * 1024 * 1024;
/// The largest manifest accepted.
const MAX_MANIFEST: usize = 8 * 1024 * 1024;
/// The most top-level items named in `DragStart` (the control stream has small frames).
pub const MAX_ANNOUNCED: usize = 32;
const COPY_BUF: usize = 256 * 1024;
/// Staging folders older than this are removed when a new drag arrives.
const STAGING_MAX_AGE: Duration = Duration::from_secs(60 * 60);

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    /// Relative, `/`-joined; the first part is the dragged item's own name.
    pub rel: String,
    pub bytes: u64,
    pub dir: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    pub token: u64,
    pub entries: Vec<Entry>,
}

impl Manifest {
    pub fn total(&self) -> u64 {
        self.entries.iter().filter(|e| !e.dir).map(|e| e.bytes).sum()
    }

    /// The dragged items themselves (first part of every path), in order.
    pub fn roots(&self) -> Vec<String> {
        let mut seen = BTreeSet::new();
        let mut out = Vec::new();
        for e in &self.entries {
            let first = e.rel.split('/').next().unwrap_or_default().to_string();
            if seen.insert(first.clone()) {
                out.push(first);
            }
        }
        out
    }
}

/// Walks the dragged items. Returns the manifest and, for every file entry in order, where its bytes are read from.
/// Symbolic links are skipped (they could point anywhere).
pub fn build_manifest(token: u64, items: &[PathBuf]) -> Result<(Manifest, Vec<PathBuf>), String> {
    let mut entries = Vec::new();
    let mut sources = Vec::new();
    let mut names = BTreeSet::new();
    for item in items {
        let meta = std::fs::symlink_metadata(item).map_err(|e| format!("{}: {e}", item.display()))?;
        if meta.file_type().is_symlink() {
            continue;
        }
        let Some(name) = item.file_name().map(|n| n.to_string_lossy().to_string()) else { continue };
        // Two dragged items with the same name (from different folders): the second gets a number.
        let name = unique_name(&mut names, &name);
        walk(item, &name, &meta, &mut entries, &mut sources)?;
    }
    if entries.is_empty() {
        return Err("nothing to send".into());
    }
    let m = Manifest { token, entries };
    if m.total() > MAX_TOTAL {
        return Err("those files are too big to drag across".into());
    }
    Ok((m, sources))
}

fn unique_name(names: &mut BTreeSet<String>, name: &str) -> String {
    let mut n = name.to_string();
    let mut i = 2;
    while !names.insert(n.clone()) {
        n = format!("{name} ({i})");
        i += 1;
    }
    n
}

fn walk(path: &Path, rel: &str, meta: &std::fs::Metadata, entries: &mut Vec<Entry>, sources: &mut Vec<PathBuf>) -> Result<(), String> {
    if entries.len() >= MAX_ENTRIES {
        return Err(format!("more than {MAX_ENTRIES} files"));
    }
    if meta.is_dir() {
        entries.push(Entry { rel: rel.to_string(), bytes: 0, dir: true });
        let mut kids: Vec<_> = std::fs::read_dir(path).map_err(|e| format!("{}: {e}", path.display()))?.filter_map(Result::ok).collect();
        kids.sort_by_key(|k| k.file_name());
        for kid in kids {
            let Ok(m) = std::fs::symlink_metadata(kid.path()) else { continue };
            if m.file_type().is_symlink() {
                continue;
            }
            let child = format!("{rel}/{}", kid.file_name().to_string_lossy());
            walk(&kid.path(), &child, &m, entries, sources)?;
        }
    } else if meta.is_file() {
        entries.push(Entry { rel: rel.to_string(), bytes: meta.len(), dir: false });
        sources.push(path.to_path_buf());
    }
    Ok(())
}

/// One part of a name from the other computer, made safe to create here: characters Windows refuses (and control
/// characters) become `_`, trailing dots and spaces go. `None` for a part that must never be used (`.`, `..`, empty).
pub fn clean_part(part: &str) -> Option<String> {
    if part.is_empty() || part == "." || part == ".." {
        return None;
    }
    let mut s: String = part.chars().map(|c| if c.is_control() || "<>:\"/\\|?*".contains(c) { '_' } else { c }).collect();
    while s.ends_with('.') || s.ends_with(' ') {
        s.pop();
    }
    if s.is_empty() {
        return None;
    }
    // Device names Windows reserves (CON, NUL, COM1...) cannot be files there.
    let stem = s.split('.').next().unwrap_or_default().to_ascii_uppercase();
    let reserved = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || ((stem.starts_with("COM") || stem.starts_with("LPT")) && stem.len() == 4 && stem.as_bytes()[3].is_ascii_digit());
    if reserved {
        s.insert(0, '_');
    }
    Some(s)
}

/// `rel` from the other computer as a path under `root`, or `None` when it would leave it.
pub fn safe_join(root: &Path, rel: &str) -> Option<PathBuf> {
    let mut out = root.to_path_buf();
    for part in rel.split('/') {
        out.push(clean_part(part)?);
    }
    // Belt and braces: every component after the root is a plain name.
    let tail = out.strip_prefix(root).ok()?;
    if tail.components().all(|c| matches!(c, Component::Normal(_))) && tail.components().next().is_some() {
        Some(out)
    } else {
        None
    }
}

/// Sends the manifest and every file's bytes on `w` (a stream already marked `KIND_TRANSFER`).
pub async fn send<W: AsyncWrite + Unpin + ?Sized>(w: &mut W, manifest: &Manifest, sources: &[PathBuf], cancel: &std::sync::atomic::AtomicBool) -> Result<(), String> {
    let body = serde_json::to_vec(manifest).map_err(|e| e.to_string())?;
    if body.len() > MAX_MANIFEST {
        return Err("too many files".into());
    }
    w.write_all(&(body.len() as u32).to_le_bytes()).await.map_err(|e| e.to_string())?;
    w.write_all(&body).await.map_err(|e| e.to_string())?;
    let files = manifest.entries.iter().filter(|e| !e.dir);
    let mut buf = vec![0u8; COPY_BUF];
    for (entry, src) in files.zip(sources) {
        let mut f = tokio::fs::File::open(src).await.map_err(|e| format!("{}: {e}", src.display()))?;
        let mut left = entry.bytes;
        while left > 0 {
            if cancel.load(std::sync::atomic::Ordering::SeqCst) {
                return Err("cancelled".into());
            }
            let want = (left.min(COPY_BUF as u64)) as usize;
            let n = f.read(&mut buf[..want]).await.map_err(|e| e.to_string())?;
            if n == 0 {
                return Err(format!("{} got shorter while it was sent", src.display()));
            }
            w.write_all(&buf[..n]).await.map_err(|e| e.to_string())?;
            left -= n as u64;
        }
    }
    w.flush().await.map_err(|e| e.to_string())?;
    Ok(())
}

/// Reads the manifest only (the first thing on a transfer stream after its kind byte).
pub async fn read_manifest<R: AsyncRead + Unpin + ?Sized>(r: &mut R) -> Result<Manifest, String> {
    let mut len = [0u8; 4];
    r.read_exact(&mut len).await.map_err(|e| e.to_string())?;
    let n = u32::from_le_bytes(len) as usize;
    if n > MAX_MANIFEST {
        return Err("the list of files is too big".into());
    }
    let mut body = vec![0u8; n];
    r.read_exact(&mut body).await.map_err(|e| e.to_string())?;
    let m: Manifest = serde_json::from_slice(&body).map_err(|_| "the list of files could not be read".to_string())?;
    if m.entries.is_empty() || m.entries.len() > MAX_ENTRIES || m.total() > MAX_TOTAL {
        return Err("the list of files is out of bounds".into());
    }
    Ok(m)
}

/// Receives the files of `m` into `stage` (created here). Returns the staged top-level items. On any failure the
/// staging folder is removed.
pub async fn receive<R: AsyncRead + Unpin + ?Sized>(r: &mut R, m: &Manifest, stage: &Path) -> Result<Vec<PathBuf>, String> {
    let result = receive_inner(r, m, stage).await;
    if result.is_err() {
        let _ = tokio::fs::remove_dir_all(stage).await;
    }
    result
}

async fn receive_inner<R: AsyncRead + Unpin + ?Sized>(r: &mut R, m: &Manifest, stage: &Path) -> Result<Vec<PathBuf>, String> {
    tokio::fs::create_dir_all(stage).await.map_err(|e| e.to_string())?;
    let mut buf = vec![0u8; COPY_BUF];
    for e in &m.entries {
        let path = safe_join(stage, &e.rel).ok_or_else(|| format!("refused an unsafe file name: {}", e.rel))?;
        if e.dir {
            tokio::fs::create_dir_all(&path).await.map_err(|err| err.to_string())?;
            continue;
        }
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await.map_err(|err| err.to_string())?;
        }
        let mut f = tokio::fs::File::create(&path).await.map_err(|err| format!("{}: {err}", path.display()))?;
        let mut left = e.bytes;
        while left > 0 {
            let want = (left.min(COPY_BUF as u64)) as usize;
            let n = r.read(&mut buf[..want]).await.map_err(|err| err.to_string())?;
            if n == 0 {
                return Err("the other computer stopped sending".into());
            }
            f.write_all(&buf[..n]).await.map_err(|err| err.to_string())?;
            left -= n as u64;
        }
        f.flush().await.map_err(|err| err.to_string())?;
    }
    let mut roots = Vec::new();
    for name in m.roots() {
        if let Some(p) = safe_join(stage, &name) {
            roots.push(p);
        }
    }
    Ok(roots)
}

/// Where staging folders live: `<temp>/my-computer-drops`.
pub fn staging_root() -> PathBuf {
    std::env::temp_dir().join("my-computer-drops")
}

/// A fresh staging folder for one drag.
pub fn new_stage(token: u64) -> PathBuf {
    let nonce: u64 = rand::random();
    staging_root().join(format!("{token}-{nonce:016x}"))
}

/// Removes staging folders left from earlier drags (older than an hour: a drop target may still be copying a recent
/// one in the background).
pub fn sweep_staging() {
    let Ok(dir) = std::fs::read_dir(staging_root()) else { return };
    let now = SystemTime::now();
    for e in dir.filter_map(Result::ok) {
        let old = e.metadata().and_then(|m| m.modified()).map(|t| now.duration_since(t).unwrap_or_default() > STAGING_MAX_AGE).unwrap_or(false);
        if old {
            let _ = std::fs::remove_dir_all(e.path());
        }
    }
}

/// `Downloads/My Computer` in the person's home folder.
pub fn downloads_dir() -> Option<PathBuf> {
    let home = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })?;
    Some(PathBuf::from(home).join("Downloads").join("My Computer"))
}

/// Moves the staged items into `dest`, never over an existing file (a number is added). Returns where they went.
pub fn keep_in(items: &[PathBuf], dest: &Path) -> Result<Vec<PathBuf>, String> {
    std::fs::create_dir_all(dest).map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    for item in items {
        let Some(name) = item.file_name().map(|n| n.to_string_lossy().to_string()) else { continue };
        let target = free_path(dest, &name);
        if std::fs::rename(item, &target).is_err() {
            copy_tree(item, &target)?;
            let _ = if item.is_dir() { std::fs::remove_dir_all(item) } else { std::fs::remove_file(item) };
        }
        out.push(target);
    }
    Ok(out)
}

fn free_path(dest: &Path, name: &str) -> PathBuf {
    let first = dest.join(name);
    if !first.exists() {
        return first;
    }
    let p = Path::new(name);
    let (stem, ext) = match (p.file_stem(), p.extension()) {
        (Some(s), Some(e)) if !name.starts_with('.') => (s.to_string_lossy().to_string(), format!(".{}", e.to_string_lossy())),
        _ => (name.to_string(), String::new()),
    };
    (2..).map(|i| dest.join(format!("{stem} ({i}){ext}"))).find(|c| !c.exists()).unwrap_or(first)
}

fn copy_tree(from: &Path, to: &Path) -> Result<(), String> {
    if from.is_dir() {
        std::fs::create_dir_all(to).map_err(|e| e.to_string())?;
        for e in std::fs::read_dir(from).map_err(|e| e.to_string())?.filter_map(Result::ok) {
            copy_tree(&e.path(), &to.join(e.file_name()))?;
        }
        Ok(())
    } else {
        std::fs::copy(from, to).map(|_| ()).map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;

    fn tmp(name: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("pm-files-test-{name}-{:x}", rand::random::<u64>()));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn names_from_the_other_computer_never_leave_the_staging_folder() {
        let root = Path::new("/stage");
        assert_eq!(safe_join(root, "a/b.txt"), Some(root.join("a").join("b.txt")));
        for bad in ["../x", "a/../../x", "a/./b", "", "a//b", "/etc/passwd", "a/.."] {
            assert_eq!(safe_join(root, bad), None, "{bad}");
        }
        // Characters Windows refuses, a drive and a backslash become plain names inside the folder.
        let p = safe_join(root, "C:\\Windows\\x").unwrap();
        assert_eq!(p, root.join("C__Windows_x"));
        assert_eq!(safe_join(root, "notes.").unwrap(), root.join("notes"));
        assert_eq!(safe_join(root, "CON.txt").unwrap(), root.join("_CON.txt"));
        assert_eq!(safe_join(root, "COM10").unwrap(), root.join("COM10"));
    }

    #[test]
    fn the_roots_are_the_dragged_items_in_order() {
        let m = Manifest {
            token: 1,
            entries: vec![
                Entry { rel: "b".into(), bytes: 0, dir: true },
                Entry { rel: "b/x".into(), bytes: 3, dir: false },
                Entry { rel: "a.txt".into(), bytes: 2, dir: false },
            ],
        };
        assert_eq!(m.roots(), vec!["b".to_string(), "a.txt".to_string()]);
        assert_eq!(m.total(), 5);
    }

    #[tokio::test]
    async fn a_folder_and_a_file_arrive_byte_for_byte() {
        let src = tmp("src");
        std::fs::create_dir_all(src.join("folder/inner")).unwrap();
        std::fs::write(src.join("folder/inner/big.bin"), vec![7u8; 600_000]).unwrap();
        std::fs::write(src.join("folder/empty.txt"), b"").unwrap();
        std::fs::write(src.join("note.txt"), b"hello").unwrap();
        let (m, sources) = build_manifest(9, &[src.join("folder"), src.join("note.txt")]).unwrap();
        assert_eq!(m.roots(), vec!["folder".to_string(), "note.txt".to_string()]);

        let (mut a, mut b) = tokio::io::duplex(64 * 1024);
        let cancel = AtomicBool::new(false);
        let stage = tmp("stage").join("one");
        let (sent, got) = tokio::join!(
            async {
                let r = send(&mut a, &m, &sources, &cancel).await;
                drop(a);
                r
            },
            async {
                let m2 = read_manifest(&mut b).await.unwrap();
                assert_eq!(m2, m);
                receive(&mut b, &m2, &stage).await
            }
        );
        sent.unwrap();
        let roots = got.unwrap();
        assert_eq!(roots, vec![stage.join("folder"), stage.join("note.txt")]);
        assert_eq!(std::fs::read(stage.join("folder/inner/big.bin")).unwrap(), vec![7u8; 600_000]);
        assert_eq!(std::fs::read(stage.join("folder/empty.txt")).unwrap(), b"");
        assert_eq!(std::fs::read(stage.join("note.txt")).unwrap(), b"hello");
    }

    #[tokio::test]
    async fn a_transfer_cut_short_leaves_nothing_behind() {
        let m = Manifest { token: 2, entries: vec![Entry { rel: "x.bin".into(), bytes: 1000, dir: false }] };
        let mut short: &[u8] = &[1, 2, 3];
        let stage = tmp("short").join("s");
        assert!(receive(&mut short, &m, &stage).await.is_err());
        assert!(!stage.exists());
    }

    #[tokio::test]
    async fn an_unsafe_name_fails_the_whole_drag() {
        let m = Manifest { token: 3, entries: vec![Entry { rel: "../evil".into(), bytes: 0, dir: false }] };
        let mut none: &[u8] = &[];
        let stage = tmp("evil").join("s");
        assert!(receive(&mut none, &m, &stage).await.is_err());
        assert!(!stage.exists());
    }

    #[tokio::test]
    async fn a_manifest_out_of_bounds_is_refused() {
        let m = Manifest { token: 4, entries: vec![Entry { rel: "huge".into(), bytes: MAX_TOTAL + 1, dir: false }] };
        let body = serde_json::to_vec(&m).unwrap();
        let mut wire = (body.len() as u32).to_le_bytes().to_vec();
        wire.extend(body);
        assert!(read_manifest(&mut wire.as_slice()).await.is_err());
    }

    #[test]
    fn kept_files_never_overwrite_what_is_there() {
        let stage = tmp("keep-stage");
        let dest = tmp("keep-dest");
        std::fs::write(dest.join("a.txt"), b"old").unwrap();
        std::fs::write(stage.join("a.txt"), b"new").unwrap();
        let kept = keep_in(&[stage.join("a.txt")], &dest).unwrap();
        assert_eq!(kept, vec![dest.join("a (2).txt")]);
        assert_eq!(std::fs::read(dest.join("a.txt")).unwrap(), b"old");
        assert_eq!(std::fs::read(dest.join("a (2).txt")).unwrap(), b"new");
    }

    #[test]
    fn two_dragged_items_with_one_name_both_go() {
        let a = tmp("dup-a");
        let b = tmp("dup-b");
        std::fs::write(a.join("same.txt"), b"1").unwrap();
        std::fs::write(b.join("same.txt"), b"2").unwrap();
        let (m, _) = build_manifest(1, &[a.join("same.txt"), b.join("same.txt")]).unwrap();
        assert_eq!(m.roots(), vec!["same.txt".to_string(), "same.txt (2)".to_string()]);
    }
}
