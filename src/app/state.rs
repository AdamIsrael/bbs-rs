//! Screen definitions, menu model, and the simple multi-field form used by
//! the compose/register screens.

/// Which screen the session is currently showing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    MainMenu,
    BoardList,
    MessageList,
    ReadMessage,
    ComposePost,
    Mailbox,
    ReadMail,
    ComposeMail,
    WhoOnline,
    Register,
    Help,
    AdminUsers,
    AdminLogins,
}

/// Actions reachable from the main menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuItem {
    Boards,
    Mail,
    Who,
    Register,
    Admin,
    Help,
    Quit,
}

impl MenuItem {
    pub fn label(self) -> &'static str {
        match self {
            MenuItem::Boards => "Message Boards",
            MenuItem::Mail => "Private Mail",
            MenuItem::Who => "Who's Online",
            MenuItem::Register => "Register New Account",
            MenuItem::Admin => "Admin",
            MenuItem::Help => "Help",
            MenuItem::Quit => "Quit",
        }
    }
}

/// A single editable, single-line form field.
#[derive(Debug, Clone)]
pub struct Field {
    pub label: String,
    pub value: String,
    pub secret: bool,
}

impl Field {
    pub fn new(label: &str, secret: bool) -> Self {
        Self {
            label: label.to_string(),
            value: String::new(),
            secret,
        }
    }
}

/// A tiny form: a list of single-line fields plus which one has focus.
/// Enter advances focus and, on the last field, signals submit.
#[derive(Debug, Clone)]
pub struct Form {
    pub fields: Vec<Field>,
    pub focus: usize,
}

impl Form {
    pub fn new(fields: Vec<Field>) -> Self {
        Self { fields, focus: 0 }
    }

    pub fn focused_mut(&mut self) -> &mut Field {
        &mut self.fields[self.focus]
    }

    pub fn insert(&mut self, c: char) {
        self.focused_mut().value.push(c);
    }

    pub fn backspace(&mut self) {
        self.focused_mut().value.pop();
    }

    pub fn next_field(&mut self) {
        if self.focus + 1 < self.fields.len() {
            self.focus += 1;
        }
    }

    pub fn prev_field(&mut self) {
        self.focus = self.focus.saturating_sub(1);
    }

    /// True when focus is on the final field (Enter there submits).
    pub fn on_last(&self) -> bool {
        self.focus + 1 == self.fields.len()
    }

    pub fn value(&self, idx: usize) -> &str {
        self.fields[idx].value.trim()
    }
}
