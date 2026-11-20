//! Homebrew from VitaDB (vitadb.rinnegatamante.it): the list of apps and the
//! download of one. The addresses are the ones VitaDB Downloader
//! (github.com/Rinnegatamante/VitaDB-Downloader, `source/network.cpp` and
//! `source/main.cpp`) uses: `vitadb/list_hbs_json.php` for the list and
//! `vitadb/get_hb_url.php?id=<id>` for a package.
//!
//! Both run on a worker thread (`VitaHost::storage_job`) over `http`, which
//! writes the answer to a file in the data folder. A package is checked by
//! `vpk::install_expecting`: it installs only when its own title id is the one
//! the list gave.

use std::fs;
use std::time::Duration;

use shelf_core::host::{CatalogApp, VpkProgress};

use super::gamework::valid_id;
use super::{http, vpk};

const LIST_URL: &str = "https://www.rinnegatamante.eu/vitadb/list_hbs_json.php";
const PACKAGE_URL: &str = "https://www.rinnegatamante.eu/vitadb/get_hb_url.php?id=";
/// Kept for the next time the list is read.
const LIST_FILE: &str = "vitadb/list.json";

/// Wait for request `id`; `progress` is called with the bytes received and
/// the total (0 when unknown) while it runs.
fn wait(id: i32, progress: impl Fn(u64, u64)) -> Result<(), String> {
    loop {
        let state = http::state(id);
        let mut words = state.split(' ');
        match words.next() {
            Some("busy") => {
                let received = words.next().and_then(|w| w.parse().ok()).unwrap_or(0);
                let total = words.next().and_then(|w| w.parse().ok()).unwrap_or(0);
                progress(received, total);
                std::thread::sleep(Duration::from_millis(100));
            }
            Some("done") => {
                let status: i32 = words.next().and_then(|w| w.parse().ok()).unwrap_or(0);
                return if status == 200 { Ok(()) } else { Err(format!("HTTP {status}")) };
            }
            _ => return Err(state.strip_prefix("error ").unwrap_or(&state).to_string()),
        }
    }
}

/// Download the list and read it.
pub fn fetch_list() -> Result<Vec<CatalogApp>, String> {
    let id = http::start(LIST_URL, "", Some(LIST_FILE), false);
    if id < 0 {
        return Err(String::from("no network"));
    }
    let waited = wait(id, |_, _| {});
    http::close(id);
    waited?;
    let bytes = fs::read(format!("{}/{LIST_FILE}", super::data_dir())).map_err(|_| String::from("file read"))?;
    parse(&bytes)
}

/// A JSON value as text: a string as it is, a number as written.
fn text(value: &serde_json::Value, key: &str) -> String {
    match value.get(key) {
        Some(serde_json::Value::String(text)) => text.trim().to_string(),
        Some(serde_json::Value::Number(number)) => number.to_string(),
        _ => String::new(),
    }
}

/// The list's entries with a usable title id; the installed version of each is
/// read from `ux0:app` here, on the worker thread.
pub fn parse(bytes: &[u8]) -> Result<Vec<CatalogApp>, String> {
    let value: serde_json::Value = serde_json::from_slice(bytes).map_err(|_| String::from("list is not JSON"))?;
    let items = value.as_array().ok_or_else(|| String::from("list is not an array"))?;
    let mut list = Vec::new();
    for item in items {
        let title_id = text(item, "titleid");
        let id = text(item, "id");
        if !valid_id(&title_id) || id.is_empty() || !id.bytes().all(|byte| byte.is_ascii_digit()) {
            continue;
        }
        list.push(CatalogApp {
            id,
            name: text(item, "name"),
            version: text(item, "version"),
            size: text(item, "size"),
            data: !text(item, "data").is_empty() || !text(item, "data_size").is_empty(),
            installed: vpk::installed_version(&title_id),
            title_id,
        });
    }
    if list.is_empty() {
        return Err(String::from("list has no apps"));
    }
    Ok(list)
}

/// Download the package with VitaDB id `id` and install it; the title id of
/// the installed app.
pub fn install(id: &str, title_id: &str) -> Result<String, String> {
    if !valid_id(title_id) || id.is_empty() || !id.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(String::from("title id"));
    }
    let file = format!("vitadb/{title_id}.vpk");
    let path = format!("{}/{file}", super::data_dir());
    let request = http::start(&format!("{PACKAGE_URL}{id}"), "", Some(&file), false);
    if request < 0 {
        return Err(String::from("no network"));
    }
    let waited = wait(request, |done, total| vpk::set(VpkProgress::Downloading(done, total)));
    http::close(request);
    let result = waited.and_then(|()| vpk::install_expecting(&path, Some(title_id)));
    vpk::set(VpkProgress::Idle);
    // The package is not kept: the card has room for one copy of an app.
    let _ = fs::remove_file(&path);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_list_reads_strings_and_numbers_and_skips_bad_entries() {
        let json = br#"[
            {"id": "12", "name": "Sample", "titleid": "SMPL00001", "version": "1.2", "size": "4.2 MB", "data": ""},
            {"id": 13, "name": "Numbers", "titleid": "NUMB00001", "version": 3, "size": "1 MB", "data": "x.zip"},
            {"id": "14", "name": "Bad id", "titleid": "short"},
            {"id": "x", "name": "Bad number", "titleid": "BADD00001"}
        ]"#;
        let list = parse(json).unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!((list[0].id.as_str(), list[0].version.as_str(), list[0].data), ("12", "1.2", false));
        assert_eq!((list[1].id.as_str(), list[1].version.as_str(), list[1].data), ("13", "3", true));
        assert!(parse(b"{}").is_err() && parse(b"nope").is_err() && parse(b"[]").is_err());
    }
}
