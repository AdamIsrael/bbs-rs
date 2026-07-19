//! The `bbscfg` state machine (#141 Slice B).
//!
//! Deliberately headless: this owns every decision — which screen, what's
//! selected, how a keystroke changes a value, when a save is refused — and
//! knows nothing about drawing. The binary renders it and feeds it keys.
//!
//! That split is what makes the editor testable. A TUI whose logic only exists
//! inside a terminal event loop can only be checked by driving a pty and
//! reading back characters, which is slow, flaky, and proves little. Here a
//! test can press keys and assert on the resulting document.

use crossterm::event::{KeyCode, KeyEvent};

use super::doc::{ConfigDoc, FieldValue, Issue};
use crate::app::ART_SCREEN_KEYS;

use super::schema::{
    self, DOOR_FIELDS, Field, FieldKind, SECTIONS, SEED_BOARD_FIELDS, Section, SectionKind,
};
use super::seed::{self, SeedStatus};

/// The one field an art row holds. Static so the shared `Edit` screen has a
/// `Field` to render like any other.
static ART_FILE_FIELD: Field = Field {
    key: "file",
    label: "Art file",
    kind: FieldKind::Str,
    help: "A file name under the art directory, e.g. boards.ans. Blank removes the art for this screen.",
};

/// What the editor is showing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    /// The list of `[sections]`.
    Sections,
    /// The settings within one section.
    Fields,
    /// Editing one setting.
    Edit,
    /// The list of configured door games (#145).
    Doors,
    /// Per-screen art: one row per screen that can carry it (#146).
    ArtScreens,
    /// Seeded boards: guest password plus a list of boards (#147).
    SeedBoards,
    /// The fields of one seeded board.
    SeedBoardFields,
    /// Confirming removal of a seeded board.
    ConfirmRemoveSeedBoard,
    /// The settings of one door.
    DoorFields,
    /// Confirming removal of a door.
    ConfirmRemoveDoor,
    /// Reviewing what's about to be written.
    Save,
    /// Confirming a quit that would discard changes.
    ConfirmQuit,
}

/// Which screen opened the shared text editor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EditFrom {
    Section,
    Door,
    ArtScreen,
    SeedBoard,
}

/// What the host loop should do after a keystroke.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    None,
    Quit,
}

pub struct Editor {
    pub doc: ConfigDoc,
    pub screen: Screen,
    pub section_sel: usize,
    pub field_sel: usize,
    /// The in-progress text while editing a `Str`/`Path`/`Int`/`StrList`.
    pub input: String,
    /// Message shown on the status line.
    pub status: String,
    /// Validation results, refreshed when the save screen opens.
    pub issues: Vec<Issue>,
    /// Which door is selected in the list, and which of its fields.
    pub door_sel: usize,
    pub door_field_sel: usize,
    /// Which screen's art is selected (#146).
    pub art_sel: usize,
    /// Which seeded board and field are selected (#147).
    pub seed_sel: usize,
    pub seed_field_sel: usize,
    /// Whether the guest_password field (rather than a board) is selected.
    pub seed_on_password: bool,
    /// Whether the configured database already has boards — so the screen can
    /// warn that edits here won't take effect. Computed when the screen opens.
    pub seed_status: SeedStatus,
    /// Where the shared text editor was opened from.
    ///
    /// `Edit` is reused by section fields, door fields and per-screen art, and
    /// the three write to different places. Committing to the wrong one would
    /// silently write nothing, so the origin is tracked rather than inferred.
    edit_from: EditFrom,
}

impl Editor {
    pub fn new(doc: ConfigDoc) -> Self {
        let status = if doc.is_new() {
            format!("New config — will be created at {}", doc.path().display())
        } else {
            format!("Editing {}", doc.path().display())
        };
        Self {
            doc,
            screen: Screen::Sections,
            section_sel: 0,
            field_sel: 0,
            input: String::new(),
            status,
            issues: Vec::new(),
            door_sel: 0,
            door_field_sel: 0,
            art_sel: 0,
            seed_sel: 0,
            seed_field_sel: 0,
            seed_on_password: true,
            seed_status: SeedStatus::WillSeed,
            edit_from: EditFrom::Section,
        }
    }

