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
    Polls,
    ViewPoll,
    ComposePoll,
    Chat,
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
    MailSysop,
    ConfirmDeleteMail,
    MailSearchInput,
    MailSearchResults,
    WhoOnline,
    ComposePage,
    Profile,
    IgnoreList,
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
    ChangePassword,
    Help,
    AdminUsers,
    AdminLogins,
    AdminAudit,
    AdminFederation,
    ComposeFederation,
    ComposeBroadcast,
}

/// Actions reachable from the main menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuItem {
    Bulletins,
    Boards,
    Oneliners,
    Polls,
    Timeline,
    RemoteBoards,
    Mail,
    MailSysop,
    Who,
    Chat,
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
    /// The built-in default label, used when the operator hasn't set one.
    pub fn label(self) -> &'static str {
        match self {
            MenuItem::Bulletins => "Bulletins",
            MenuItem::Boards => "Message Boards",
            MenuItem::Oneliners => "Oneliners",
            MenuItem::Polls => "Polls",
            MenuItem::Timeline => "Timeline",
            MenuItem::RemoteBoards => "Remote Boards",
            MenuItem::Mail => "Private Mail",
            MenuItem::MailSysop => "Mail Sysop",
            MenuItem::Who => "Who's Online",
            MenuItem::Chat => "Chat",
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

    /// The stable identifier an operator names this target by in `[[menu]]`
    /// config (#84). Distinct from [`label`](Self::label), which is display text
    /// the operator may change; the action is the wiring and never changes.
    pub fn action(self) -> &'static str {
        match self {
            MenuItem::Bulletins => "bulletins",
            MenuItem::Boards => "boards",
            MenuItem::Oneliners => "oneliners",
            MenuItem::Polls => "polls",
            MenuItem::Timeline => "timeline",
            MenuItem::RemoteBoards => "remote_boards",
            MenuItem::Mail => "mail",
            MenuItem::MailSysop => "mail_sysop",
            MenuItem::Who => "who",
            MenuItem::Chat => "chat",
            MenuItem::Profile => "profile",
            MenuItem::Stats => "stats",
            MenuItem::Search => "search",
            MenuItem::Doors => "doors",
            MenuItem::Files => "files",
            MenuItem::Keys => "keys",
            MenuItem::Register => "register",
            MenuItem::Admin => "admin",
            MenuItem::Help => "help",
            MenuItem::Quit => "quit",
        }
    }

    /// Resolve a config `action` string to its target. Unknown actions return
    /// `None`, so a typo in `[[menu]]` drops that entry rather than crashing.
    pub fn from_action(action: &str) -> Option<Self> {
        // Every variant's `action()` is checked, so this can't drift out of sync.
        const ALL: [MenuItem; 20] = [
            MenuItem::Bulletins,
            MenuItem::Boards,
            MenuItem::Oneliners,
            MenuItem::Polls,
            MenuItem::Timeline,
            MenuItem::RemoteBoards,
            MenuItem::Mail,
            MenuItem::MailSysop,
            MenuItem::Who,
            MenuItem::Chat,
            MenuItem::Profile,
            MenuItem::Stats,
            MenuItem::Search,
            MenuItem::Doors,
            MenuItem::Files,
            MenuItem::Keys,
            MenuItem::Register,
            MenuItem::Admin,
            MenuItem::Help,
            MenuItem::Quit,
        ];
        let action = action.trim().to_ascii_lowercase();
        ALL.into_iter().find(|i| i.action() == action)
    }

    /// The built-in default hotkey (#84), used when the operator hasn't set one.
    /// Chosen to be distinct across every item so the default menu has no
    /// collisions; an operator's `key` override may of course reuse a letter.
    pub fn default_key(self) -> char {
        match self {
            MenuItem::Bulletins => 'n', // "news"
            MenuItem::Boards => 'b',
            MenuItem::Oneliners => 'o',
            MenuItem::Polls => 'v', // "voting booth"
            MenuItem::Timeline => 't',
            MenuItem::RemoteBoards => 'r',
            MenuItem::Mail => 'm',
            MenuItem::MailSysop => 'e', // "feedback"
            MenuItem::Who => 'w',
            MenuItem::Chat => 'c',
            MenuItem::Profile => 'p',
            MenuItem::Stats => 's',
            MenuItem::Search => '/',
            MenuItem::Doors => 'd',
            MenuItem::Files => 'f',
            MenuItem::Keys => 'k',
            MenuItem::Register => 'g', // "get access"
            MenuItem::Admin => 'a',
            MenuItem::Help => 'h',
            MenuItem::Quit => 'q',
        }
    }
}

/// What a menu entry does (#86). A built-in screen, or a compound target: a
/// named door, a board opened directly, or a submenu to descend into.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MenuAction {
    Builtin(MenuItem),
    /// Launch the `[[doors]]` entry with this name.
    Door(String),
    /// Open the board with this name directly.
    Board(String),
    /// Push the `[[submenus.<name>]]` group.
    Submenu(String),
}

impl MenuAction {
    /// Parse a config `action` string. `door:`/`board:`/`submenu:` prefixes give
    /// the compound targets; anything else is a built-in id. Returns `None` only
    /// for an unknown built-in or an empty compound name.
    pub fn parse(action: &str) -> Option<Self> {
        let action = action.trim();
        for (prefix, ctor) in [
            ("door:", MenuAction::Door as fn(String) -> MenuAction),
            ("board:", MenuAction::Board as fn(String) -> MenuAction),
            ("submenu:", MenuAction::Submenu as fn(String) -> MenuAction),
        ] {
            if let Some(rest) = action.strip_prefix(prefix) {
                let name = rest.trim();
                return (!name.is_empty()).then(|| ctor(name.to_string()));
            }
        }
        MenuItem::from_action(action).map(MenuAction::Builtin)
    }

    /// A default hotkey when the operator didn't set one: the built-in's key, or
    /// the first letter of a compound target's name.
    pub fn default_key(&self, name_hint: &str) -> Option<char> {
        match self {
            MenuAction::Builtin(item) => Some(item.default_key()),
            _ => name_hint
                .chars()
                .find(|c| c.is_alphanumeric())
                .map(|c| c.to_ascii_lowercase()),
        }
    }
}

/// A resolved menu entry (#84, #86): the [`MenuAction`] it dispatches to, plus
/// the operator-chosen (or defaulted) display label, hotkey, and placement.
#[derive(Debug, Clone)]
pub struct MenuEntry {
    pub action: MenuAction,
    pub label: String,
    pub key: Option<char>,
    /// Placement on an ANSI menu backdrop (#85); `None` = the list layout.
    pub row: Option<u16>,
    pub col: Option<u16>,
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
