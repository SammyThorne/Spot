//! AppImages: finding them, reading their name and icon, and starting them.
//!
//! A type 2 AppImage is an ELF "runtime" with a squashfs image appended. The squashfs starts
//! where the ELF's section header table ends. Metadata comes from reading that squashfs
//! in-process (the top-level `.desktop` file and icon), so indexing never executes the files
//! (`--appimage-extract` would). Results are cached per (path, size, mtime).
//!
//! Launching: GNOME has no AppImage integration, so a launch goes through a synthesized
//! desktop entry. That way GIO still hands over an XDG activation token and spawns the app in
//! its own systemd scope. Two runtime generations exist:
//! - the current static runtime (AppImage/type2-runtime) mounts via `fusermount3`;
//! - the old AppImageKit runtime `dlopen`s libfuse.so.2. Debian 13 / Ubuntu 24.04+ no longer
//!   install it by default, so those AppImages run with `APPIMAGE_EXTRACT_AND_RUN=1` instead
//!   (slower start, extracts to /tmp, but it works).

use std::collections::HashSet;
use std::fs::{self, File};
use std::io::{self, BufReader, Read, Seek, SeekFrom};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use backhand::{FilesystemReader, InnerNode, Node, SquashfsFileReader};
use gio::glib;
use gio::prelude::*;
use gio_unix::DesktopAppInfo;

use crate::entry::{AppImage, Entry, EntryKind};
use crate::LaunchError;

/// Bump when the cached metadata format or extraction logic changes.
const CACHE_VERSION: u32 = 1;
const MAX_DESKTOP_BYTES: u64 = 256 * 1024;
const MAX_ICON_BYTES: u64 = 4 * 1024 * 1024;
const MIN_APPIMAGE_BYTES: u64 = 8 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppImageType {
    /// ISO 9660 based, pre-2017. No metadata extraction; still launchable.
    Type1,
    /// ELF runtime + squashfs.
    Type2,
}

/// Folders scanned by default (non-recursive). `SPOT_APPIMAGE_DIRS` (colon-separated) replaces
/// the list; a settings UI comes later.
pub fn default_dirs() -> Vec<PathBuf> {
    if let Some(v) = std::env::var_os("SPOT_APPIMAGE_DIRS") {
        return std::env::split_paths(&v).filter(|p| !p.as_os_str().is_empty()).collect();
    }
    let home = glib::home_dir();
    let mut dirs = vec![home.join("Applications"), home.join("AppImages"), home.join(".local/bin")];
    for d in [glib::UserDirectory::Downloads, glib::UserDirectory::Desktop] {
        if let Some(p) = glib::user_special_dir(d) {
            if p != home {
                dirs.push(p);
            }
        }
    }
    dirs.dedup();
    dirs
}

pub fn default_cache_dir() -> PathBuf {
    glib::user_cache_dir().join("spot").join("appimages")
}

/// AppImage type from the magic bytes `AI\x01` / `AI\x02` at offset 8 of an ELF file.
pub fn detect(path: &Path) -> Option<AppImageType> {
    let mut head = [0u8; 11];
    File::open(path).ok()?.read_exact(&mut head).ok()?;
    if &head[..4] != b"\x7fELF" || &head[8..10] != b"AI" {
        return None;
    }
    match head[10] {
        1 => Some(AppImageType::Type1),
        2 => Some(AppImageType::Type2),
        _ => None,
    }
}

