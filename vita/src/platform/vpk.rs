//! SELECT ▸ System ▸ Install VPK on the Vita: the packages in `ux0:install`
//! and the folders below it, each with the name and icon its `param.sfo` and
//! `icon0.png` give, and the install of one of them.
//!
//! A VPK is a ZIP of the package's files. Installing unpacks it into
//! `update`'s sibling folder `install/pkg` in the data folder, a file at a
//! time in 64 KiB pieces (a game's files are far larger than the RAM), checks
//! each file's CRC-32, and hands that folder to the system's package
//! installer, the call the self-update makes for its helper app
//! (`update::Promoter`). The unpacked copy is removed afterwards, so the card
//! needs twice the package's size free (`MARGIN` more).
//!
//! Both jobs run on a worker thread (`host.rs`); `progress` is what the
//! frame reads while one runs.

use std::ffi::CString;
use std::fs;
use std::io::{Read, Write};
use std::sync::Mutex;
use std::time::UNIX_EPOCH;

use miniz_oxide::inflate::stream::{inflate, InflateState};
use miniz_oxide::{DataFormat, MZError, MZFlush, MZStatus};
use shelf_core::host::{VpkInfo, VpkProgress};

use super::gamework::valid_id;
use super::installed::{sfo_string, SELF_TITLE_ID};
use super::update::{crc32_update, data_start, entries, read_entry, Entry, Promoter};

/// Where packages are looked for.
pub const INSTALL_DIR: &str = "ux0:install";
/// How many folders down the search goes.
const DEPTH: usize = 4;
/// Packages listed at most.
const LIMIT: usize = 400;
/// The largest `param.sfo` or `icon0.png` read for a preview.
const SMALL: usize = 4 * 1024 * 1024;
/// Free bytes kept beyond the unpacked copy and the installed one.
const MARGIN: u64 = 32 * 1024 * 1024;
/// One piece of a file read, inflated and written.
const PIECE: usize = 64 * 1024;

static PROGRESS: Mutex<VpkProgress> = Mutex::new(VpkProgress::Idle);

pub(super) fn set(state: VpkProgress) {
    if let Ok(mut current) = PROGRESS.lock() {
        *current = state;
    }
}

/// What the install is doing.
pub fn progress() -> VpkProgress {
    PROGRESS.lock().map(|state| *state).unwrap_or(VpkProgress::Idle)
}

fn cache_dir() -> String {
    format!("{}/install/icons", super::data_dir())
}

fn find(dir: &str, depth: usize, out: &mut Vec<String>) {
    let Ok(read) = fs::read_dir(dir) else { return };
    let mut names: Vec<String> = read.flatten().filter_map(|entry| entry.file_name().to_str().map(String::from)).collect();
    names.sort();
    for name in names {
        if out.len() >= LIMIT {
            return;
        }
        let path = format!("{dir}/{name}");
        let Ok(meta) = fs::metadata(&path) else { continue };
        if meta.is_dir() {
            if depth < DEPTH {
                find(&path, depth + 1, out);
            }
        } else if name.to_ascii_lowercase().ends_with(".vpk") {
            out.push(path);
        }
    }
}

/// The packages under `ux0:install`, each read for its name and icon.
pub fn scan() -> Vec<VpkInfo> {
    let mut paths = Vec::new();
    find(INSTALL_DIR, 0, &mut paths);
    let icons = cache_dir();
    let _ = fs::remove_dir_all(&icons);
    let _ = fs::create_dir_all(&icons);
    paths.iter().map(|path| inspect(path, &icons)).collect()
}

fn small(file: &mut fs::File, list: &[Entry], name: &str) -> Option<Vec<u8>> {
    let entry = list.iter().find(|entry| entry.name == name)?;
    if entry.size > SMALL || entry.compressed > SMALL {
        return None;
    }
    read_entry(file, entry).ok()
}

