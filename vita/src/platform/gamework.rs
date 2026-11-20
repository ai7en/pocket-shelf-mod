//! Shrink, Delete and Delete with data for an installed game (the editor's
//! three rows). Each runs on a worker thread: a game's folder holds
//! thousands of files, and the uninstall waits for the system.
//!
//! - **Shrink** follows ONEMenu's Shrink Game: it removes the game's manual
//!   (`ux0:app/<id>/sce_sys/manual`) and every file of `ux0:app/<id>` that
//!   also exists, at the same path, in `ux0:patch/<id>` or `ux0:rePatch/<id>`.
//!   `sce_sys`, `eboot.bin` and `sce_module` of the base game stay: the
//!   system reads them whether or not a patch replaces them. A game shrunk
//!   against `ux0:rePatch` does not start without the rePatch plugin.
//! - **Delete** uninstalls the bubble and the game through the system's
//!   promoter (`scePromoterUtilityDeletePkg`, the call the updater's cleanup
//!   uses), after closing the game when it runs.
//! - **Back up saves** copies `ux0:user/00/savedata/<id>` to `backup/savedata/<id>`
//!   in the data folder, replacing the last copy after the new one is whole.
//! - **Delete with data** also deletes `ux0:data/<id>` and `ux0:rePatch/<id>`.
//!   A homebrew that keeps its data under another name leaves that folder.

use std::ffi::CString;
use std::fs;
use std::sync::atomic::{AtomicBool, Ordering};

use shelf_core::host::GameWork;

use crate::host::log_line;

/// Where the files a patch replaces can be, beside `ux0:app`.
const PATCH_ROOTS: [&str; 2] = ["ux0:patch", "ux0:rePatch"];
/// Paths of the base game that stay, whatever a patch holds.
const KEEP: [&str; 3] = ["sce_sys", "eboot.bin", "sce_module"];

/// One job runs at a time.
static BUSY: AtomicBool = AtomicBool::new(false);

/// Take the busy flag; false while a job runs.
pub fn begin() -> bool {
    !BUSY.swap(true, Ordering::AcqRel)
}

/// Release the busy flag.
pub fn end() {
    BUSY.store(false, Ordering::Release);
}

/// A title id is nine ASCII letters or digits (`PCSA00069`, `VITASHELL`);
/// anything else never becomes part of a path here.
pub(super) fn valid_id(id: &str) -> bool {
    id.len() == 9 && id.bytes().all(|b| b.is_ascii_alphanumeric())
}

/// Do the work; the bytes freed (0 for a delete), or what failed.
pub fn run(id: &str, work: GameWork) -> Result<u64, String> {
    if !valid_id(id) {
        return Err(String::from("title id"));
    }
    if id == super::installed::SELF_TITLE_ID {
        return Err(String::from("this app"));
    }
    // Each step goes to the log before it runs, so a hang or a power-off
    // leaves the last step in the file.
    log_line(&format!("game work {work:?} {id}: start"));
    let result = match work {
        GameWork::Shrink => shrink(id),
        GameWork::BackupSaves => backup_saves(id),
        GameWork::Delete => uninstall(id).map(|()| 0),
        GameWork::DeleteWithData => {
            uninstall(id)?;
            for folder in [format!("ux0:data/{id}"), format!("ux0:rePatch/{id}")] {
                if fs::metadata(&folder).is_ok() {
                    log_line(&format!("game work {work:?} {id}: removing {folder}"));
                    let removed = fs::remove_dir_all(&folder);
                    log_line(&format!("game work {work:?} {id}: {folder} -> {}", if removed.is_ok() { "removed" } else { "failed" }));
                    removed.map_err(|_| format!("remove {folder}"))?;
                } else {
                    log_line(&format!("game work {work:?} {id}: {folder} not there"));
                }
            }
            Ok(0)
        }
    };
    match &result {
        Ok(bytes) => log_line(&format!("game work {work:?} {id}: done, {bytes} bytes freed")),
        Err(error) => log_line(&format!("game work {work:?} {id}: failed: {error}")),
    }
    result
}

/// The bytes in `ux0:app/<id>`, `ux0:data/<id>` and `ux0:rePatch/<id>`, `None` for a folder
/// that does not exist. Walks every file: worker thread only.
pub fn folder_sizes(id: &str) -> shelf_core::host::FolderSizes {
    if !valid_id(id) {
        return Default::default();
    }
    let size = |root: &str| {
        let folder = format!("{root}/{id}");
        fs::metadata(&folder).ok()?;
        let mut found = Vec::new();
        files(&folder, "", &mut found);
        Some(found.iter().map(|(_, bytes)| bytes).sum::<u64>())
    };
    shelf_core::host::FolderSizes { app: size("ux0:app"), data: size("ux0:data"), repatch: size("ux0:rePatch"), savedata: size("ux0:user/00/savedata") }
}

