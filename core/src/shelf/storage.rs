//! SELECT ▸ Tools ▸ Storage: the memory card's space, a check for folders
//! no installed game owns, the largest games, and Shrink for every game.
//! Each job runs on a host thread and answers with an `Event`; the panel
//! shows what is known and starts one job at a time.

use super::chrome::Button;
use super::editor::format_size;
use super::navigation::{Direction, Step as Nav};
use super::panel::{self, Drawer, Step};
use super::Shelf;
use crate::gfx::Gfx;
use crate::host::{Host, Leftover};
use crate::i18n::{fill, upper};
use crate::library::categories::Platform;

/// Rows shown at once; the note under them takes the rest of the card.
const ROWS: usize = 6;
/// The largest games listed.
const TOP: usize = 20;

/// A row that starts a job.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Action {
    Check,
    Clean,
    Measure,
    ShrinkAll,
}

/// The panel's state.
pub struct Storage {
    pub row: usize,
    /// The first row shown, kept from the last draw for taps.
    pub first: usize,
    /// The job a first press asked to confirm.
    pub armed: Option<Action>,
    /// The job that runs.
    pub busy: Option<Action>,
    pub note: String,
    /// Free and total bytes of the card.
    pub space: Option<(u64, u64)>,
    /// The last check's findings; None before a check, and after a clean.
    pub found: Option<Vec<Leftover>>,
    /// Every game's bytes, largest first.
    pub sizes: Option<Vec<(String, u64)>>,
}

/// Rows that are not the lists: the card, then the four jobs.
const FIXED: usize = 5;

impl Storage {
    fn listed(&self) -> usize {
        self.found.as_ref().map_or(0, Vec::len) + self.sizes.as_ref().map_or(0, |s| s.len().min(TOP))
    }

    pub fn count(&self) -> usize {
        FIXED + self.listed()
    }

    /// The job a row starts, for the fixed rows after the card.
    fn action(row: usize) -> Option<Action> {
        match row {
            1 => Some(Action::Check),
            2 => Some(Action::Clean),
            3 => Some(Action::Measure),
            4 => Some(Action::ShrinkAll),
            _ => None,
        }
    }

    /// What a press of Clean removes.
    fn removable(&self) -> Vec<(String, crate::host::LeftoverKind)> {
        self.found
            .iter()
            .flatten()
            .filter(|item| item.kind.removable())
            .map(|item| (item.id.clone(), item.kind))
            .collect()
    }
}

impl Shelf {
    pub(super) fn open_storage(&mut self, host: &mut dyn Host) {
        self.storage = Some(Storage {
            row: 0,
            first: 0,
            armed: None,
            busy: None,
            note: String::new(),
            space: host.storage_space(),
            found: None,
            sizes: None,
        });
        self.drawer = panel::slide();
        self.drawer.set(1.0, self.now);
    }

    /// A game's name for a list row; its id when the shelf does not list it.
    fn storage_name(&self, id: &str) -> String {
        self.library.games().iter().find(|game| &*game.id == id).map_or_else(|| id.to_string(), |game| format!("{} ({id})", game.title))
    }

    /// The Vita games Shrink all goes through.
    fn storage_shrinkable(&self) -> Vec<String> {
        self.library
            .games()
            .iter()
            .filter(|game| game.platform == Platform::Vita && !game.adrenaline && *game.id != *crate::autostart::SELF_ID)
            .map(|game| game.id.to_string())
            .collect()
    }

    pub(super) fn storage_input(&mut self, host: &mut dyn Host, step: Option<Nav>, confirm: bool, cancel: bool) {
        let t = self.t();
        let ids = self.storage_shrinkable();
        let Some(panel) = &mut self.storage else { return };
        match step {
            Some(Nav::Press(Direction::Up)) | Some(Nav::Repeat(Direction::Up, _)) => {
                panel.row = panel.row.saturating_sub(1);
                panel.armed = None;
            }
            Some(Nav::Press(Direction::Down)) | Some(Nav::Repeat(Direction::Down, _)) => {
                panel.row = (panel.row + 1).min(panel.count() - 1);
                panel.armed = None;
            }
            _ => {}
        }
        if cancel {
            // A running job goes on behind the shelf; its answer finds no panel.
            self.storage = None;
            return;
        }
        if !confirm || panel.busy.is_some() {
            return;
        }
        let Some(action) = Storage::action(panel.row) else { return };
        panel.space = host.storage_space();
        panel.note.clear();
        let started = match action {
            Action::Check => host.storage_check(),
            Action::Measure => host.storage_sizes(),
            Action::Clean | Action::ShrinkAll => {
                let count = if action == Action::Clean { panel.removable().len() } else { ids.len() };
                if count == 0 {
                    panel.note = t.storage_none.to_string();
                    return;
                }
                if panel.armed != Some(action) {
                    panel.armed = Some(action);
                    let text = if action == Action::Clean { t.storage_confirm_clean } else { t.storage_confirm_shrink };
                    panel.note = fill(text, &[("count", &count.to_string())]);
                    return;
                }
                panel.armed = None;
                if action == Action::Clean {
                    host.storage_clean(panel.removable())
                } else {
                    host.storage_shrink_all(ids)
                }
            }
        };
        if started {
            panel.busy = Some(action);
        } else {
            panel.note = t.storage_failed.to_string();
        }
    }