/// Offset of the appended squashfs: the end of the ELF section header table.
pub fn squashfs_offset(f: &mut (impl Read + Seek)) -> io::Result<u64> {
    let mut h = [0u8; 64];
    f.seek(SeekFrom::Start(0))?;
    f.read_exact(&mut h)?;
    let le = match h[5] {
        1 => true,
        2 => false,
        _ => return Err(invalid("unknown ELF byte order")),
    };
    let u16_at = |o: usize| {
        let b = [h[o], h[o + 1]];
        u64::from(if le { u16::from_le_bytes(b) } else { u16::from_be_bytes(b) })
    };
    let u32_at = |o: usize| {
        let b = h[o..o + 4].try_into().unwrap();
        u64::from(if le { u32::from_le_bytes(b) } else { u32::from_be_bytes(b) })
    };
    let u64_at = |o: usize| {
        let b = h[o..o + 8].try_into().unwrap();
        if le { u64::from_le_bytes(b) } else { u64::from_be_bytes(b) }
    };
    let (shoff, shentsize, shnum) = match h[4] {
        1 => (u32_at(0x20), u16_at(0x2E), u16_at(0x30)),
        2 => (u64_at(0x28), u16_at(0x3A), u16_at(0x3C)),
        _ => return Err(invalid("unknown ELF class")),
    };
    let offset = shoff + shentsize * shnum;
    let mut magic = [0u8; 4];
    f.seek(SeekFrom::Start(offset))?;
    f.read_exact(&mut magic)?;
    if &magic != b"hsqs" {
        return Err(invalid("no squashfs after the ELF runtime"));
    }
    Ok(offset)
}

/// What the AppImage says about itself.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Metadata {
    pub name: Option<String>,
    pub generic_name: Option<String>,
    pub comment: Option<String>,
    pub keywords: Vec<String>,
    pub wm_class: Option<String>,
    /// Extracted icon file (inside the cache).
    pub icon: Option<PathBuf>,
    pub needs_libfuse2: bool,
}

/// Read metadata from a type 2 AppImage; the icon (if any) is written into `icon_dir`.
pub fn read_metadata(path: &Path, icon_dir: &Path) -> io::Result<Metadata> {
    let mut r = BufReader::new(File::open(path)?);
    let offset = squashfs_offset(&mut r)?;
    let needs_libfuse2 = contains(&mut r, offset, b"libfuse.so.2")?;
    let fs = FilesystemReader::from_reader_with_offset(r, offset).map_err(|e| invalid(&e.to_string()))?;
    let nodes: Vec<&Node<SquashfsFileReader>> = fs.files().collect();

    let mut meta = Metadata { needs_libfuse2, ..Default::default() };
    let desktop = nodes
        .iter()
        .find(|n| {
            let p = rel(&n.fullpath);
            !p.contains('/') && p.ends_with(".desktop")
        })
        .and_then(|n| read_node(&fs, &nodes, n, MAX_DESKTOP_BYTES));
    let mut icon_name = None;
    if let Some(bytes) = desktop {
        let kf = glib::KeyFile::new();
        if kf.load_from_bytes(&glib::Bytes::from_owned(bytes), glib::KeyFileFlags::NONE).is_ok() {
            const G: &str = "Desktop Entry";
            let get = |k: &str| kf.locale_string(G, k, None).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
            meta.name = get("Name");
            meta.generic_name = get("GenericName");
            meta.comment = get("Comment");
            meta.wm_class = get("StartupWMClass");
            meta.keywords = kf
                .locale_string_list(G, "Keywords", None)
                .map(|l| l.iter().map(|s| s.to_string()).filter(|s| !s.is_empty()).collect())
                .unwrap_or_default();
            icon_name = get("Icon");
        }
    }

    // Icon: the desktop file's Icon= at the top level or in hicolor, else .DirIcon (spec: PNG).
    let mut candidates = Vec::new();
    if let Some(name) = &icon_name {
        let stem = Path::new(name).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        for ext in ["png", "svg", "xpm"] {
            candidates.push(format!("{stem}.{ext}"));
        }
        for size in ["256x256", "512x512", "128x128", "scalable", "64x64", "48x48"] {
            for ext in ["png", "svg"] {
                candidates.push(format!("usr/share/icons/hicolor/{size}/apps/{stem}.{ext}"));
            }
        }
    }
    candidates.push(".DirIcon".into());
    for c in candidates {
        let Some(node) = resolve(&nodes, &c, 0) else { continue };
        let Some(bytes) = read_node(&fs, &nodes, node, MAX_ICON_BYTES) else { continue };
        let ext = match Path::new(rel(&node.fullpath)).extension().and_then(|e| e.to_str()) {
            Some(e @ ("png" | "svg" | "xpm")) => e,
            _ if bytes.starts_with(b"<") => "svg",
            _ => "png",
        };
        let dest = icon_dir.join(format!("icon.{ext}"));
        fs::create_dir_all(icon_dir)?;
        fs::write(&dest, bytes)?;
        meta.icon = Some(dest);
        break;
    }
    Ok(meta)
}

