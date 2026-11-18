//! SELECT ▸ System ▸ Install VPK: the packages the host found in
//! `ux0:install` and its folders, each with the name and icon of the game or
//! homebrew inside, and the install of the highlighted one (two presses).
//! The host reads and installs on worker threads; the panel lists, sorts
//! and shows how far an install is.

use super::chrome::Button;
use super::editor::format_size;
use super::navigation::{Direction, Step as Nav};
use super::panel::{self, Drawer, Step};
use super::Shelf;
use crate::gfx::{Format, Gfx, Rect, TexId};
use crate::host::{DecodeAs, Host, Picture, PictureKey, ScanPhase, VpkInfo, VpkProgress};
use crate::i18n::{fill, upper};
use crate::shapes::Symbol;
use crate::text::Role;

/// Rows shown at once, beside the icon.
const ROWS: usize = 6;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum VpkSort {
    Name,
    Date,
    Size,
}

impl VpkSort {
    fn next(self) -> VpkSort {
        match self {
            VpkSort::Name => VpkSort::Date,
            VpkSort::Date => VpkSort::Size,
            VpkSort::Size => VpkSort::Name,
        }
    }
}

pub struct Vpks {
    /// The packages in the order shown.
    pub list: Vec<VpkInfo>,
    pub sort: VpkSort,
    pub row: usize,
    /// The host is still reading the folders.
    pub scanning: bool,
    /// The path of the package being installed.
    pub installing: Option<String>,
    /// The package a first press asked to confirm.
    pub armed: Option<String>,
    pub note: String,
    /// The highlighted package's icon, and its texture once decoded.
    pub preview: Option<(PictureKey, Option<(TexId, Option<u32>, [f32; 4])>)>,
}

/// A package's name for sorting and for the row.
fn shown_name(info: &VpkInfo) -> &str {
    if info.title.is_empty() {
        &info.file
    } else {
        &info.title
    }
}

fn sort_list(list: &mut [VpkInfo], by: VpkSort) {
    let name = |info: &VpkInfo| (shown_name(info).to_lowercase(), info.path.clone());
    match by {
        VpkSort::Name => list.sort_by_key(name),
        VpkSort::Date => list.sort_by(|a, b| b.modified.cmp(&a.modified).then_with(|| name(a).cmp(&name(b)))),
        VpkSort::Size => list.sort_by(|a, b| b.size.cmp(&a.size).then_with(|| name(a).cmp(&name(b)))),
    }
}

/// `2026-11-18` for seconds since 1970 (UTC); empty for 0.
fn date(secs: u64) -> String {
    if secs == 0 {
        return String::new();
    }
    let z = (secs / 86_400) as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}")
}

impl Shelf {
    pub(super) fn open_vpks(&mut self, host: &mut dyn Host) {
        let scanning = host.vpk_scan();
        let note = if scanning { self.t().install_looking.to_string() } else { self.t().storage_failed.to_string() };
        self.vpks = Some(Vpks { list: Vec::new(), sort: VpkSort::Name, row: 0, scanning, installing: None, armed: None, note, preview: None });
        self.drawer = panel::slide();
        self.drawer.set(1.0, self.now);
    }

    /// The preview's texture goes (freed at the next draw).
    fn drop_vpk_preview(&mut self) {
        let Some(panel) = &mut self.vpks else { return };
        if let Some((_, Some((tex, ..)))) = panel.preview.take() {
            self.pending_free.push(tex);
        }
    }

    pub(super) fn close_vpks(&mut self) {
        self.drop_vpk_preview();
        self.vpks = None;
    }

    /// Ask for the highlighted package's icon.
    fn ask_vpk_icon(&mut self, host: &mut dyn Host) {
        self.drop_vpk_preview();
        let Some(panel) = &mut self.vpks else { return };
        let Some(path) = panel.list.get(panel.row).and_then(|info| info.icon.clone()) else { return };
        let key = PictureKey::File { path, as_: DecodeAs::FittedArt };
        panel.preview = Some((key.clone(), None));
        host.request(key);
    }

