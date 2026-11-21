//! SELECT ▸ System ▸ Storage on the Vita: the card's space, a check for
//! folders no installed game owns, the games' sizes, and Shrink for all.
//! Every walk of a folder runs on a worker thread (`host.rs`).
//!
//! - **Empty install**: `ux0:app/<id>` with no `eboot.bin`. Removing it is
//!   what a failed install needs before the game is installed again.
//! - **Patch**: `ux0:patch/<id>` or `ux0:rePatch/<id>` while `ux0:app/<id>`
//!   does not exist; removable (the patch downloads again with the game).
//! - **Data**: `ux0:data/<id>` of a title that is not installed. It may hold
//!   saves, so it is only listed. Only ids written in capitals count:
//!   `ux0:data/retroarch` is nine letters and no title.

use std::fs;

use shelf_core::host::{GameWork, Leftover, LeftoverKind};

use super::gamework::{files, valid_id};

/// The system's answer to the device query of `sceIoDevctl` (`SceIoDevInfo`
/// in vita-headers): the sizes in bytes and the cluster size.
#[repr(C)]
#[allow(dead_code)]
struct DevInfo {
    max_size: u64,
    free_size: u64,
    cluster_size: u32,
    unused: u32,
}

extern "C" {
    fn sceIoDevctl(dev: *const core::ffi::c_char, cmd: u32, indata: *const core::ffi::c_void, inlen: u32, outdata: *mut core::ffi::c_void, outlen: u32) -> i32;
}

/// Free and total bytes of `ux0:`.
pub fn space() -> Option<(u64, u64)> {
    let mut info = DevInfo { max_size: 0, free_size: 0, cluster_size: 0, unused: 0 };
    let code = unsafe {
        sceIoDevctl(c"ux0:".as_ptr(), 0x3001, core::ptr::null(), 0, (&mut info as *mut DevInfo).cast(), core::mem::size_of::<DevInfo>() as u32)
    };
    (code >= 0 && info.max_size > 0).then_some((info.free_size, info.max_size))
}

/// The ids of the folders in `root` that are written as a title id.
fn ids_in(root: &str, capitals: bool) -> Vec<String> {
    let Ok(entries) = fs::read_dir(root) else { return Vec::new() };
    let mut ids: Vec<String> = entries
        .flatten()
        .filter_map(|entry| entry.file_name().to_str().map(String::from))
        .filter(|name| valid_id(name) && (!capitals || name.bytes().all(|b| !b.is_ascii_lowercase())))
        .collect();
    ids.sort();
    ids
}

fn folder_bytes(folder: &str) -> u64 {
    let mut found = Vec::new();
    files(folder, "", &mut found);
    found.iter().map(|(_, bytes)| bytes).sum()
}

/// Whether `ux0:app/<id>` has what a game needs to start.
fn has_eboot(id: &str) -> bool {
    fs::metadata(format!("ux0:app/{id}/eboot.bin")).is_ok()
}

/// Look for [`Leftover`]s, in the order of their kinds and ids.
pub fn check() -> Vec<Leftover> {
    let apps = ids_in("ux0:app", false);
    let mut found = Vec::new();
    for id in &apps {
        if !has_eboot(id) {
            let bytes = folder_bytes(&format!("ux0:app/{id}"));
            found.push(Leftover { id: id.clone(), kind: LeftoverKind::EmptyApp, bytes });
        } else if fs::metadata(format!("ux0:app/{id}/sce_sys/param.sfo")).is_err() {
            let bytes = folder_bytes(&format!("ux0:app/{id}"));
            found.push(Leftover { id: id.clone(), kind: LeftoverKind::Damaged, bytes });
        }
    }
    let owned = |id: &String| apps.binary_search(id).is_ok();
    let mut patches: Vec<String> = ids_in("ux0:patch", true).into_iter().chain(ids_in("ux0:rePatch", true)).collect();
    patches.sort();
    patches.dedup();
    for id in patches.into_iter().filter(|id| !owned(id)) {
        let bytes = folder_bytes(&format!("ux0:patch/{id}")) + folder_bytes(&format!("ux0:rePatch/{id}"));
        found.push(Leftover { id, kind: LeftoverKind::Patch, bytes });
    }
    for id in ids_in("ux0:data", true).into_iter().filter(|id| !owned(id)) {
        let bytes = folder_bytes(&format!("ux0:data/{id}"));
        found.push(Leftover { id, kind: LeftoverKind::Data, bytes });
    }
    found
}

/// Remove the folders of removable findings, checking each again first:
/// the answer is `(removed, stayed)`. Never the app itself, and never a
/// game that has since been installed.
pub fn clean(targets: &[(String, LeftoverKind)]) -> (u32, u32) {
    let (mut removed, mut stayed) = (0, 0);
    for (id, kind) in targets {
        if !valid_id(id) || id == super::installed::SELF_TITLE_ID || !kind.removable() {
            stayed += 1;
            continue;
        }
        let folders: Vec<String> = match kind {
            LeftoverKind::EmptyApp if !has_eboot(id) => vec![format!("ux0:app/{id}")],
            LeftoverKind::Patch if fs::metadata(format!("ux0:app/{id}")).is_err() => vec![format!("ux0:patch/{id}"), format!("ux0:rePatch/{id}")],
            _ => {
                stayed += 1;
                continue;
            }
        };
        let mut ok = true;
        for folder in folders {
            if fs::metadata(&folder).is_ok() && fs::remove_dir_all(&folder).is_err() {
                ok = false;
            }
        }
        if ok {
            removed += 1;
        } else {
            stayed += 1;
        }
    }
    (removed, stayed)
}

/// Every installed game's bytes in `ux0:app`, largest first.
pub fn sizes() -> Vec<(String, u64)> {
    let mut sizes: Vec<(String, u64)> = ids_in("ux0:app", false)
        .into_iter()
        .filter(|id| id != super::installed::SELF_TITLE_ID)
        .map(|id| {
            let bytes = folder_bytes(&format!("ux0:app/{id}"));
            (id, bytes)
        })
        .collect();
    sizes.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    sizes
}

/// Shrink each game as the editor's Shrink does: the games that lost files
/// and the bytes freed. A game that fails stays as it was and is left out.
pub fn shrink_all(ids: &[String]) -> (u32, u64) {
    let (mut games, mut freed) = (0, 0);
    for id in ids {
        match super::gamework::run(id, GameWork::Shrink, "") {
            Ok(bytes) if bytes > 0 => {
                games += 1;
                freed += bytes;
            }
            Ok(_) => {}
            Err(error) => crate::platform::vita_log(format_args!("[storage] shrink {id}: {error}")),
        }
    }
    (games, freed)
}
