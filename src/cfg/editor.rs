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
use super::schema::{self, FieldKind, SECTIONS, Section};

/// What the editor is showing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    /// The list of `[sections]`.
    Sections,
    /// The settings within one section.
    Fields,
    /// Editing one setting.
    Edit,
    /// Reviewing what's about to be written.
    Save,
    /// Confirming a quit that would discard changes.
    ConfirmQuit,
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
            KeyCode::Enter | KeyCode::Right => {
                self.field_sel = 0;
                self.screen = Screen::Fields;
            }
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
                self.screen = Screen::Fields;
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

    /// Apply the typed value, refusing anything the schema says is out of range
    /// rather than writing a config the server would reject at boot.
    fn commit_edit(&mut self) {
        let Some(field) = self.field() else { return };
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

        self.doc.set(section, field.key, value);
        self.screen = Screen::Fields;
        self.status = format!("{} updated", field.key);
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