/// Human-friendly name from a file name: `Bitwarden-2024.1.0-x86_64.AppImage` -> `Bitwarden`,
/// `LM-Studio-0.3.5.AppImage` -> `LM Studio`.
pub fn name_from_file_name(file_name: &str) -> String {
    let stem = file_name
        .strip_suffix(".AppImage")
        .or_else(|| file_name.strip_suffix(".appimage"))
        .or_else(|| file_name.strip_suffix(".APPIMAGE"))
        .unwrap_or(file_name);
    const ARCH: [&str; 8] = ["x86", "x64", "amd64", "aarch64", "arm64", "i386", "i686", "linux"];
    let mut words = Vec::new();
    for w in stem.split(['-', '_', ' ']).filter(|w| !w.is_empty()) {
        let lw = w.to_ascii_lowercase();
        let versionish = w.starts_with(|c: char| c.is_ascii_digit())
            || (lw.starts_with('v') && w[1..].starts_with(|c: char| c.is_ascii_digit()));
        if !words.is_empty() && (versionish || ARCH.contains(&lw.as_str())) {
            break;
        }
        words.push(w);
    }
    if words.is_empty() { stem.to_string() } else { words.join(" ") }
}

/// Find AppImages in `dirs`, skipping canonical paths in `skip` (ones that already have a
/// desktop entry). Metadata is cached under `cache_dir`; unused cache entries are removed.
pub fn scan(dirs: &[PathBuf], cache_dir: &Path, skip: &HashSet<PathBuf>) -> Vec<Entry> {
    let mut seen = HashSet::new();
    let mut used_keys = HashSet::new();
    let mut entries = Vec::new();
    for dir in dirs {
        let Ok(rd) = fs::read_dir(dir) else { continue };
        for de in rd.flatten() {
            let Ok(path) = de.path().canonicalize() else { continue };
            let Ok(md) = fs::metadata(&path) else { continue };
            if !md.is_file() || md.len() < MIN_APPIMAGE_BYTES || skip.contains(&path) || !seen.insert(path.clone()) {
                continue;
            }
            let Some(kind) = detect(&path) else { continue };
            let key = cache_key(&path, &md);
            used_keys.insert(key.clone());
            let meta = match kind {
                AppImageType::Type2 => cached_metadata(&path, &cache_dir.join(&key)),
                AppImageType::Type1 => Metadata::default(),
            };
            entries.push(entry(path, meta));
        }
    }
    gc_cache(cache_dir, &used_keys);
    entries
}

fn entry(path: PathBuf, meta: Metadata) -> Entry {
    let file_name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    Entry {
        id: format!("appimage:{}", path.display()),
        name: meta.name.unwrap_or_else(|| name_from_file_name(&file_name)),
        generic_name: meta.generic_name,
        comment: meta.comment,
        keywords: meta.keywords,
        exec_name: Some(file_name),
        icon: meta.icon.map(|p| p.to_string_lossy().into_owned()),
        kind: EntryKind::AppImage(AppImage { path, needs_libfuse2: meta.needs_libfuse2, wm_class: meta.wm_class }),
    }
}

fn cache_key(path: &Path, md: &fs::Metadata) -> String {
    let mtime = md.modified().ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map_or(0, |d| d.as_nanos());
    let id = format!("{CACHE_VERSION}\0{}\0{}\0{mtime}", path.display(), md.len());
    let sum = glib::compute_checksum_for_string(glib::ChecksumType::Sha256, &id).expect("sha256");
    sum[..24].to_string()
}

