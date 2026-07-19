//! Screen definitions, menu model, and the simple multi-field form used by
//! the compose/register screens.

/// Which screen the session is currently showing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Screen {
    MainMenu,
    Bulletins,
    ReadBulletin,
    Oneliners,
    ComposeOneliner,
    Timeline,
    FollowRemote,
    RemoteBoards,
    RemoteBoardPosts,
    ComposeRemotePost,
    BoardList,
    MessageList,
    ReadMessage,
    ComposePost,
    Mailbox,
    ReadMail,
    ComposeMail,
    ConfirmDeleteMail,
    WhoOnline,
    ComposePage,
    Profile,
    EditProfile,
    Stats,
    SearchInput,
    SearchResults,
    Doors,
    FileAreas,
    FileList,
    FileDetail,
    EditFileDesc,
    ArchiveList,
    FileView,
    Keys,
    AddKey,
    Register,
    Help,
    AdminUsers,
    AdminLogins,
}

/// Actions reachable from the main menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuItem {
    Bulletins,
    Boards,
    Oneliners,
    Timeline,
    RemoteBoards,
    Mail,
    Who,
    Profile,
    Stats,
    Search,
    Doors,
    Files,
    Keys,
    Register,
    Admin,
    Help,
    Quit,
}

impl MenuItem {
    pub fn label(self) -> &'static str {
        match self {
            MenuItem::Bulletins => "Bulletins",
            MenuItem::Boards => "Message Boards",
            MenuItem::Oneliners => "Oneliners",
            MenuItem::Timeline => "Timeline",
            MenuItem::RemoteBoards => "Remote Boards",
            MenuItem::Mail => "Private Mail",
            MenuItem::Who => "Who's Online",
            MenuItem::Profile => "My Profile",
            MenuItem::Stats => "Stats",
            MenuItem::Search => "Search Messages",
            MenuItem::Doors => "Door Games",
            MenuItem::Files => "File Areas",
            MenuItem::Keys => "SSH Keys",
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

/// One row of a mirrored board's thread view.
///
/// Mirrored posts and our own not-yet-published submissions are merged into a
/// single ordered list so a pending reply sits under the post it answers,
/// instead of floating at the top detached from its conversation. Keeping them
/// in two lists would reproduce, on the sending side, exactly the flat-thread
/// problem #139 Slice B fixed on the receiving side.
#[derive(Debug, Clone)]
pub struct MirrorRow {
    /// The post's permanent URI — what a reply to it points at.
    pub ap_id: String,
    pub subject: String,
    pub author_handle: String,
    pub published: i64,
    pub body: String,
    pub depth: u16,
    /// True for our own submission the board hasn't announced back yet.
    pub pending: bool,
}
