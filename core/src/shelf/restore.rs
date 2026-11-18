//! SELECT ▸ Tools ▸ To reinstall: the games and apps this card has had that
//! are not installed now.
//!
//! Each scan adds what it finds to `known.tsv` in the data folder (`id`,
//! TAB, name; the file only grows, except for what the user forgets or
//! deletes in the app). The panel lists the entries whose id is not among the
//! installed titles, so a title that is installed again leaves the list on
//! its own. The data folder is not the card's `ux0:app`, so a card that was
//! formatted or lost files keeps the record of what it held.

use std::collections::HashSet;

use super::chrome::Button;
use super::navigation::{Direction, Step as Nav};
use super::panel::{self, Drawer, Step};
use super::Shelf;
use crate::gfx::Gfx;
use crate::host::Host;
use crate::i18n::{fill, upper};
use crate::library::categories::Platform;
use crate::shapes::Symbol;
use crate::text::Role;

pub const LEDGER_FILE: &str = "known.tsv";

/// Rows shown at once.
const ROWS: usize = 7;

/// A title the card has had.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Seen {
    pub id: String,
    pub name: String,
}

pub struct Restore {
    pub row: usize,
}

pub fn parse(text: &str) -> Vec<Seen> {
    text.lines()
        .filter_map(|line| {
            let (id, name) = line.split_once('\t')?;
            (!id.is_empty()).then(|| Seen { id: id.to_string(), name: name.to_string() })
        })
        .collect()
}

pub fn render(list: &[Seen]) -> String {
    list.iter().map(|seen| format!("{}\t{}\n", seen.id, seen.name.replace(['\t', '\n'], " "))).collect()
}

/// Add the installed titles to the record, updating a name that changed.
/// True when the record changed.
pub fn merge<'a>(ledger: &mut Vec<Seen>, installed: impl Iterator<Item = (&'a str, &'a str)>) -> bool {
    let mut changed = false;
    for (id, name) in installed {
        match ledger.iter_mut().find(|seen| seen.id == id) {
            Some(seen) if seen.name == name => {}
            Some(seen) => {
                seen.name = name.to_string();
                changed = true;
            }
            None => {
                ledger.push(Seen { id: id.to_string(), name: name.to_string() });
                changed = true;
            }
        }
    }
    changed
}

/// The record's entries that are not installed, by name.
pub fn missing<'a>(ledger: &'a [Seen], installed: &HashSet<&str>) -> Vec<&'a Seen> {
    let mut list: Vec<&Seen> = ledger.iter().filter(|seen| !installed.contains(seen.id.as_str())).collect();
    list.sort_by_key(|seen| (seen.name.to_lowercase(), seen.id.clone()));
    list
}

impl Shelf {
    /// Add what the last scan found to the record. Called after each title list.
    pub fn update_ledger(&mut self, host: &mut dyn Host) {
        let ledger = self.ledger.get_or_insert_with(|| host.read(LEDGER_FILE).map(|bytes| parse(&String::from_utf8_lossy(&bytes))).unwrap_or_default());
        let installed = self.library.games().iter().filter(|game| game.platform == Platform::Vita && !game.adrenaline).map(|game| (&*game.id, &*game.title));
        if merge(ledger, installed) {
            host.write(LEDGER_FILE, render(ledger).as_bytes());
        }
        self.recount_missing();
    }

    /// Take a title out of the record (forgotten, or deleted here on purpose).
    pub(super) fn ledger_forget(&mut self, host: &mut dyn Host, id: &str) {
        let ledger = self.ledger.get_or_insert_with(|| host.read(LEDGER_FILE).map(|bytes| parse(&String::from_utf8_lossy(&bytes))).unwrap_or_default());
        let before = ledger.len();
        ledger.retain(|seen| seen.id != id);
        if ledger.len() != before {
            host.write(LEDGER_FILE, render(ledger).as_bytes());
        }
        self.recount_missing();
    }

    /// The status bar's text: `12.3 GB free`, then `2 missing` when titles are to reinstall.
    pub(super) fn summary_text(&self) -> String {
        let t = self.t();
        let Some(free) = self.free else { return String::new() };
        let mut text = fill(t.summary_free, &[("free", &super::editor::format_size(free))]);
        if self.missing > 0 {
            text.push_str(" \u{b7} ");
            text.push_str(&fill(t.summary_missing, &[("count", &self.missing.to_string())]));
        }
        text
    }

    /// Count the titles to reinstall again; called when the list or the record changed.
    fn recount_missing(&mut self) {
        self.missing = self.to_reinstall_count();
    }