    /// The host's list.
    pub fn vpks_listed(&mut self, host: &mut dyn Host, mut list: Vec<VpkInfo>) {
        let t = self.t();
        let Some(panel) = &mut self.vpks else { return };
        sort_list(&mut list, panel.sort);
        panel.scanning = false;
        panel.note = if list.is_empty() { t.install_none.to_string() } else { String::new() };
        panel.list = list;
        panel.row = 0;
        self.ask_vpk_icon(host);
    }

    /// A picture answered for the preview; true when it was.
    pub(super) fn vpks_picture(&mut self, gfx: &mut Gfx, key: &PictureKey, picture: &Option<Picture>) -> bool {
        let Some(panel) = &mut self.vpks else { return false };
        if panel.preview.as_ref().map(|p| &p.0) != Some(key) {
            return false;
        }
        if let Some(p) = picture {
            let tex = gfx.add_texture(p.w, p.h, Format::Rgba8, p.rgba.clone(), true);
            panel.preview = Some((key.clone(), Some((tex, p.accent, super::art::shown_part(p.content, p.w, p.h)))));
        }
        true
    }

    /// While an install runs: how far it is.
    pub(super) fn vpks_frame(&mut self, host: &mut dyn Host) {
        let t = self.t();
        let Some(panel) = &mut self.vpks else { return };
        if panel.installing.is_none() {
            return;
        }
        panel.note = match host.vpk_progress() {
            VpkProgress::Unpacking(done, total) if total > 0 => fill(t.install_unpacking, &[("percent", &(done * 100 / total).to_string())]),
            VpkProgress::Unpacking(..) => t.working.to_string(),
            VpkProgress::Installing => t.install_installing.to_string(),
            VpkProgress::Idle => panel.note.clone(),
        };
    }

    pub(super) fn vpks_input(&mut self, host: &mut dyn Host, step: Option<Nav>, confirm: bool, cancel: bool, sort: bool) {
        let t = self.t();
        let Some(panel) = &mut self.vpks else { return };
        let before = panel.row;
        match step {
            Some(Nav::Press(Direction::Up)) | Some(Nav::Repeat(Direction::Up, _)) => panel.row = panel.row.saturating_sub(1),
            Some(Nav::Press(Direction::Down)) | Some(Nav::Repeat(Direction::Down, _)) => {
                panel.row = (panel.row + 1).min(panel.list.len().saturating_sub(1));
            }
            _ => {}
        }
        if cancel {
            self.close_vpks();
            return;
        }
        if panel.row != before {
            panel.armed = None;
            if panel.installing.is_none() {
                panel.note.clear();
            }
            self.ask_vpk_icon(host);
            return;
        }
        if panel.installing.is_some() || panel.scanning {
            return;
        }
        if sort {
            let kept = panel.list.get(panel.row).map(|info| info.path.clone());
            panel.sort = panel.sort.next();
            sort_list(&mut panel.list, panel.sort);
            panel.row = kept.and_then(|path| panel.list.iter().position(|info| info.path == path)).unwrap_or(0);
            return;
        }
        if !confirm {
            return;
        }
        let Some(info) = panel.list.get(panel.row).cloned() else { return };
        if !info.problem.is_empty() {
            panel.note = fill(t.failed, &[("reason", &info.problem)]);
            return;
        }
        let name = shown_name(&info).to_string();
        if panel.armed.as_deref() != Some(info.path.as_str()) {
            panel.armed = Some(info.path);
            panel.note = fill(t.install_confirm, &[("name", &name)]);
            return;
        }
        panel.armed = None;
        if host.vpk_install(&info.path) {
            panel.installing = Some(info.path);
            panel.note = t.working.to_string();
        } else {
            panel.note = fill(t.failed, &[("reason", "unavailable")]);
        }
    }

    /// The install finished. The titles are read again either way: a package
    /// that failed half way may still have left an entry behind.
    pub fn vpk_installed(&mut self, host: &mut dyn Host, path: &str, result: Result<String, String>) {
        let t = self.t();
        if result.is_ok() {
            host.rescan_some(vec![ScanPhase::Apps]);
        }
        let Some(panel) = &mut self.vpks else { return };
        panel.installing = None;
        let name = panel.list.iter().find(|info| info.path == path).map(|info| shown_name(info).to_string()).unwrap_or_else(|| path.to_string());
        panel.note = match result {
            Ok(_) => fill(t.install_done, &[("name", &name)]),
            Err(reason) => fill(t.failed, &[("reason", &reason)]),
        };
    }