    /// The check finished.
    pub fn storage_checked(&mut self, found: Vec<Leftover>) {
        let t = self.t();
        let Some(panel) = &mut self.storage else { return };
        panel.busy = None;
        panel.note = if found.is_empty() { t.storage_none.to_string() } else { fill(t.storage_found, &[("count", &found.len().to_string())]) };
        panel.found = Some(found);
    }

    /// Clean finished: what it removed, and what stayed.
    pub fn storage_cleaned(&mut self, removed: u32, failed: u32) {
        let t = self.t();
        let Some(panel) = &mut self.storage else { return };
        panel.busy = None;
        // The findings are out of date; check again to list what is left.
        panel.found = None;
        panel.row = panel.row.min(panel.count() - 1);
        panel.note = fill(t.storage_cleaned, &[("removed", &removed.to_string()), ("failed", &failed.to_string())]);
    }

    /// The games' sizes arrived.
    pub fn storage_measured(&mut self, sizes: Vec<(String, u64)>) {
        // The size orders use the numbers whether or not the panel is open.
        self.library.set_sizes(&sizes);
        if matches!(self.library.order_of(&self.tab), crate::library::order::SortOrder::LargestSize | crate::library::order::SortOrder::SmallestSize) {
            let keep = self.selected_game().map(|g| g.id.to_string());
            self.relist(keep);
        }
        let Some(panel) = &mut self.storage else { return };
        panel.busy = None;
        panel.note.clear();
        panel.sizes = Some(sizes);
    }

    /// Shrink all finished.
    pub fn storage_shrunk(&mut self, games: u32, freed: u64) {
        let t = self.t();
        let Some(panel) = &mut self.storage else { return };
        panel.busy = None;
        // Sizes before the shrink are out of date.
        panel.sizes = None;
        panel.row = panel.row.min(panel.count() - 1);
        panel.note = fill(t.storage_shrunk, &[("count", &games.to_string()), ("size", &format_size(freed))]);
    }

    pub(super) fn draw_storage(&mut self, gfx: &mut Gfx) {
        let Some(panel) = &self.storage else { return };
        let t = self.t();
        let theme = self.theme;
        let slide = self.drawer.at(self.now);
        let (row, busy, note) = (panel.row, panel.busy, panel.note.clone());
        let card = panel.space.map(|(free, total)| fill(t.storage_card_value, &[("free", &format_size(free)), ("total", &format_size(total))]));
        let jobs = [
            (t.storage_check, match (busy, &panel.found) {
                (Some(Action::Check), _) => t.storage_checking.to_string(),
                (_, Some(found)) if found.is_empty() => t.storage_none.to_string(),
                (_, Some(found)) => fill(t.storage_found, &[("count", &found.len().to_string())]),
                _ => String::new(),
            }),
            (t.storage_clean, match &panel.found {
                Some(_) => panel.removable().len().to_string(),
                None => String::new(),
            }),
            (t.storage_measure, if busy == Some(Action::Measure) { t.storage_measuring.to_string() } else { String::new() }),
            (t.storage_shrink_all, if busy == Some(Action::ShrinkAll) { t.working.to_string() } else { String::new() }),
        ];
        let mut lines: Vec<(String, String)> = vec![(t.storage_card.to_string(), card.unwrap_or_default())];
        lines.extend(jobs.iter().map(|(label, value)| (label.to_string(), value.clone())));
        for item in panel.found.iter().flatten() {
            let kind = match item.kind {
                crate::host::LeftoverKind::EmptyApp => t.kind_empty,
                crate::host::LeftoverKind::Patch => t.kind_patch,
                crate::host::LeftoverKind::Data => t.kind_data,
                crate::host::LeftoverKind::Damaged => t.kind_damaged,
            };
            lines.push((item.id.clone(), format!("{kind} {}", format_size(item.bytes))));
        }
        for (id, bytes) in panel.sizes.iter().flatten().take(TOP) {
            lines.push((self.storage_name(id), format_size(*bytes)));
        }
        let first = row.saturating_sub(ROWS / 2).min(lines.len().saturating_sub(ROWS));
        let mut drawer = Drawer::open(gfx, &theme, &upper(t.storage, self.settings.language), panel::Size::Wide, slide);
        for (i, (label, value)) in lines.iter().enumerate().skip(first).take(ROWS) {
            let value = (!value.is_empty()).then_some(value.as_str());
            drawer.row(gfx, label, value, None, i == row, Step::None);
        }
        let text = if note.is_empty() { t.storage_help.to_string() } else { note };
        drawer.note(gfx, &text, 1);
        drawer.hints(gfx, &[(Button::DpadVertical, t.navigate), (Button::confirm(self.settings.confirm), t.choose), (Button::cancel(self.settings.confirm), t.back)]);
        if let Some(panel) = &mut self.storage {
            panel.first = first;
        }
    }
}