fn inspect(path: &str, icons: &str) -> VpkInfo {
    let file_name = path.rsplit('/').next().unwrap_or(path).to_string();
    let meta = fs::metadata(path).ok();
    let mut info = VpkInfo {
        path: path.to_string(),
        file: file_name,
        size: meta.as_ref().map_or(0, |m| m.len()),
        modified: meta.and_then(|m| m.modified().ok()).and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map_or(0, |d| d.as_secs()),
        ..Default::default()
    };
    let mut file = match fs::File::open(path) {
        Ok(file) => file,
        Err(_) => {
            info.problem = String::from("cannot open");
            return info;
        }
    };
    let list = match entries(&mut file) {
        Ok(list) => list,
        Err(reason) => {
            info.problem = reason;
            return info;
        }
    };
    match small(&mut file, &list, "sce_sys/param.sfo") {
        Some(sfo) => {
            info.title = sfo_string(&sfo, "TITLE").unwrap_or_default().replace('\n', " ");
            info.title_id = sfo_string(&sfo, "TITLE_ID").unwrap_or_default();
            info.version = sfo_string(&sfo, "APP_VER").unwrap_or_default();
        }
        None => info.problem = String::from("no param.sfo"),
    }
    if info.problem.is_empty() && !list.iter().any(|entry| entry.name == "eboot.bin") {
        info.problem = String::from("no eboot.bin");
    }
    if info.problem.is_empty() && !valid_id(&info.title_id) {
        info.problem = format!("title id {}", info.title_id);
    }
    if info.title_id == SELF_TITLE_ID {
        info.problem = String::from("this app");
    }
    if valid_id(&info.title_id) {
        info.installed = installed_version(&info.title_id);
    }
    if let Some(png) = small(&mut file, &list, "sce_sys/icon0.png") {
        let target = format!("{icons}/{:08x}.png", !crc32_update(!0, path.as_bytes()));
        if fs::write(&target, png).is_ok() {
            info.icon = Some(target);
        }
    }
    info
}

/// One file of the package, written to `out_path` in pieces and checked
/// against its CRC-32. `done` counts the bytes written across the package.
fn write_entry(file: &mut fs::File, entry: &Entry, out_path: &str, done: &mut u64, total: u64) -> Result<(), String> {
    data_start(file, entry)?;
    let damaged = || format!("{} damaged", entry.name);
    let mut out = fs::File::create(out_path).map_err(|_| format!("write {}", entry.name))?;
    let mut input = vec![0u8; PIECE];
    let mut output = vec![0u8; PIECE];
    let mut remaining = entry.compressed;
    let (mut crc, mut written) = (!0u32, 0usize);
    let mut put = |bytes: &[u8], crc: &mut u32, written: &mut usize, done: &mut u64| -> Result<(), String> {
        *crc = crc32_update(*crc, bytes);
        out.write_all(bytes).map_err(|_| format!("write {}", entry.name))?;
        *written += bytes.len();
        *done += bytes.len() as u64;
        set(VpkProgress::Unpacking(*done, total));
        Ok(())
    };
    match entry.method {
        0 => {
            while remaining > 0 {
                let piece = remaining.min(PIECE);
                file.read_exact(&mut input[..piece]).map_err(|_| format!("{} cut short", entry.name))?;
                put(&input[..piece], &mut crc, &mut written, done)?;
                remaining -= piece;
            }
        }
        8 => {
            let mut state = InflateState::new_boxed(DataFormat::Raw);
            let (mut have, mut at, mut stalls) = (0usize, 0usize, 0u32);
            loop {
                if at == have && remaining > 0 {
                    let piece = remaining.min(PIECE);
                    file.read_exact(&mut input[..piece]).map_err(|_| format!("{} cut short", entry.name))?;
                    (have, at, remaining) = (piece, 0, remaining - piece);
                }
                let result = inflate(&mut state, &input[at..have], &mut output, MZFlush::None);
                at += result.bytes_consumed;
                if result.bytes_written > 0 {
                    put(&output[..result.bytes_written], &mut crc, &mut written, done)?;
                }
                match result.status {
                    Ok(MZStatus::StreamEnd) => break,
                    Ok(_) | Err(MZError::Buf) => {
                        if result.bytes_consumed == 0 && result.bytes_written == 0 {
                            stalls += 1;
                            if stalls > 2 || (at == have && remaining == 0) {
                                return Err(damaged());
                            }
                        } else {
                            stalls = 0;
                        }
                    }
                    Err(_) => return Err(damaged()),
                }
            }
        }
        method => return Err(format!("{} method {method}", entry.name)),
    }
    if written != entry.size || !crc != entry.crc {
        return Err(damaged());
    }
    Ok(())
}

