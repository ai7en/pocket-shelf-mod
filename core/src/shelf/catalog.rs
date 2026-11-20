//! SELECT ▸ Tools ▸ Homebrew (VitaDB): updates for installed homebrew and
//! missing homebrew that VitaDB (vitadb.rinnegatamante.it) lists. The host
//! downloads the list and a package on a worker thread; this panel lists what
//! matches, asks twice and hands the install to the host.
//!
//! An update is a catalog version that is a higher number than the installed
//! one (`newer`); versions that are not dotted numbers are not compared.

use std::collections::HashSet;

use super::chrome::Button;
use super::navigation::{Direction, Step as Nav};
use super::panel::{self, Drawer, Step};
use super::Shelf;
use crate::gfx::Gfx;
use crate::host::{CatalogApp, Host, ScanPhase, VpkProgress};
use crate::i18n::{fill, upper};
use crate::shapes::Symbol;
use crate::text::Role;

/// Rows shown at once.
const ROWS: usize = 6;

/// Why a catalog entry is listed.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Update,
    Missing,
}

pub struct Catalog {
    pub row: usize,
    /// The list is being fetched.
    pub loading: bool,
    /// The title id being downloaded and installed.
    pub installing: Option<String>,
    /// The title id a first press asked about.
    pub armed: Option<String>,
    pub note: String,
}

/// A version as dotted numbers (`1.20`, `v2.0.1`), without trailing zeros.
fn parts(version: &str) -> Option<Vec<u64>> {
    let version = version.trim().trim_start_matches(['v', 'V']);
    let mut list: Vec<u64> = version.split('.').map(|part| part.parse().ok()).collect::<Option<_>>()?;
    while list.last() == Some(&0) {
        list.pop();
    }
    Some(list)
}

/// Whether `catalog` is a higher version than `installed`; false when either
/// is not made of dotted numbers.
pub fn newer(catalog: &str, installed: &str) -> bool {
    matches!((parts(catalog), parts(installed)), (Some(a), Some(b)) if a > b)
}

/// How many catalog entries are updates for installed titles.
pub fn count_updates(list: &[CatalogApp]) -> usize {
    list.iter().filter(|app| app.installed.as_deref().is_some_and(|installed| newer(&app.version, installed))).count()
}

impl Shelf {
    /// The rows: updates first, then missing titles, each by name.
    fn catalog_rows(&self) -> Vec<(CatalogApp, Kind)> {
        let Some(list) = &self.catalog_list else { return Vec::new() };
        let missing: HashSet<String> = self.to_reinstall().into_iter().map(|(id, _)| id).collect();
        let mut rows: Vec<(CatalogApp, Kind)> = list
            .iter()
            .filter_map(|app| match &app.installed {
                Some(installed) if newer(&app.version, installed) => Some((app.clone(), Kind::Update)),
                None if missing.contains(&app.title_id) => Some((app.clone(), Kind::Missing)),
                _ => None,
            })
            .collect();
        rows.sort_by_key(|(app, kind)| (*kind == Kind::Missing, app.name.to_lowercase()));
        rows
    }

    pub(super) fn catalog_row_count(&self) -> usize {
        self.catalog_rows().len()
    }

    pub(super) fn open_catalog(&mut self, host: &mut dyn Host) {
        let t = self.t();
        let mut panel = Catalog { row: 0, loading: false, installing: None, armed: None, note: String::new() };
        // The list of this session is kept; Refresh fetches it again.
        if self.catalog_list.is_none() {
            if host.catalog_fetch() {
                panel.loading = true;
                panel.note = t.catalog_loading.to_string();
            } else {
                panel.note = fill(t.failed, &[("reason", "unavailable")]);
            }
        }
        self.catalog = Some(panel);
        self.drawer = panel::slide();
        self.drawer.set(1.0, self.now);
    }

    /// The list arrived, or the fetch failed.
    pub fn catalog_arrived(&mut self, result: Result<Vec<CatalogApp>, String>) {
        let t = self.t();
        let note = match result {
            Ok(list) => {
                self.updates = count_updates(&list);
                self.catalog_list = Some(list);
                String::new()
            }
            Err(reason) => fill(t.failed, &[("reason", &reason)]),
        };
        if let Some(panel) = &mut self.catalog {
            panel.loading = false;
            panel.note = note;
        }
    }

    /// A download and install finished. The titles are read again whether or
    /// not the panel is open.
    pub fn catalog_installed(&mut self, host: &mut dyn Host, title_id: &str, result: Result<String, String>) {
        let t = self.t();
        let name = self.catalog_list.iter().flatten().find(|app| app.title_id == title_id).map(|app| app.name.clone()).unwrap_or_else(|| title_id.to_string());
        let ok = result.is_ok();
        if ok {
            host.rescan_some(vec![ScanPhase::Apps]);
            if let Some(list) = &mut self.catalog_list {
                // The next fetch brings the real version; until then it is the catalog's.
                for app in list.iter_mut().filter(|app| app.title_id == title_id) {
                    app.installed = Some(app.version.clone());
                }
                self.updates = count_updates(list);
            }
        }
        if let Some(panel) = &mut self.catalog {
            panel.installing = None;
            panel.armed = None;
            panel.note = match result {
                Ok(_) => fill(t.catalog_done, &[("name", &name)]),
                Err(reason) => fill(t.failed, &[("reason", &reason)]),
            };
            let count = self.catalog_rows().len();
            if let Some(panel) = &mut self.catalog {
                panel.row = panel.row.min(count.saturating_sub(1));
            }
        }
    }