    pub(super) fn draw_vpks(&mut self, gfx: &mut Gfx) {
        let Some(panel) = &self.vpks else { return };
        let t = self.t();
        let theme = self.theme;
        let slide = self.drawer.at(self.now);
        let mut drawer = Drawer::open(gfx, &theme, &upper(t.install_vpk, self.settings.language), panel::Size::Wide, slide);
        if let Some((_, Some((tex, ..)))) = &panel.preview {
            if gfx.textures.ready(*tex) {
                gfx.fill(Rect::new(64.0, 140.0, 264.0, 264.0), theme.tile);
                gfx.image_full(*tex, Rect::new(68.0, 144.0, 256.0, 256.0), crate::gfx::Color::rgb(0xffffff));
            }
        }
        let first = panel.row.saturating_sub(ROWS / 2).min(panel.list.len().saturating_sub(ROWS));
        drawer.index = first;
        drawer.selectable = true;
        for i in first..(first + ROWS).min(panel.list.len()) {
            let info = &panel.list[i];
            let label = gfx.fit(Role::BodyBold, shown_name(info), 300.0);
            drawer.row(gfx, &label, Some(&format_size(info.size)), None, i == panel.row, Step::None);
        }
        // What the highlighted package is, then what there is to say.
        let mut lines = Vec::new();
        if let Some(info) = panel.list.get(panel.row) {
            let parts: Vec<String> = [info.title_id.clone(), info.version.clone(), date(info.modified)].into_iter().filter(|part| !part.is_empty()).collect();
            lines.push(gfx.fit(Role::Small, &parts.join(" \u{b7} "), 540.0));
            if !info.problem.is_empty() {
                lines.push(fill(t.failed, &[("reason", &info.problem)]));
            }
        }
        if !panel.note.is_empty() {
            lines.push(panel.note.clone());
        }
        drawer.note(gfx, &lines.join("\n"), 1);
        let sort = match panel.sort {
            VpkSort::Name => t.install_by_name,
            VpkSort::Date => t.install_by_date,
            VpkSort::Size => t.install_by_size,
        };
        drawer.hints(gfx, &[(Button::confirm(self.settings.confirm), t.choose), (Button::Face(Symbol::Square), sort), (Button::cancel(self.settings.confirm), t.back)]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates_are_written_as_year_month_day() {
        assert_eq!(date(0), "");
        assert_eq!(date(86_400 * 365), "1971-01-01");
        assert_eq!(date(1_700_000_000), "2023-11-14");
        assert_eq!(date(1_709_164_800), "2024-02-29");
    }

    #[test]
    fn packages_sort_by_name_date_and_size() {
        let pkg = |file: &str, title: &str, size: u64, modified: u64| VpkInfo { path: format!("ux0:install/{file}"), file: file.into(), title: title.into(), size, modified, ..Default::default() };
        let mut list = vec![pkg("b.vpk", "Beta", 5, 30), pkg("a.vpk", "", 9, 10), pkg("c.vpk", "alpha", 1, 20)];
        sort_list(&mut list, VpkSort::Name);
        assert_eq!(list.iter().map(|i| i.file.as_str()).collect::<Vec<_>>(), ["a.vpk", "c.vpk", "b.vpk"]);
        sort_list(&mut list, VpkSort::Date);
        assert_eq!(list.iter().map(|i| i.file.as_str()).collect::<Vec<_>>(), ["b.vpk", "c.vpk", "a.vpk"]);
        sort_list(&mut list, VpkSort::Size);
        assert_eq!(list.iter().map(|i| i.file.as_str()).collect::<Vec<_>>(), ["a.vpk", "b.vpk", "c.vpk"]);
    }

    #[test]
    fn the_sort_cycles_through_all_three() {
        assert_eq!(VpkSort::Name.next().next().next(), VpkSort::Name);
    }
}