const INFO: &str = "info.ini";
const GROUP: &str = "AppImage";

fn cached_metadata(path: &Path, dir: &Path) -> Metadata {
    if let Some(m) = load_info(&dir.join(INFO)) {
        return m;
    }
    let _ = fs::remove_dir_all(dir);
    let (meta, error) = match read_metadata(path, dir) {
        Ok(m) => (m, None),
        Err(e) => (Metadata::default(), Some(e.to_string())),
    };
    // Written last: its presence marks a complete entry. Failures are cached too, so a broken
    // file isn't re-parsed on every scan (a changed file gets a new key anyway).
    let kf = glib::KeyFile::new();
    let opt = |k: &str, v: &Option<String>| {
        if let Some(v) = v {
            kf.set_string(GROUP, k, v);
        }
    };
    opt("Name", &meta.name);
    opt("GenericName", &meta.generic_name);
    opt("Comment", &meta.comment);
    opt("StartupWMClass", &meta.wm_class);
    opt("Icon", &meta.icon.as_ref().map(|p| p.to_string_lossy().into_owned()));
    opt("Error", &error);
    if !meta.keywords.is_empty() {
        kf.set_string(GROUP, "Keywords", &meta.keywords.join(";"));
    }
    kf.set_boolean(GROUP, "NeedsLibfuse2", meta.needs_libfuse2);
    if fs::create_dir_all(dir).is_ok() {
        let _ = kf.save_to_file(dir.join(INFO));
    }
    meta
}

fn load_info(file: &Path) -> Option<Metadata> {
    let kf = glib::KeyFile::new();
    kf.load_from_file(file, glib::KeyFileFlags::NONE).ok()?;
    let get = |k: &str| kf.string(GROUP, k).ok().map(|s| s.to_string());
    let icon = get("Icon").map(PathBuf::from).filter(|p| p.exists());
    Some(Metadata {
        name: get("Name"),
        generic_name: get("GenericName"),
        comment: get("Comment"),
        keywords: get("Keywords")
            .map(|k| k.split(';').filter(|s| !s.is_empty()).map(String::from).collect())
            .unwrap_or_default(),
        wm_class: get("StartupWMClass"),
        icon,
        needs_libfuse2: kf.boolean(GROUP, "NeedsLibfuse2").unwrap_or(false),
    })
}

fn gc_cache(cache_dir: &Path, used: &HashSet<String>) {
    let Ok(rd) = fs::read_dir(cache_dir) else { return };
    for de in rd.flatten() {
        if !used.contains(de.file_name().to_string_lossy().as_ref()) {
            let _ = fs::remove_dir_all(de.path());
        }
    }
}

// --- launching ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaunchMode {
    /// Normal: the runtime mounts the image with FUSE.
    Mount,
    /// `APPIMAGE_EXTRACT_AND_RUN=1`: no FUSE needed.
    ExtractAndRun,
}

pub fn launch_mode(app: &AppImage) -> LaunchMode {
    let fuse_ok = Path::new("/dev/fuse").exists()
        && (glib::find_program_in_path("fusermount3").is_some() || glib::find_program_in_path("fusermount").is_some());
    if !fuse_ok || (app.needs_libfuse2 && !libfuse2_installed()) {
        LaunchMode::ExtractAndRun
    } else {
        LaunchMode::Mount
    }
}

fn libfuse2_installed() -> bool {
    const DIRS: [&str; 8] = [
        "/usr/lib/x86_64-linux-gnu", "/lib/x86_64-linux-gnu", "/usr/lib/aarch64-linux-gnu",
        "/lib/aarch64-linux-gnu", "/usr/lib64", "/lib64", "/usr/lib", "/lib",
    ];
    DIRS.iter().any(|d| Path::new(d).join("libfuse.so.2").exists())
}