/// Install the package at `path`: the title id on success, else why not.
/// The `APP_VER` of the installed title `id` (empty when it has none); None when it is not installed.
pub(super) fn installed_version(id: &str) -> Option<String> {
    let folder = format!("ux0:app/{id}");
    fs::metadata(&folder).ok()?;
    Some(fs::read(format!("{folder}/sce_sys/param.sfo")).ok().and_then(|sfo| sfo_string(&sfo, "APP_VER")).unwrap_or_default())
}

/// Delete one package file; only a `.vpk` below the install folder.
pub fn delete(path: &str) -> bool {
    let ok = path.starts_with("ux0:install/") && path.to_ascii_lowercase().ends_with(".vpk") && !path.contains("..");
    ok && fs::remove_file(path).is_ok()
}

pub fn install(path: &str) -> Result<String, String> {
    install_expecting(path, None)
}

/// Install, refusing a package whose title id is not `expected`.
pub fn install_expecting(path: &str, expected: Option<&str>) -> Result<String, String> {
    set(VpkProgress::Unpacking(0, 0));
    let result = install_inner(path, expected);
    set(VpkProgress::Idle);
    result
}

fn install_inner(path: &str, expected: Option<&str>) -> Result<String, String> {
    let mut file = fs::File::open(path).map_err(|_| String::from("no vpk"))?;
    let list = entries(&mut file)?;
    let sfo = small(&mut file, &list, "sce_sys/param.sfo").ok_or_else(|| String::from("no param.sfo"))?;
    let title_id = sfo_string(&sfo, "TITLE_ID").unwrap_or_default();
    if !valid_id(&title_id) {
        return Err(format!("title id {title_id}"));
    }
    if title_id == SELF_TITLE_ID {
        return Err(String::from("this app"));
    }
    if expected.is_some_and(|expected| expected != title_id) {
        return Err(format!("package is for {title_id}"));
    }
    if !list.iter().any(|entry| entry.name == "eboot.bin") {
        return Err(String::from("no eboot.bin"));
    }
    let total: u64 = list.iter().filter(|entry| !entry.name.ends_with('/')).map(|entry| entry.size as u64).sum();
    if let Some((free, _)) = super::storage::space() {
        let need = total * 2 + MARGIN;
        if free < need {
            return Err(format!("space: {} MB more needed", (need - free) / (1024 * 1024) + 1));
        }
    }
    let dir = format!("{}/install/pkg", super::data_dir());
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).map_err(|_| String::from("mkdir"))?;
    let unpacked = unpack(&mut file, &list, &dir, total);
    let installed = unpacked.and_then(|_| promote(&dir));
    let _ = fs::remove_dir_all(&dir);
    installed.map(|_| title_id)
}

fn unpack(file: &mut fs::File, list: &[Entry], dir: &str, total: u64) -> Result<(), String> {
    let mut done = 0u64;
    let mut parent = String::new();
    for entry in list {
        let target = format!("{dir}/{}", entry.name.trim_end_matches('/'));
        if entry.name.ends_with('/') {
            fs::create_dir_all(&target).map_err(|_| String::from("mkdir"))?;
            continue;
        }
        if let Some((folder, _)) = target.rsplit_once('/') {
            if folder != parent {
                fs::create_dir_all(folder).map_err(|_| String::from("mkdir"))?;
                parent = folder.to_string();
            }
        }
        write_entry(file, entry, &target, &mut done, total)?;
    }
    if fs::metadata(format!("{dir}/eboot.bin")).map(|meta| meta.len()).unwrap_or(0) == 0 {
        return Err(String::from("no eboot.bin"));
    }
    Ok(())
}

fn promote(dir: &str) -> Result<(), String> {
    set(VpkProgress::Installing);
    let path = CString::new(dir).map_err(|_| String::from("path"))?;
    unsafe {
        let _promoter = Promoter::open()?;
        let code = vitasdk_sys::scePromoterUtilityPromotePkgWithRif(path.as_ptr(), 1);
        if code < 0 {
            return Err(format!("install 0x{:08x}", code as u32));
        }
    }
    Ok(())
}