/// Copy `ux0:user/00/savedata/<id>` to `<data>/backup/savedata/<id>`, replacing the
/// last copy only after the new one is complete. The bytes copied.
fn backup_saves(id: &str) -> Result<u64, String> {
    let src = format!("ux0:user/00/savedata/{id}");
    fs::metadata(&src).map_err(|_| String::from("no saves"))?;
    let mut found = Vec::new();
    files(&src, "", &mut found);
    let total: u64 = found.iter().map(|(_, bytes)| bytes).sum();
    // The copy sits on the same card: it needs its size and some room.
    if super::storage::space().is_some_and(|(free, _)| free < total + 16 * 1024 * 1024) {
        return Err(String::from("not enough space"));
    }
    let root = format!("{}/backup/savedata", crate::host::DATA);
    let (new, last) = (format!("{root}/{id}.new"), format!("{root}/{id}"));
    if fs::metadata(&new).is_ok() {
        fs::remove_dir_all(&new).map_err(|_| String::from("remove old temp"))?;
    }
    for (rel, bytes) in &found {
        let to = format!("{new}/{rel}");
        if let Some((dir, _)) = to.rsplit_once('/') {
            fs::create_dir_all(dir).map_err(|_| format!("create {dir}"))?;
        }
        let copied = fs::copy(format!("{src}/{rel}"), &to).map_err(|_| format!("copy {rel}"))?;
        if copied != *bytes {
            return Err(format!("size of {rel}"));
        }
    }
    // The new copy is whole; only now does it take the last one's place.
    fs::create_dir_all(&new).map_err(|_| String::from("create backup"))?;
    if fs::metadata(&last).is_ok() {
        fs::remove_dir_all(&last).map_err(|_| String::from("remove last copy"))?;
    }
    fs::rename(&new, &last).map_err(|_| String::from("rename backup"))?;
    Ok(total)
}

fn uninstall(id: &str) -> Result<(), String> {
    let name = CString::new(id).map_err(|_| String::from("title id"))?;
    unsafe {
        log_line(&format!("game work {id}: closing the app"));
        super::installed::close_app(&name);
        let _promoter = super::update::Promoter::open()?;
        log_line(&format!("game work {id}: uninstalling through the promoter"));
        let code = vitasdk_sys::scePromoterUtilityDeletePkg(name.as_ptr());
        log_line(&format!("game work {id}: promoter returned 0x{:08x}", code as u32));
        if code < 0 {
            return Err(format!("uninstall 0x{:08x}", code as u32));
        }
    }
    // The promoter removes the app's folder; a leftover goes too.
    let app = format!("ux0:app/{id}");
    if fs::metadata(&app).is_ok() {
        let _ = fs::remove_dir_all(&app);
    }
    Ok(())
}

/// The files under `dir` as `(path relative to base, size)`, folders walked.
pub(super) fn files(base: &str, rel: &str, out: &mut Vec<(String, u64)>) {
    let dir = if rel.is_empty() { base.to_string() } else { format!("{base}/{rel}") };
    let Ok(entries) = fs::read_dir(&dir) else { return };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let path = if rel.is_empty() { name } else { format!("{rel}/{name}") };
        match entry.file_type() {
            Ok(kind) if kind.is_dir() => files(base, &path, out),
            Ok(_) => out.push((path, entry.metadata().map(|m| m.len()).unwrap_or(0))),
            Err(_) => {}
        }
    }
}

fn shrink(id: &str) -> Result<u64, String> {
    let base = format!("ux0:app/{id}");
    if fs::metadata(&base).is_err() {
        return Err(String::from("not installed"));
    }
    let mut freed = 0u64;
    // The manual.
    let manual = format!("{base}/sce_sys/manual");
    if fs::metadata(&manual).is_ok() {
        let mut found = Vec::new();
        files(&manual, "", &mut found);
        freed += found.iter().map(|(_, size)| size).sum::<u64>();
        fs::remove_dir_all(&manual).map_err(|_| String::from("remove manual"))?;
    }
    // The files a patch replaces.
    let mut own = Vec::new();
    files(&base, "", &mut own);
    for root in PATCH_ROOTS {
        let patch = format!("{root}/{id}");
        if fs::metadata(&patch).is_err() {
            continue;
        }
        for (path, size) in &own {
            let top = path.split('/').next().unwrap_or("");
            if KEEP.contains(&top) {
                continue;
            }
            if fs::metadata(format!("{patch}/{path}")).is_ok_and(|m| m.is_file()) && fs::remove_file(format!("{base}/{path}")).is_ok() {
                freed += size;
            }
        }
    }
    Ok(freed)
}