/// Desktop-entry `Exec=` quoting for a single absolute path.
pub fn exec_quote(path: &str) -> String {
    let mut s = String::with_capacity(path.len() + 2);
    s.push('"');
    for c in path.chars() {
        match c {
            '"' | '`' | '$' | '\\' => {
                s.push('\\');
                s.push(c);
            }
            '%' => s.push_str("%%"),
            _ => s.push(c),
        }
    }
    s.push('"');
    s
}

pub fn launch(app: &AppImage, name: &str, ctx: Option<&gio::AppLaunchContext>) -> Result<(), LaunchError> {
    let path = &app.path;
    let md = fs::metadata(path).map_err(|_| LaunchError::NotFound(path.display().to_string()))?;
    // Downloads usually lack the executable bit; picking the file in Spot is the user's go-ahead.
    let mode = md.permissions().mode();
    if mode & 0o100 == 0 {
        fs::set_permissions(path, fs::Permissions::from_mode(mode | 0o100)).map_err(LaunchError::Io)?;
    }

    let kf = glib::KeyFile::new();
    const G: &str = "Desktop Entry";
    kf.set_string(G, "Type", "Application");
    kf.set_string(G, "Name", name);
    kf.set_string(G, "Exec", &exec_quote(&path.to_string_lossy()));
    if let Some(wm) = &app.wm_class {
        kf.set_string(G, "StartupWMClass", wm);
    }
    let info = DesktopAppInfo::from_keyfile(&kf).ok_or_else(|| LaunchError::NotFound(path.display().to_string()))?;

    let ctx = ctx.cloned().unwrap_or_else(gio::AppLaunchContext::new);
    if launch_mode(app) == LaunchMode::ExtractAndRun {
        ctx.setenv("APPIMAGE_EXTRACT_AND_RUN", "1");
    }
    info.launch(&[], Some(&ctx)).map_err(LaunchError::Gio)
}

// --- squashfs helpers ----------------------------------------------------------------------

fn invalid(msg: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg.to_string())
}

/// Path inside the image without the leading `/`.
fn rel(p: &Path) -> &str {
    p.to_str().unwrap_or("").trim_start_matches('/')
}

fn resolve<'a>(nodes: &[&'a Node<SquashfsFileReader>], path: &str, depth: u8) -> Option<&'a Node<SquashfsFileReader>> {
    let node = *nodes.iter().find(|n| rel(&n.fullpath) == path)?;
    match &node.inner {
        InnerNode::File(_) => Some(node),
        InnerNode::Symlink(link) if depth < 8 => {
            let target = link.link.to_string_lossy();
            let target = if let Some(abs) = target.strip_prefix('/') {
                abs.to_string()
            } else {
                let parent = Path::new(path).parent().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default();
                normalize(&format!("{parent}/{target}"))
            };
            resolve(nodes, &target, depth + 1)
        }
        _ => None,
    }
}

fn normalize(p: &str) -> String {
    let mut out: Vec<&str> = Vec::new();
    for part in p.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                out.pop();
            }
            _ => out.push(part),
        }
    }
    out.join("/")
}

fn read_node(
    fs: &FilesystemReader,
    nodes: &[&Node<SquashfsFileReader>],
    node: &Node<SquashfsFileReader>,
    limit: u64,
) -> Option<Vec<u8>> {
    let node = resolve(nodes, rel(&node.fullpath), 0)?;
    let InnerNode::File(file) = &node.inner else { return None };
    let mut buf = Vec::new();
    fs.file(file).reader().take(limit + 1).read_to_end(&mut buf).ok()?;
    (buf.len() as u64 <= limit).then_some(buf)
}

/// Does `needle` occur in the first `len` bytes?
fn contains(r: &mut (impl Read + Seek), len: u64, needle: &[u8]) -> io::Result<bool> {
    let mut hay = Vec::with_capacity(len as usize);
    r.seek(SeekFrom::Start(0))?;
    r.take(len).read_to_end(&mut hay)?;
    Ok(hay.windows(needle.len()).any(|w| w == needle))
}

#[cfg(test)]
mod tests {
    use super::*;
    use backhand::{FilesystemCompressor, FilesystemWriter, NodeHeader};
    use std::io::Cursor;