    pub fn section(&self) -> &'static Section {
        &SECTIONS[self.section_sel.min(SECTIONS.len() - 1)]
    }

    pub fn field(&self) -> Option<&'static schema::Field> {
        self.section().fields.get(self.field_sel)
    }

    /// The value shown for a setting, and whether it's explicitly set or is the
    /// built-in default showing through.
    pub fn shown_value(&self, key: &str) -> (String, bool) {
        let explicit = self.doc.get(self.section().name, key);
        match explicit {
            Some(v) => (v.display(), true),
            None => (
                self.doc
                    .effective(self.section().name, key)
                    .map(|v| v.display())
                    .unwrap_or_default(),
                false,
            ),
        }
    }

    pub fn on_key(&mut self, key: KeyEvent) -> Action {
        match self.screen {
            Screen::Sections => self.on_sections(key),
            Screen::Fields => self.on_fields(key),
            Screen::Edit => self.on_edit(key),
            Screen::Save => self.on_save(key),
            Screen::ConfirmQuit => self.on_confirm_quit(key),
            Screen::Doors => self.on_doors(key),
            Screen::ArtScreens => self.on_art_screens(key),
            Screen::SeedBoards => self.on_seed(key),
            Screen::SeedBoardFields => self.on_seed_board_fields(key),
            Screen::ConfirmRemoveSeedBoard => self.on_confirm_remove_seed_board(key),
            Screen::DoorFields => self.on_door_fields(key),
            Screen::ConfirmRemoveDoor => self.on_confirm_remove_door(key),
        }
    }

    fn on_sections(&mut self, key: KeyEvent) -> Action {
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                self.section_sel = self.section_sel.saturating_sub(1);
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if self.section_sel + 1 < SECTIONS.len() {
                    self.section_sel += 1;
                }
            }
            KeyCode::Enter | KeyCode::Right => match self.section().kind {
                SectionKind::Fields => {
                    self.field_sel = 0;
                    self.screen = Screen::Fields;
                }
                SectionKind::Doors => {
                    self.door_sel = 0;
                    self.screen = Screen::Doors;
                }
                SectionKind::ArtScreens => {
                    self.art_sel = 0;
                    self.screen = Screen::ArtScreens;
                }
                SectionKind::SeedBoards => self.open_seed(),
            },
            KeyCode::Char('s') => self.open_save(),
            KeyCode::Char('q') | KeyCode::Esc => return self.request_quit(),
            _ => {}
        }
        Action::None
    }

    fn on_fields(&mut self, key: KeyEvent) -> Action {
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                self.field_sel = self.field_sel.saturating_sub(1);
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if self.field_sel + 1 < self.section().fields.len() {
                    self.field_sel += 1;
                }
            }
            KeyCode::Enter => self.begin_edit(),
            // Removing a setting is how you go back to the built-in default,
            // which is different from setting it to an empty value.
            KeyCode::Char('u') => self.unset_current(),
            KeyCode::Char('s') => self.open_save(),
            KeyCode::Esc | KeyCode::Left | KeyCode::Char('q') => self.screen = Screen::Sections,
            _ => {}
        }
        Action::None
    }

    /// Booleans toggle in place — a whole edit screen to flip true/false would
    /// be ceremony. Everything else opens the editor primed with the current
    /// value.
    fn begin_edit(&mut self) {
        let Some(field) = self.field() else { return };
        let section = self.section().name;
        match field.kind {
            FieldKind::Bool => {
                let now = matches!(
                    self.doc.effective(section, field.key),
                    Some(FieldValue::Bool(true))
                );
                self.doc.set(section, field.key, FieldValue::Bool(!now));
                self.status = format!("{} = {}", field.key, !now);
            }
            FieldKind::Enum(options) => {
                let current = self
                    .doc
                    .effective(section, field.key)
                    .map(|v| v.display())
                    .unwrap_or_default();
                let idx = options.iter().position(|o| *o == current).unwrap_or(0);
                let next = options[(idx + 1) % options.len()];
                self.doc
                    .set(section, field.key, FieldValue::Str(next.to_string()));
                self.status = format!("{} = {next}", field.key);
            }
            _ => {
                self.input = self
                    .doc
                    .effective(section, field.key)
                    .map(|v| v.display())
                    .unwrap_or_default();
                self.edit_from = EditFrom::Section;
                self.screen = Screen::Edit;
                self.status = field.help.to_string();
            }
        }
    }

    fn unset_current(&mut self) {
        let Some(field) = self.field() else { return };
        let section = self.section().name;
        if self.doc.unset(section, field.key) {
            let now = self
                .doc
                .effective(section, field.key)
                .map(|v| v.display())
                .unwrap_or_default();
            self.status = format!("{} reset to the default ({now})", field.key);
        } else {
            self.status = format!("{} was already at its default", field.key);
        }
    }

    fn on_edit(&mut self, key: KeyEvent) -> Action {
        match key.code {
            KeyCode::Esc => {
                self.screen = self.edit_origin();
                self.status = "Cancelled.".into();
            }
            KeyCode::Enter => self.commit_edit(),
            KeyCode::Backspace => {
                self.input.pop();
            }
            KeyCode::Char(c) => self.input.push(c),
            _ => {}
        }
        Action::None
    }

    /// The field the shared `Edit` screen is currently editing, whichever kind
    /// it came from — what a renderer needs to label the input and show help.
    pub fn edit_field(&self) -> Option<&'static schema::Field> {
        match self.edit_from {
            EditFrom::Door => self.door_field(),
            EditFrom::ArtScreen => Some(&ART_FILE_FIELD),
            EditFrom::SeedBoard => self.seed_board_field(),
            EditFrom::Section => self.field(),
        }
    }

    /// Where Esc and a successful commit return to.
    fn edit_origin(&self) -> Screen {
        match self.edit_from {
            EditFrom::Door => Screen::DoorFields,
            EditFrom::ArtScreen => Screen::ArtScreens,
            EditFrom::SeedBoard => Screen::SeedBoardFields,
            EditFrom::Section => Screen::Fields,
        }
    }

    /// Apply the typed value, refusing anything the schema says is out of range
    /// rather than writing a config the server would reject at boot.
    fn commit_edit(&mut self) {
        let Some(field) = self.edit_field() else {
            return;
        };
        let section = self.section().name;
        let raw = self.input.trim().to_string();

        let value = match field.kind {
            FieldKind::Int { min, max } => match raw.parse::<i64>() {
                Ok(n) if (min..=max).contains(&n) => FieldValue::Int(n),
                Ok(n) => {
                    self.status = format!("{n} is outside {min}–{max} for {}", field.key);
                    return;
                }
                Err(_) => {
                    self.status = format!("{raw:?} is not a number");
                    return;
                }
            },
            FieldKind::StrList => FieldValue::List(
                raw.split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect(),
            ),
            _ => FieldValue::Str(raw),
        };

        match self.edit_from {
            EditFrom::Door => self.doc.door_set(self.door_sel, field.key, value),
            EditFrom::ArtScreen => {
                let (key, _) = ART_SCREEN_KEYS[self.art_sel];
                self.doc.art_screen_set(key, &value.display());
            }
            EditFrom::SeedBoard => {
                // seed_sel is the row (row 0 is the password); the board array
                // is one behind it.
                let idx = self.seed_board_index();
                self.doc.seed_board_set(idx, field.key, &value.display());
            }
            EditFrom::Section => self.doc.set(section, field.key, value),
        }
        self.screen = self.edit_origin();
        self.status = format!("{} updated", field.key);
    }

    // ---- Seeded boards (#147) --------------------------------------------

    /// Rows on the seed screen: the guest-password field, then each board.
    fn open_seed(&mut self) {
        self.seed_sel = 0;
        self.seed_on_password = true;
        // The check touches the database, so do it once on open rather than on
        // every keystroke.
        let url = self
            .doc
            .get("network", "database_url")
            .map(|v| v.display())
            .unwrap_or_default();
        let dir = self
            .doc
            .path()
            .parent()
            .unwrap_or_else(|| std::path::Path::new("."))
            .to_path_buf();
        self.seed_status = seed::status(&url, &dir);
        self.screen = Screen::SeedBoards;
    }

    /// The field of the seed board currently being edited.
    pub fn seed_board_field(&self) -> Option<&'static Field> {
        SEED_BOARD_FIELDS.get(self.seed_field_sel)
    }

    /// How many rows the seed screen shows: 1 (guest password) + one per board.
    fn seed_row_count(&self) -> usize {
        1 + self.doc.seed_board_names().len()
    }

    fn on_seed(&mut self, key: KeyEvent) -> Action {
        let boards = self.doc.seed_board_names().len();
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                self.seed_sel = self.seed_sel.saturating_sub(1);
                self.seed_on_password = self.seed_sel == 0;
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if self.seed_sel + 1 < self.seed_row_count() {
                    self.seed_sel += 1;
                }
                self.seed_on_password = self.seed_sel == 0;
            }
            KeyCode::Enter => {
                if self.seed_on_password {
                    // The password is a plain field on [seed].
                    self.input = self
                        .doc
                        .effective("seed", "guest_password")
                        .map(|v| v.display())
                        .unwrap_or_default();
                    self.edit_from = EditFrom::Section;
                    self.field_sel = 0; // guest_password is [seed]'s only field
                    self.screen = Screen::Edit;
                } else {
                    self.seed_field_sel = 0;
                    self.screen = Screen::SeedBoardFields;
                }
            }
            KeyCode::Char('a') => {
                self.seed_sel = 1 + self.doc.seed_board_add("New board");
                self.seed_on_password = false;
                self.seed_field_sel = 0;
                self.screen = Screen::SeedBoardFields;
                self.status = "Added a board.".into();
            }
            KeyCode::Char('d') if !self.seed_on_password && boards > 0 => {
                self.screen = Screen::ConfirmRemoveSeedBoard;
            }
            KeyCode::Char('s') => self.open_save(),
            KeyCode::Esc | KeyCode::Left | KeyCode::Char('q') => self.screen = Screen::Sections,
            _ => {}
        }
        Action::None
    }

    /// The board index the selection points at (row 0 is the password).
    fn seed_board_index(&self) -> usize {
        self.seed_sel.saturating_sub(1)
    }

    fn on_seed_board_fields(&mut self, key: KeyEvent) -> Action {
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                self.seed_field_sel = self.seed_field_sel.saturating_sub(1);
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if self.seed_field_sel + 1 < SEED_BOARD_FIELDS.len() {
                    self.seed_field_sel += 1;
                }
            }
            KeyCode::Enter => self.begin_seed_board_edit(),
            KeyCode::Char('s') => self.open_save(),
            KeyCode::Esc | KeyCode::Left | KeyCode::Char('q') => self.screen = Screen::SeedBoards,
            _ => {}
        }
        Action::None
    }

    fn begin_seed_board_edit(&mut self) {
        let Some(field) = self.seed_board_field() else {
            return;
        };
        let idx = self.seed_board_index();
        match field.kind {
            FieldKind::Enum(options) => {
                let current = self
                    .doc
                    .seed_board_get(idx, field.key)
                    .map(|v| v.display())
                    .unwrap_or_default();
                let pos = options.iter().position(|o| *o == current).unwrap_or(0);
                let next = options[(pos + 1) % options.len()];
                self.doc.seed_board_set(idx, field.key, next);
                self.status = format!("{} = {next}", field.key);
            }
            _ => {
                self.input = self
                    .doc
                    .seed_board_get(idx, field.key)
                    .map(|v| v.display())
                    .unwrap_or_default();
                self.seed_sel = idx + 1;
                self.edit_from = EditFrom::SeedBoard;
                self.screen = Screen::Edit;
                self.status = field.help.to_string();
            }
        }
    }

    fn on_confirm_remove_seed_board(&mut self, key: KeyEvent) -> Action {
        match key.code {
            KeyCode::Char('y') => {
                let idx = self.seed_board_index();
                let name = self
                    .doc
                    .seed_board_names()
                    .get(idx)
                    .cloned()
                    .unwrap_or_default();
                self.doc.seed_board_remove(idx);
                self.seed_sel = self.seed_sel.min(self.seed_row_count().saturating_sub(1));
                self.seed_on_password = self.seed_sel == 0;
                self.screen = Screen::SeedBoards;
                self.status = format!("Removed {name}.");
            }
            _ => {
                self.screen = Screen::SeedBoards;
                self.status = "Kept.".into();
            }
        }
        Action::None
    }

    // ---- Per-screen art (#146) -------------------------------------------

    /// The art rows: every screen that can carry art, with its label and the
    /// file currently set.
    pub fn art_rows(&self) -> Vec<(&'static str, &'static str, String)> {
        ART_SCREEN_KEYS
            .iter()
            .map(|(key, label)| {
                (
                    *key,
                    *label,
                    self.doc.art_screen_get(key).unwrap_or_default(),
                )
            })
            .collect()
    }

    fn on_art_screens(&mut self, key: KeyEvent) -> Action {
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                self.art_sel = self.art_sel.saturating_sub(1);
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if self.art_sel + 1 < ART_SCREEN_KEYS.len() {
                    self.art_sel += 1;
                }
            }
            KeyCode::Enter => {
                let (k, _) = ART_SCREEN_KEYS[self.art_sel];
                self.input = self.doc.art_screen_get(k).unwrap_or_default();
                self.edit_from = EditFrom::ArtScreen;
                self.screen = Screen::Edit;
                self.status = ART_FILE_FIELD.help.to_string();
            }
            // Clearing is common enough to deserve its own key rather than
            // making the operator open the editor and delete the text.
            KeyCode::Char('u') => {
                let (k, label) = ART_SCREEN_KEYS[self.art_sel];
                self.status = if self.doc.art_screen_unset(k) {
                    format!("{label}: art removed")
                } else {
                    format!("{label} has no art set")
                };
            }
            KeyCode::Char('s') => self.open_save(),
            KeyCode::Esc | KeyCode::Left | KeyCode::Char('q') => self.screen = Screen::Sections,
            _ => {}
        }
        Action::None
    }

    // ---- Door games (#145) ----------------------------------------------

    /// The field of the door currently being edited.
    pub fn door_field(&self) -> Option<&'static schema::Field> {
        DOOR_FIELDS.get(self.door_field_sel)
    }

    /// A door's value for display, and whether it's set at all.
    pub fn door_shown_value(&self, key: &str) -> (String, bool) {
        match self.doc.door_get(self.door_sel, key) {
            Some(v) => (v.display(), true),
            None => (String::new(), false),
        }
    }

    fn on_doors(&mut self, key: KeyEvent) -> Action {
        let count = self.doc.door_count();
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                self.door_sel = self.door_sel.saturating_sub(1);
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if self.door_sel + 1 < count {
                    self.door_sel += 1;
                }
            }
            KeyCode::Enter | KeyCode::Right if count > 0 => {
                self.door_field_sel = 0;
                self.screen = Screen::DoorFields;
            }
            KeyCode::Char('a') => {
                // Pre-filled so the entry is valid the moment it exists; an
                // operator who adds a door and quits shouldn't leave a config
                // that won't parse.
                self.door_sel = self.doc.door_add("New door", "/path/to/program");
                self.door_field_sel = 0;
                self.screen = Screen::DoorFields;
                self.status = "Added a door — set its command.".into();
            }
            KeyCode::Char('d') if count > 0 => self.screen = Screen::ConfirmRemoveDoor,
            // Menu order is what callers see, so it's worth being able to change.
            KeyCode::Char('K') => {
                if let Some(to) = self.doc.door_move(self.door_sel, true) {
                    self.door_sel = to;
                }
            }
            KeyCode::Char('J') => {
                if let Some(to) = self.doc.door_move(self.door_sel, false) {
                    self.door_sel = to;
                }
            }
            KeyCode::Char('s') => self.open_save(),
            KeyCode::Esc | KeyCode::Left | KeyCode::Char('q') => self.screen = Screen::Sections,
            _ => {}
        }
        Action::None
    }

    fn on_door_fields(&mut self, key: KeyEvent) -> Action {
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                self.door_field_sel = self.door_field_sel.saturating_sub(1);
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if self.door_field_sel + 1 < DOOR_FIELDS.len() {
                    self.door_field_sel += 1;
                }
            }
            KeyCode::Enter => self.begin_door_edit(),
            KeyCode::Char('s') => self.open_save(),
            KeyCode::Esc | KeyCode::Left | KeyCode::Char('q') => self.screen = Screen::Doors,
            _ => {}
        }
        Action::None
    }

    /// Same rules as a section field: enums cycle in place, everything else
    /// opens the text editor primed with the current value.
    fn begin_door_edit(&mut self) {
        let Some(field) = self.door_field() else {
            return;
        };
        match field.kind {
            FieldKind::Enum(options) => {
                let current = self
                    .doc
                    .door_get(self.door_sel, field.key)
                    .map(|v| v.display())
                    .unwrap_or_default();
                let idx = options.iter().position(|o| *o == current).unwrap_or(0);
                let next = options[(idx + 1) % options.len()];
                self.doc
                    .door_set(self.door_sel, field.key, FieldValue::Str(next.to_string()));
                self.status = if next.is_empty() {
                    format!("{} = (none)", field.key)
                } else {
                    format!("{} = {next}", field.key)
                };
            }
            _ => {
                self.input = self
                    .doc
                    .door_get(self.door_sel, field.key)
                    .map(|v| v.display())
                    .unwrap_or_default();
                self.edit_from = EditFrom::Door;
                self.screen = Screen::Edit;
                self.status = field.help.to_string();
            }
        }
    }

    /// Removing a door is the one destructive action in the editor, so it asks.
    fn on_confirm_remove_door(&mut self, key: KeyEvent) -> Action {
        match key.code {
            KeyCode::Char('y') => {
                let name = self
                    .doc
                    .door_names()
                    .get(self.door_sel)
                    .cloned()
                    .unwrap_or_default();
                self.doc.door_remove(self.door_sel);
                self.door_sel = self.door_sel.min(self.doc.door_count().saturating_sub(1));
                self.screen = Screen::Doors;
                self.status = format!("Removed {name}. Its files on disk are untouched.");
            }
            _ => {
                self.screen = Screen::Doors;
                self.status = "Kept.".into();
            }
        }
        Action::None
    }

    fn open_save(&mut self) {
        self.issues = self.doc.validate();
        self.screen = Screen::Save;
    }

    /// Sections that changed, and of those, which need a restart.
    pub fn pending(&self) -> (Vec<&'static str>, Vec<&'static str>) {
        (self.doc.changed_sections(), self.doc.restart_needed())
    }

    pub fn blocking_issues(&self) -> Vec<&Issue> {
        self.issues.iter().filter(|i| i.blocking).collect()
    }

    fn on_save(&mut self, key: KeyEvent) -> Action {
        match key.code {
            KeyCode::Char('y') | KeyCode::Enter => {
                // A blocking problem means the board wouldn't start. Writing it
                // anyway would turn a caught mistake into a failed boot, so the
                // save is refused rather than merely warned about.
                if !self.blocking_issues().is_empty() {
                    self.status =
                        "Fix the blocking problems first — this config would not start.".into();
                    return Action::None;
                }
                match self.doc.save() {
                    Ok(()) => {
                        self.status = format!("Saved {}", self.doc.path().display());
                        self.screen = Screen::Sections;
                    }
                    Err(e) => self.status = format!("Could not save: {e:#}"),
                }
            }
            KeyCode::Char('n') | KeyCode::Esc => {
                self.screen = Screen::Sections;
                self.status = "Not saved.".into();
            }
            _ => {}
        }
        Action::None
    }

    /// Quitting with unsaved edits asks first — this tool's whole job is a file
    /// the operator cares about.
    fn request_quit(&mut self) -> Action {
        if self.doc.is_dirty() {
            self.screen = Screen::ConfirmQuit;
            Action::None
        } else {
            Action::Quit
        }
    }

    fn on_confirm_quit(&mut self, key: KeyEvent) -> Action {
        match key.code {
            KeyCode::Char('y') => Action::Quit,
            KeyCode::Char('s') => {
                self.open_save();
                Action::None
            }
            _ => {
                self.screen = Screen::Sections;
                self.status = "Still editing.".into();
                Action::None
            }
        }
    }
}