    /// The titles to reinstall: id and name.
    fn to_reinstall(&self) -> Vec<(String, String)> {
        let installed: HashSet<&str> = self.library.games().iter().map(|game| &*game.id).collect();
        let empty = Vec::new();
        missing(self.ledger.as_ref().unwrap_or(&empty), &installed).into_iter().map(|seen| (seen.id.clone(), seen.name.clone())).collect()
    }

    /// How many titles the panel lists.
    pub(super) fn to_reinstall_count(&self) -> usize {
        self.to_reinstall().len()
    }

    pub(super) fn open_restore(&mut self, host: &mut dyn Host) {
        self.update_ledger(host);
        self.restore = Some(Restore { row: 0 });
        self.drawer = panel::slide();
        self.drawer.set(1.0, self.now);
    }

    pub(super) fn restore_input(&mut self, host: &mut dyn Host, step: Option<Nav>, forget: bool, cancel: bool) {
        let list = self.to_reinstall();
        let Some(panel) = &mut self.restore else { return };
        match step {
            Some(Nav::Press(Direction::Up)) | Some(Nav::Repeat(Direction::Up, _)) => panel.row = panel.row.saturating_sub(1),
            Some(Nav::Press(Direction::Down)) | Some(Nav::Repeat(Direction::Down, _)) => panel.row = (panel.row + 1).min(list.len().saturating_sub(1)),
            _ => {}
        }
        if cancel {
            self.restore = None;
            return;
        }
        if forget {
            if let Some((id, _)) = list.get(panel.row) {
                let id = id.clone();
                panel.row = panel.row.min(list.len().saturating_sub(2));
                self.ledger_forget(host, &id);
            }
        }
    }

    pub(super) fn draw_restore(&mut self, gfx: &mut Gfx) {
        let Some(panel) = &self.restore else { return };
        let t = self.t();
        let theme = self.theme;
        let slide = self.drawer.at(self.now);
        let list = self.to_reinstall();
        let mut drawer = Drawer::open(gfx, &theme, &upper(t.restore, self.settings.language), panel::Size::Wide, slide);
        let first = panel.row.saturating_sub(ROWS / 2).min(list.len().saturating_sub(ROWS));
        drawer.index = first;
        drawer.selectable = true;
        for (i, (id, name)) in list.iter().enumerate().skip(first).take(ROWS) {
            let label = gfx.fit(Role::BodyBold, if name.is_empty() { id } else { name }, 330.0);
            drawer.row(gfx, &label, Some(id), None, i == panel.row, Step::None);
        }
        let note = if list.is_empty() { t.restore_none.to_string() } else { format!("{}\n{}", fill(t.restore_count, &[("count", &list.len().to_string())]), t.restore_help) };
        drawer.note(gfx, &note, 1);
        drawer.hints(gfx, &[(Button::DpadVertical, t.navigate), (Button::Face(Symbol::Triangle), t.restore_forget), (Button::cancel(self.settings.confirm), t.back)]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seen(id: &str, name: &str) -> Seen {
        Seen { id: id.into(), name: name.into() }
    }

    #[test]
    fn the_record_round_trips_and_ignores_bad_lines() {
        let list = vec![seen("PCSB00581", "Crimsonland"), seen("VITASHELL", "VitaShell")];
        assert_eq!(parse(&render(&list)), list);
        assert_eq!(parse("no tab here\n\tno id\nA\tB\n"), vec![seen("A", "B")]);
        assert_eq!(render(&[seen("X", "a\tb\nc")]), "X\ta b c\n");
    }

    #[test]
    fn a_scan_adds_and_renames_but_never_removes() {
        let mut ledger = vec![seen("OLD000001", "Old game")];
        assert!(merge(&mut ledger, [("NEW000001", "New game"), ("OLD000001", "Old game")].into_iter()));
        assert_eq!(ledger.len(), 2);
        assert!(!merge(&mut ledger, [("NEW000001", "New game")].into_iter()));
        assert!(merge(&mut ledger, [("NEW000001", "New game 2")].into_iter()));
        assert_eq!(ledger[1].name, "New game 2");
        // An empty scan changes nothing.
        assert!(!merge(&mut ledger, std::iter::empty()));
        assert_eq!(ledger.len(), 2);
    }

    #[test]
    fn what_is_installed_again_leaves_the_list() {
        let ledger = vec![seen("B", "beta"), seen("A", "Alpha"), seen("C", "Gamma")];
        let installed: HashSet<&str> = ["B"].into_iter().collect();
        let list = missing(&ledger, &installed);
        assert_eq!(list.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(), ["A", "C"]);
    }
}