    const PNG: &[u8] = b"\x89PNG\r\n\x1a\nfake icon bytes";

    /// A minimal ELF64 "runtime" padded to 4 KiB whose section table ends at 4096, then a
    /// squashfs with the given files/symlinks. Optionally mention libfuse.so.2 in the runtime.
    fn fake_appimage(files: &[(&str, &[u8])], links: &[(&str, &str)], libfuse2: bool, comp: backhand::compression::Compressor) -> Vec<u8> {
        let mut img = vec![0u8; 4096];
        img[..4].copy_from_slice(b"\x7fELF");
        img[4] = 2; // ELF64
        img[5] = 1; // little endian
        img[6] = 1;
        img[8..11].copy_from_slice(b"AI\x02");
        img[0x28..0x30].copy_from_slice(&4096u64.to_le_bytes()); // e_shoff
        img[0x3A..0x3C].copy_from_slice(&64u16.to_le_bytes()); // e_shentsize
        img[0x3C..0x3E].copy_from_slice(&0u16.to_le_bytes()); // e_shnum
        if libfuse2 {
            img[512..524].copy_from_slice(b"libfuse.so.2");
        }
        let mut w = FilesystemWriter::default();
        w.set_compressor(FilesystemCompressor::new(comp, None).unwrap());
        let h = NodeHeader::new(0o755, 0, 0, 0);
        for (path, data) in files {
            if let Some(parent) = Path::new(path).parent().filter(|p| !p.as_os_str().is_empty()) {
                let _ = w.push_dir_all(parent, h);
            }
            w.push_file(Cursor::new(data.to_vec()), path, h).unwrap();
        }
        for (path, target) in links {
            w.push_symlink(target, path, h).unwrap();
        }
        let mut cur = Cursor::new(img);
        w.write_with_offset(&mut cur, 4096).unwrap();
        cur.into_inner()
    }

    const DESKTOP: &[u8] = b"[Desktop Entry]\nType=Application\nName=Obsidian\nComment=Knowledge base\n\
        Keywords=notes;markdown;\nIcon=obsidian\nExec=AppRun %U\nStartupWMClass=obsidian\n";

    fn write(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
        let p = dir.join(name);
        fs::write(&p, bytes).unwrap();
        p
    }

    #[test]
    fn detects_type_and_finds_squashfs() {
        let d = tempfile::tempdir().unwrap();
        let p = write(d.path(), "a.AppImage", &fake_appimage(&[("AppRun", b"#!/bin/sh\n")], &[], false, backhand::compression::Compressor::Gzip));
        assert_eq!(detect(&p), Some(AppImageType::Type2));
        assert_eq!(squashfs_offset(&mut File::open(&p).unwrap()).unwrap(), 4096);
        let not = write(d.path(), "b", b"\x7fELF\x02\x01\x01\0\0\0\0 plain binary");
        assert_eq!(detect(&not), None);
        let text = write(d.path(), "c.AppImage", b"hello");
        assert_eq!(detect(&text), None);
    }

    #[test]
    fn reads_desktop_file_and_icon() {
        for comp in [backhand::compression::Compressor::Gzip, backhand::compression::Compressor::Zstd, backhand::compression::Compressor::Xz] {
            let d = tempfile::tempdir().unwrap();
            let img = fake_appimage(
                &[("obsidian.desktop", DESKTOP), ("obsidian.png", PNG), ("AppRun", b"#!/bin/sh\n")],
                &[(".DirIcon", "obsidian.png")],
                false,
                comp,
            );
            let p = write(d.path(), "Obsidian-1.5.3.AppImage", &img);
            let m = read_metadata(&p, &d.path().join("icon")).unwrap();
            assert_eq!(m.name.as_deref(), Some("Obsidian"), "{comp:?}");
            assert_eq!(m.comment.as_deref(), Some("Knowledge base"));
            assert_eq!(m.keywords, ["notes", "markdown"]);
            assert_eq!(m.wm_class.as_deref(), Some("obsidian"));
            assert!(!m.needs_libfuse2);
            assert_eq!(fs::read(m.icon.unwrap()).unwrap(), PNG);
        }
    }