    /// The download's and the install's progress, once per frame.
    pub(super) fn catalog_frame(&mut self, host: &mut dyn Host) {
        let t = self.t();
        let Some(panel) = &mut self.catalog else { return };
        if panel.installing.is_none() {
            return;
        }
        panel.note = match host.vpk_progress() {
            VpkProgress::Downloading(done, total) if total > 0 => fill(t.catalog_downloading, &[("percent", &(done * 100 / total).to_string())]),
            VpkProgress::Downloading(..) => t.working.to_string(),
            VpkProgress::Unpacking(done, total) if total > 0 => fill(t.install_unpacking, &[("percent", &(done * 100 / total).to_string())]),
            VpkProgress::Unpacking(..) => t.working.to_string(),
            VpkProgress::Installing => t.install_installing.to_string(),
            VpkProgress::Idle => panel.note.clone(),
        };
    }

    pub(super) fn catalog_input(&mut self, host: &mut dyn Host, step: Option<Nav>, confirm: bool, refresh: bool, cancel: bool) {
        let t = self.t();
        let rows = self.catalog_rows();
        let Some(panel) = &mut self.catalog else { return };
        let before = panel.row;
        match step {
            Some(Nav::Press(Direction::Up)) | Some(Nav::Repeat(Direction::Up, _)) => panel.row = panel.row.saturating_sub(1),
            Some(Nav::Press(Direction::Down)) | Some(Nav::Repeat(Direction::Down, _)) => panel.row = (panel.row + 1).min(rows.len().saturating_sub(1)),
            _ => {}
        }
        if panel.row != before {
            panel.armed = None;
        }
        if cancel {
            // A running download goes on behind the shelf; its answer finds no panel.
            self.catalog = None;
            return;
        }
        if panel.loading || panel.installing.is_some() {
            return;
        }
        if refresh {
            panel.armed = None;
            if host.catalog_fetch() {
                panel.loading = true;
                panel.note = t.catalog_loading.to_string();
            } else {
                panel.note = fill(t.failed, &[("reason", "unavailable")]);
            }
            return;
        }
        if !confirm {
            return;
        }
        let Some((app, _)) = rows.get(panel.row) else { return };
        if app.data {
            panel.note = fill(t.catalog_data, &[("name", &app.name)]);
            return;
        }
        if panel.armed.as_deref() != Some(app.title_id.as_str()) {
            panel.armed = Some(app.title_id.clone());
            panel.note = fill(t.catalog_confirm, &[("name", &app.name), ("version", &app.version), ("size", &app.size)]);
            return;
        }
        panel.armed = None;
        if host.catalog_install(&app.id, &app.title_id) {
            panel.installing = Some(app.title_id.clone());
            panel.note = t.working.to_string();
        } else {
            panel.note = fill(t.failed, &[("reason", "unavailable")]);
        }
    }

    pub(super) fn draw_catalog(&mut self, gfx: &mut Gfx) {
        let Some(panel) = &self.catalog else { return };
        let t = self.t();
        let theme = self.theme;
        let slide = self.drawer.at(self.now);
        let rows = self.catalog_rows();
        let mut drawer = Drawer::open(gfx, &theme, &upper(t.catalog, self.settings.language), panel::Size::Wide, slide);
        let first = panel.row.saturating_sub(ROWS / 2).min(rows.len().saturating_sub(ROWS));
        drawer.index = first;
        drawer.selectable = true;
        for (i, (app, kind)) in rows.iter().enumerate().skip(first).take(ROWS) {
            let label = gfx.fit(Role::BodyBold, &app.name, 300.0);
            let value = match kind {
                Kind::Update => fill(t.catalog_tag_update, &[("installed", app.installed.as_deref().unwrap_or("")), ("version", &app.version)]),
                Kind::Missing => fill(t.catalog_tag_missing, &[("version", &app.version)]),
            };
            drawer.row(gfx, &label, Some(&value), None, i == panel.row, Step::None);
        }
        let note = if !panel.note.is_empty() {
            panel.note.clone()
        } else if rows.is_empty() && self.catalog_list.is_some() {
            t.catalog_none.to_string()
        } else {
            t.catalog_help.to_string()
        };
        drawer.note(gfx, &note, 2);
        drawer.hints(
            gfx,
            &[
                (Button::DpadVertical, t.navigate),
                (Button::confirm(self.settings.confirm), t.choose),
                (Button::Face(Symbol::Square), t.catalog_refresh),
                (Button::cancel(self.settings.confirm), t.back),
            ],
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_compare_as_dotted_numbers() {
        assert!(newer("1.3", "1.20") == false);
        assert!(newer("1.21", "01.20"));
        assert!(newer("2.0", "1.99"));
        assert!(!newer("1.0", "1.00"), "trailing zeros are the same version");
        assert!(newer("v2.1", "2.0.9"));
        assert!(!newer("r123", "100"), "a version that is not a number is not compared");
        assert!(!newer("1.2", "beta"));
        assert!(!newer("", "1.0"));
    }
}
