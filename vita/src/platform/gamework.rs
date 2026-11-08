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
//! - **Delete with data** also deletes `ux0:data/<id>` and `ux0:rePatch/<id>`.
//!   A homebrew that keeps its data under another name leaves that folder.

use std::ffi::CString;
use std::fs;
use std::sync::atomic::{AtomicBool, Ordering};

use shelf_core::host::GameWork;

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
fn valid_id(id: &str) -> bool {
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
    match work {
        GameWork::Shrink => shrink(id),
        GameWork::Delete => uninstall(id).map(|()| 0),
        GameWork::DeleteWithData => {
            uninstall(id)?;
            for folder in [format!("ux0:data/{id}"), format!("ux0:rePatch/{id}")] {
                if fs::metadata(&folder).is_ok() {
                    fs::remove_dir_all(&folder).map_err(|_| format!("remove {folder}"))?;
                }
            }
            Ok(0)
        }
    }
}

fn uninstall(id: &str) -> Result<(), String> {
    let name = CString::new(id).map_err(|_| String::from("title id"))?;
    unsafe {
        super::installed::close_app(&name);
        let _promoter = super::update::Promoter::open()?;
        let code = vitasdk_sys::scePromoterUtilityDeletePkg(name.as_ptr());
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
fn files(base: &str, rel: &str, out: &mut Vec<(String, u64)>) {
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