    #[test]
    fn falls_back_to_dir_icon_through_symlinks() {
        let d = tempfile::tempdir().unwrap();
        let desktop = b"[Desktop Entry]\nType=Application\nName=Tool\nIcon=missing\nExec=AppRun\n";
        let img = fake_appimage(
            &[("tool.desktop", desktop), ("usr/share/pixmaps/tool.png", PNG)],
            &[(".DirIcon", "usr/share/pixmaps/tool.png")],
            true,
            backhand::compression::Compressor::Gzip,
        );
        let p = write(d.path(), "tool.AppImage", &img);
        let m = read_metadata(&p, &d.path().join("icon")).unwrap();
        assert_eq!(fs::read(m.icon.unwrap()).unwrap(), PNG);
        assert!(m.needs_libfuse2);
    }

    #[test]
    fn names_from_file_names() {
        assert_eq!(name_from_file_name("Bitwarden-2024.1.0-x86_64.AppImage"), "Bitwarden");
        assert_eq!(name_from_file_name("LM-Studio-0.3.5.AppImage"), "LM Studio");
        assert_eq!(name_from_file_name("nvim.appimage"), "nvim");
        assert_eq!(name_from_file_name("balenaEtcher-v1.18.11-x64.AppImage"), "balenaEtcher");
        assert_eq!(name_from_file_name("Cursor_x86_64.AppImage"), "Cursor");
        assert_eq!(name_from_file_name("2048.AppImage"), "2048");
    }

    #[test]
    fn scan_finds_caches_and_skips() {
        let d = tempfile::tempdir().unwrap();
        let apps = d.path().join("apps");
        fs::create_dir(&apps).unwrap();
        let cache = d.path().join("cache");
        let good = write(&apps, "Obsidian-1.5.3.AppImage",
            &fake_appimage(&[("o.desktop", DESKTOP), ("obsidian.png", PNG)], &[], false, backhand::compression::Compressor::Gzip));
        let mut broken = fake_appimage(&[("x", b"x")], &[], false, backhand::compression::Compressor::Gzip);
        broken.truncate(4200); // valid header and squashfs magic, garbage after it
        broken.resize(10_000, 0);
        write(&apps, "Broken-2.0.AppImage", &broken);
        write(&apps, "readme.txt", &[b'x'; 10_000]);
        std::os::unix::fs::symlink(&good, apps.join("obsidian")).unwrap(); // same file twice

        let mut found = scan(std::slice::from_ref(&apps), &cache, &HashSet::new());
        found.sort_by(|a, b| a.name.cmp(&b.name));
        let names: Vec<_> = found.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["Broken", "Obsidian"]);
        let obs = &found[1];
        assert_eq!(obs.kind_label(), Some("AppImage"));
        assert!(obs.icon.as_deref().unwrap().starts_with(cache.to_str().unwrap()));
        assert_eq!(fs::read_dir(&cache).unwrap().count(), 2);

        // Second scan is served from the cache (same result), and an integrated one is skipped.
        let again = scan(std::slice::from_ref(&apps), &cache, &HashSet::new());
        assert_eq!(again.len(), 2);
        let skip: HashSet<_> = [good.canonicalize().unwrap()].into();
        let skipped = scan(&[apps], &cache, &skip);
        assert_eq!(skipped.len(), 1);
        assert_eq!(fs::read_dir(&cache).unwrap().count(), 1, "stale cache entry removed");
    }

    #[test]
    fn exec_quoting_round_trips() {
        for p in ["/home/u/My Apps/a.AppImage", "/x/we\"ird $HOME `x` back\\slash 100%.AppImage"] {
            let unescaped_percent = exec_quote(p).replace("%%", "%");
            let argv = glib::shell_parse_argv(unescaped_percent).unwrap();
            assert_eq!(argv, [std::ffi::OsString::from(p)]);
        }
    }
}
