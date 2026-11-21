//! The editor's Data folder row: which folder of `ux0:data` holds the files of
//! an installed app. Most homebrew keeps them under its own name (`VitaShell`,
//! `RetroArch`), not under its title id, so the size and Delete with data find
//! nothing until the user picks the folder.
//!
//! The choice is saved in `data-folders.tsv` in the data folder (`id`, TAB,
//! folder name) and passed to the host with each call that needs it: the host
//! does not read the file back. The list of folders is read on a host thread;
//! the names that contain the title id or the title come first.

use std::collections::BTreeMap;

use super::chrome::Button;
use super::navigation::{Direction, Step as Nav};
use super::panel::{self, Drawer, Step};
use super::Shelf;
use crate::gfx::Gfx;
use crate::host::Host;
use crate::i18n::{fill, upper};
use crate::text::Role;

pub const DATA_FOLDERS_FILE: &str = "data-folders.tsv";

/// Rows shown at once.
const ROWS: usize = 7;

/// The open list of folders.
pub struct DataPick {
    pub id: String,
    /// The folder names of `ux0:data`; `None` until the host answers.
    pub list: Option<Vec<String>>,
    pub row: usize,
    pub note: String,
}

pub fn parse(text: &str) -> BTreeMap<String, String> {
    text.lines()
        .filter_map(|line| line.split_once('\t'))
        .filter(|(id, folder)| !id.trim().is_empty() && valid_folder(folder.trim()))
        .map(|(id, folder)| (id.trim().to_string(), folder.trim().to_string()))
        .collect()
}

pub fn render(map: &BTreeMap<String, String>) -> String {
    map.iter().map(|(id, folder)| format!("{id}\t{folder}\n")).collect()
}

/// A folder name that is one path component.
pub fn valid_folder(name: &str) -> bool {
    !name.is_empty() && name != "." && name != ".." && !name.contains(['/', '\\', ':'])
}

fn slug(text: &str) -> String {
    text.chars().filter(|c| c.is_alphanumeric()).flat_map(|c| c.to_lowercase()).collect()
}

/// A folder name that looks like the app's: it holds the title id or the
/// title's letters, or the other way round.
pub fn suggested(folder: &str, id: &str, title: &str) -> bool {
    let (folder, id, title) = (slug(folder), slug(id), slug(title));
    folder.len() >= 3 && (folder.contains(&id) || (title.len() >= 3 && (folder.contains(&title) || title.contains(&folder))))
}

impl Shelf {
    /// The saved choices, read once.
    pub(super) fn data_map(&mut self, host: &mut dyn Host) -> &mut BTreeMap<String, String> {
        self.data_map.get_or_insert_with(|| host.read(DATA_FOLDERS_FILE).map(|bytes| parse(&String::from_utf8_lossy(&bytes))).unwrap_or_default())
    }

    /// The folder of `ux0:data` chosen for a title; empty for the title id.
    pub(super) fn data_folder_for(&mut self, host: &mut dyn Host, id: &str) -> String {
        self.data_map(host).get(id).cloned().unwrap_or_default()
    }

    /// The Data folder row was pressed.
    pub(super) fn open_data_folder(&mut self, host: &mut dyn Host) {
        let Some(id) = self.editor.as_ref().map(|e| e.id.clone()) else { return };
        let t = self.t();
        let mut pick = DataPick { id, list: None, row: 0, note: t.working.to_string() };
        if !host.data_folders() {
            pick.note = fill(t.failed, &[("reason", "unavailable")]);
        }
        self.datadir = Some(pick);
    }

    /// The host listed `ux0:data`.
    pub fn data_folders_listed(&mut self, list: Vec<String>) {
        if let Some(pick) = &mut self.datadir {
            pick.list = Some(list);
            pick.note.clear();
        }
    }

    /// The rows: `None` is the title id's own folder, then the folders, the
    /// suggested first.
    fn datadir_rows(&self) -> Vec<(Option<String>, bool)> {
        let Some(pick) = &self.datadir else { return Vec::new() };
        let title = self.library.game(&pick.id).map(|g| g.title.to_string()).unwrap_or_default();
        let mut folders: Vec<(String, bool)> = pick.list.iter().flatten().map(|name| (name.clone(), suggested(name, &pick.id, &title))).collect();
        folders.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.to_lowercase().cmp(&b.0.to_lowercase())));
        std::iter::once((None, false)).chain(folders.into_iter().map(|(name, hint)| (Some(name), hint))).collect()
    }

    pub(super) fn datadir_row_count(&self) -> usize {
        self.datadir_rows().len()
    }

    pub(super) fn datadir_input(&mut self, host: &mut dyn Host, step: Option<Nav>, confirm: bool, cancel: bool) {
        let rows = self.datadir_rows();
        let Some(pick) = &mut self.datadir else { return };
        match step {
            Some(Nav::Press(Direction::Up)) | Some(Nav::Repeat(Direction::Up, _)) => pick.row = pick.row.saturating_sub(1),
            Some(Nav::Press(Direction::Down)) | Some(Nav::Repeat(Direction::Down, _)) => pick.row = (pick.row + 1).min(rows.len().saturating_sub(1)),
            _ => {}
        }
        if cancel {
            self.datadir = None;
            return;
        }
        if !confirm || pick.list.is_none() {
            return;
        }
        let Some((choice, _)) = rows.get(pick.row).cloned() else { return };
        let id = pick.id.clone();
        self.datadir = None;
        let map = self.data_map(host);
        match &choice {
            Some(folder) => {
                map.insert(id.clone(), folder.clone());
            }
            None => {
                map.remove(&id);
            }
        }
        let text = render(map);
        host.write(DATA_FOLDERS_FILE, text.as_bytes());
        // The size is walked again for the folder now chosen.
        host.folder_sizes(&id, choice.as_deref().unwrap_or(""));
        if let Some(editor) = &mut self.editor {
            editor.folders = None;
        }
    }

    pub(super) fn draw_datadir(&mut self, gfx: &mut Gfx) {
        let Some(pick) = &self.datadir else { return };
        let t = self.t();
        let theme = self.theme;
        let slide = self.drawer.at(self.now);
        let rows = self.datadir_rows();
        let mut drawer = Drawer::open(gfx, &theme, &upper(t.data_folder, self.settings.language), panel::Size::Wide, slide);
        let first = pick.row.saturating_sub(ROWS / 2).min(rows.len().saturating_sub(ROWS));
        drawer.index = first;
        drawer.selectable = true;
        for (i, (choice, hint)) in rows.iter().enumerate().skip(first).take(ROWS) {
            let label = match choice {
                Some(name) => gfx.fit(Role::BodyBold, name, 300.0),
                None => fill(t.data_folder_default, &[("id", &pick.id)]),
            };
            let value = hint.then_some(t.data_folder_suggested);
            drawer.row(gfx, &label, value, None, i == pick.row, Step::None);
        }
        let note = if !pick.note.is_empty() { pick.note.as_str() } else { t.data_folder_help };
        drawer.note(gfx, note, 2);
        drawer.hints(gfx, &[(Button::DpadVertical, t.navigate), (Button::confirm(self.settings.confirm), t.choose), (Button::cancel(self.settings.confirm), t.back)]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_choices_round_trip_and_unsafe_names_are_dropped() {
        let map = parse("VITASHELL\tVitaShell\nBAD000001\t../app\nRETROARCH\tRetroArch\n\tx\n");
        assert_eq!(map.len(), 2);
        assert_eq!(parse(&render(&map)), map);
        assert!(valid_folder("VitaShell") && !valid_folder("a/b") && !valid_folder("..") && !valid_folder(""));
    }

    #[test]
    fn folders_that_look_like_the_app_are_suggested() {
        assert!(suggested("VitaShell", "VITASHELL", "VitaShell"));
        assert!(suggested("RetroArch-data", "RARCH0001", "RetroArch"));
        assert!(!suggested("PocketShelf", "VITASHELL", "VitaShell"));
        assert!(!suggested("ab", "VITASHELL", "VitaShell"));
    }
}
