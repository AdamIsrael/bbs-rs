//! The transport-agnostic TUI application: state, the async event loop, and
//! per-screen key handling. Rendering lives in [`ui`].

pub mod ansi;
pub mod door;
pub mod state;
pub mod textarea;
pub mod theme;
pub mod ui;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::Rect;
use ratatui::text::Text;
use sqlx::sqlite::SqlitePool;
use tokio::sync::mpsc::Receiver;

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use crate::app::theme::Theme;

use crate::config::Settings;
use crate::db::models::{
    Board, Bulletin, FileArea, FileEntry, Login, Mail, Message, Oneliner, User, UserKey,
};
use crate::error::AppError;
use crate::services::archive::{self, ArchiveEntry, Preview};
use crate::services::boards::ThreadItem;
use crate::services::presence::{OnlineUser, Presence};
use crate::services::profiles::{self, Profile};
use crate::services::search::{self, SearchHit};
use crate::services::stats::{self, Stats};
use crate::services::{
    admin, audit, auth, blocks, boards, bulletins, files, keys, mail, oneliners,
};
use crate::ssh::pubkey;
use crate::transport::{Event, Transport};
use crate::util::{now_unix, reply_subject};

use state::{Field, Form, MenuAction, MenuEntry, MenuItem, MirrorRow, Screen};

/// One suspended menu level while a submenu is open (#86). Holds the parent
/// menu's entries and selection so popping restores it exactly, plus the
/// parent's title for the breadcrumb.
pub struct MenuFrame {
    pub menu: Vec<MenuEntry>,
    pub sel: usize,
    pub title: Option<String>,
}

/// Deepest submenu nesting allowed (#86). A guard against a config that cycles
/// (`a → b → a`) or nests absurdly; descending past it is simply ignored.
const MAX_MENU_DEPTH: usize = 16;

/// All per-session state.
pub struct App {
    pool: SqlitePool,
    presence: Presence,
    pub config: Arc<Settings>,
    /// Resolved color theme (from `config.theme`).
    pub theme: Theme,
    /// Operator ANSI/text art, keyed by the screen it heads (the main-menu
    /// welcome art is stored under `Screen::MainMenu`). Empty when unconfigured.
    pub art: HashMap<Screen, Text<'static>>,
    pub user: User,
    session_id: usize,
    /// How this session connected. The app is otherwise transport-agnostic;
    /// this drives the few spots where it matters (e.g. telling a browser user
    /// the SSH address, and an SSH user the web URL).
    pub transport: Transport,

    pub screen: Screen,
    pub should_quit: bool,
    pub status: String,

    // Main menu
    pub menu: Vec<MenuEntry>,
    pub menu_sel: usize,
    /// Submenu breadcrumb (#86): each frame is the parent menu we descended
    /// from, with its selection preserved so a pop restores it. Empty at the
    /// top-level main menu.
    pub menu_stack: Vec<MenuFrame>,
    /// Title of the menu currently shown; `None` at the top-level main menu.
    pub menu_title: Option<String>,

    // Bulletins
    pub bulletins: Vec<Bulletin>,
    pub bulletin_sel: usize,
    pub current_bulletin: Option<Bulletin>,

    // Oneliners (graffiti wall)
    pub oneliners: Vec<Oneliner>,

    // Federated timeline: cached statuses from followed remote accounts.
    pub timeline: Vec<crate::services::federation::timeline::Entry>,
    pub timeline_sel: usize,
    // Subscribed remote boards and the mirrored posts of the open one (#132).
    // Deliberately separate from `boards`/`messages`: these are cached copies of
    // someone else's board, not content we're the authority for.
    pub remote_boards: Vec<crate::services::federation::mirror::Board>,
    pub remote_board_sel: usize,
    pub mirror_rows: Vec<MirrorRow>,
    pub mirror_sel: usize,
    pub current_remote_board: Option<crate::services::federation::mirror::Board>,
    /// The remote post URI being replied to, while composing (#139 Slice C).
    pub remote_reply_to: Option<String>,

    // Boards
    pub boards: Vec<Board>,
    pub board_sel: usize,
    pub current_board: Option<Board>,
    /// Unread message counts per board id ("new since last call"), empty for
    /// guests (whose shared account has no meaningful watermark).
    pub board_unread: std::collections::HashMap<i64, i64>,
    /// Unread private-mail count, surfaced as a login notice and a main-menu
    /// badge. Always 0 for guests (they can't receive mail) and when the
    /// private-mail feature is disabled.
    pub mail_unread: i64,

    // Messages (threaded: each item carries its reply depth)
    pub messages: Vec<ThreadItem>,
    pub msg_sel: usize,
    pub current_message: Option<Message>,
    /// The post being edited (#92), if in edit mode; `None` for a new post.
    edit_target: Option<i64>,
    /// The current board's seen-watermark captured on open: a message newer
    /// than this (and not the viewer's own) is highlighted as new. `i64::MAX`
    /// suppresses highlighting (guests, or before a board is opened).
    msg_seen_threshold: i64,
    /// The signature of the message currently being read (empty if none), shown
    /// beneath its body.
    pub current_msg_signature: String,
    /// When composing, the message being replied to (None = new thread).
    reply_parent: Option<i64>,

    // Profiles
    pub current_profile: Option<Profile>,
    /// Where the profile screen returns to (main menu, or who's-online).
    profile_back: Screen,
    /// Whether the viewer has blocked the currently-shown profile (#97).
    pub current_profile_blocked: bool,

    // Ignore / block list (#97)
    pub ignored: Vec<(i64, String)>,
    pub ignored_sel: usize,

    // Stats / leaderboards
    pub stats: Option<Stats>,

    // Message search
    pub search_results: Vec<SearchHit>,
    pub search_sel: usize,
    /// The query that produced `search_results` (shown in the results title).
    pub search_query: String,

    // Door games
    pub door_sel: usize,
    /// Set when the user picks a door; the run loop launches it and clears this.
    pub pending_door: Option<usize>,

    // Mail
    pub mails: Vec<Mail>,
    pub mail_sel: usize,
    pub current_mail: Option<Mail>,
    /// Where to return after a mail-delete confirmation (#70).
    mail_delete_return: Screen,
    // Mail full-text search (#93)
    pub mail_search: Vec<Mail>,
    pub mail_search_sel: usize,
    pub mail_search_query: String,

    // Who's online
    pub online: Vec<OnlineUser>,
    pub who_sel: usize,
    /// The user being paged, while the page-compose screen is open (#68).
    page_target: Option<String>,

    // SSH keys (the current user's own)
    pub user_keys: Vec<UserKey>,
    pub key_sel: usize,

    // File areas
    pub file_areas: Vec<FileArea>,
    pub file_area_sel: usize,
    pub current_file_area: Option<FileArea>,
    pub files: Vec<FileEntry>,
    pub file_sel: usize,
    pub current_file: Option<FileEntry>,

    // Archive entry listing + inline text viewer
    pub archive_entries: Vec<ArchiveEntry>,
    pub archive_sel: usize,
    pub archive_truncated: bool,
    pub file_view_title: String,
    pub file_view_body: String,
    pub file_view_scroll: u16,
    pub file_view_truncated: bool,
    file_view_back: Screen,

    // Admin
    pub admin_users: Vec<User>,
    pub admin_user_sel: usize,
    pub admin_logins: Vec<Login>,
    pub admin_login_sel: usize,
    pub admin_audit: Vec<crate::db::models::AuditEntry>,
    pub admin_audit_sel: usize,
    // Federation domain policy (#159): (kind, domain, reason, severity) rows.
    pub fed_policy: Vec<(String, String, String, String)>,
    pub fed_sel: usize,
    /// While the federation-domain composer is open, the (kind, severity) the
    /// typed domain will be set to.
    fed_pending: Option<(&'static str, &'static str)>,

    // Shared form for compose/register screens
    pub form: Form,
    /// The multi-line body buffer for post/mail compose (#96). The header
    /// fields stay in `form`; focus moves between the two.
    pub body: crate::app::textarea::TextArea,
    pub body_focused: bool,
}

/// Whether a menu target is available to `user` under the current config (#84).
/// The gate a configured `[[menu]]` entry passes through — same rules that used
/// to be inline `if` blocks — so an entry for a disabled feature or a role the
/// user lacks is simply dropped.
fn menu_item_available(item: MenuItem, config: &Settings, user: &User) -> bool {
    let f = &config.features;
    match item {
        MenuItem::Bulletins
        | MenuItem::Boards
        | MenuItem::Stats
        | MenuItem::Search
        | MenuItem::Help
        | MenuItem::Quit => true,
        MenuItem::Oneliners => f.oneliners,
        MenuItem::Timeline | MenuItem::RemoteBoards => config.federation.enabled,
        MenuItem::Mail => f.private_mail,
        MenuItem::Who => f.who_online,
        MenuItem::Profile => !user.is_guest(),
        MenuItem::Doors => !config.doors.is_empty(),
        MenuItem::Files => f.file_areas,
        MenuItem::Keys => f.pubkey_auth && !user.is_guest(),
        MenuItem::Register => user.is_guest() && f.registration,
        MenuItem::Admin => user.is_admin(),
    }
}

/// The built-in menu order, used when no `[[menu]]` is configured. Matches the
/// classic layout before the menu became config-driven.
fn default_menu_order() -> [MenuItem; 17] {
    use MenuItem::*;
    [
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
    ]
}

/// Whether a resolved [`MenuAction`] is reachable under the current config and
/// role (#86). Built-ins defer to [`menu_item_available`]; a `door:`/`submenu:`
/// target is dropped when its name isn't configured (a dangling target); a
/// `board:` target is always shown and validated when activated (board access
/// is per-message-visibility, not a menu-time gate).
fn menu_action_available(action: &MenuAction, config: &Settings, user: &User) -> bool {
    match action {
        MenuAction::Builtin(item) => menu_item_available(*item, config, user),
        MenuAction::Door(name) => config.doors.iter().any(|d| &d.name == name),
        MenuAction::Submenu(name) => config.submenus.contains_key(name),
        MenuAction::Board(_) => true,
    }
}

/// Resolve one configured entry group into displayable [`MenuEntry`]s (#86),
/// dropping entries whose action is unknown or unavailable. Shared by the main
/// menu and every submenu so nesting behaves identically at each level.
fn build_menu_group(
    entries: &[crate::config::MenuEntry],
    config: &Settings,
    user: &User,
) -> Vec<MenuEntry> {
    entries
        .iter()
        .filter_map(|e| {
            let action = MenuAction::parse(&e.action)?;
            if !menu_action_available(&action, config, user) {
                return None;
            }
            let default_label = match &action {
                MenuAction::Builtin(item) => item.label().to_string(),
                MenuAction::Door(n) | MenuAction::Board(n) | MenuAction::Submenu(n) => n.clone(),
            };
            let label = if e.label.trim().is_empty() {
                default_label
            } else {
                e.label.trim().to_string()
            };
            let key = e.key.chars().next().or_else(|| action.default_key(&label));
            Some(MenuEntry {
                action,
                label,
                key,
                row: e.row,
                col: e.col,
            })
        })
        .collect()
}

/// Build the resolved main menu for a session (#84): the configured `[[menu]]`
/// when the operator set one (array order, each entry's label/key falling back
/// to the target's default), otherwise the built-in default set. Both are
/// filtered by [`menu_action_available`].
fn build_menu(config: &Settings, user: &User) -> Vec<MenuEntry> {
    if config.menu.is_empty() {
        default_menu_order()
            .into_iter()
            .filter(|&i| menu_item_available(i, config, user))
            .map(|item| MenuEntry {
                action: MenuAction::Builtin(item),
                label: item.label().to_string(),
                key: Some(item.default_key()),
                row: None,
                col: None,
            })
            .collect()
    } else {
        build_menu_group(&config.menu, config, user)
    }
}

impl App {
    pub fn new(
        pool: SqlitePool,
        presence: Presence,
        config: Arc<Settings>,
        user: User,
        session_id: usize,
        transport: Transport,
    ) -> Self {
        // The main menu is built from config when the operator defined one, else
        // the built-in default; either way it's filtered by the feature toggles
        // and role gates (#84).
        let menu = build_menu(&config, &user);
        let theme = Theme::resolve(&config.theme);
        let art = load_art(&config.art);
        Self {
            pool,
            presence,
            config,
            theme,
            art,
            user,
            session_id,
            transport,
            screen: Screen::MainMenu,
            should_quit: false,
            status: String::new(),
            menu,
            menu_sel: 0,
            menu_stack: Vec::new(),
            menu_title: None,
            bulletins: Vec::new(),
            bulletin_sel: 0,
            current_bulletin: None,
            oneliners: Vec::new(),
            timeline: Vec::new(),
            timeline_sel: 0,
            remote_boards: Vec::new(),
            remote_board_sel: 0,
            mirror_rows: Vec::new(),
            mirror_sel: 0,
            current_remote_board: None,
            remote_reply_to: None,
            boards: Vec::new(),
            board_sel: 0,
            current_board: None,
            board_unread: std::collections::HashMap::new(),
            mail_unread: 0,
            messages: Vec::new(),
            msg_sel: 0,
            current_message: None,
            edit_target: None,
            msg_seen_threshold: i64::MAX,
            current_msg_signature: String::new(),
            reply_parent: None,
            current_profile: None,
            profile_back: Screen::MainMenu,
            current_profile_blocked: false,
            ignored: Vec::new(),
            ignored_sel: 0,
            stats: None,
            search_results: Vec::new(),
            search_sel: 0,
            search_query: String::new(),
            door_sel: 0,
            pending_door: None,
            mails: Vec::new(),
            mail_sel: 0,
            current_mail: None,
            mail_delete_return: Screen::Mailbox,
            mail_search: Vec::new(),
            mail_search_sel: 0,
            mail_search_query: String::new(),
            online: Vec::new(),
            who_sel: 0,
            page_target: None,
            user_keys: Vec::new(),
            key_sel: 0,
            file_areas: Vec::new(),
            file_area_sel: 0,
            current_file_area: None,
            files: Vec::new(),
            file_sel: 0,
            current_file: None,
            archive_entries: Vec::new(),
            archive_sel: 0,
            archive_truncated: false,
            file_view_title: String::new(),
            file_view_body: String::new(),
            file_view_scroll: 0,
            file_view_truncated: false,
            file_view_back: Screen::FileDetail,
            admin_users: Vec::new(),
            admin_user_sel: 0,
            admin_logins: Vec::new(),
            admin_login_sel: 0,
            admin_audit: Vec::new(),
            admin_audit_sel: 0,
            fed_policy: Vec::new(),
            fed_sel: 0,
            fed_pending: None,
            form: Form::new(Vec::new()),
            body: crate::app::textarea::TextArea::new(),
            body_focused: false,
        }
    }

    /// Handle one decoded key press, mutating state and hitting services.
    pub async fn handle_key(&mut self, key: KeyEvent) {
        // Ctrl-C ends the session from anywhere.
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            self.should_quit = true;
            return;
        }
        self.status.clear();

        match self.screen {
            Screen::MainMenu => self.on_main_menu(key).await,
            Screen::Bulletins => self.on_bulletins(key).await,
            Screen::ReadBulletin => self.on_reader(key, Screen::Bulletins),
            Screen::Oneliners => self.on_oneliners(key).await,
            Screen::ComposeOneliner => self.on_compose_oneliner(key).await,
            Screen::Timeline => self.on_timeline(key).await,
            Screen::RemoteBoards => self.on_remote_boards(key).await,
            Screen::RemoteBoardPosts => self.on_remote_board_posts(key).await,
            Screen::ComposeRemotePost => self.on_compose_remote_post(key).await,
            Screen::FollowRemote => self.on_follow_remote(key).await,
            Screen::BoardList => self.on_board_list(key).await,
            Screen::MessageList => self.on_message_list(key).await,
            Screen::ReadMessage => self.on_read_message(key).await,
            Screen::ComposePost => self.on_compose_post(key).await,
            Screen::Mailbox => self.on_mailbox(key).await,
            Screen::MailSearchInput => self.on_mail_search_input(key).await,
            Screen::MailSearchResults => self.on_mail_search_results(key).await,
            Screen::ReadMail => self.on_read_mail(key).await,
            Screen::ConfirmDeleteMail => self.on_confirm_delete_mail(key).await,
            Screen::ComposeMail => self.on_compose_mail(key).await,
            Screen::WhoOnline => self.on_who(key).await,
            Screen::ComposePage => self.on_compose_page(key).await,
            Screen::Profile => self.on_profile(key).await,
            Screen::IgnoreList => self.on_ignore_list(key).await,
            Screen::EditProfile => self.on_edit_profile(key).await,
            Screen::Stats => self.on_stats(key).await,
            Screen::SearchInput => self.on_search_input(key).await,
            Screen::SearchResults => self.on_search_results(key).await,
            Screen::Doors => self.on_doors(key),
            Screen::FileAreas => self.on_file_areas(key).await,
            Screen::FileList => self.on_file_list(key).await,
            Screen::FileDetail => self.on_file_detail(key).await,
            Screen::EditFileDesc => self.on_edit_file_desc(key).await,
            Screen::ArchiveList => self.on_archive_list(key).await,
            Screen::FileView => self.on_file_view(key),
            Screen::Keys => self.on_keys(key).await,
            Screen::AddKey => self.on_add_key(key).await,
            Screen::Register => self.on_register(key).await,
            Screen::Help => self.on_reader(key, Screen::MainMenu),
            Screen::AdminUsers => self.on_admin_users(key).await,
            Screen::AdminLogins => self.on_admin_logins(key).await,
            Screen::AdminAudit => self.on_admin_audit(key).await,
            Screen::AdminFederation => self.on_admin_federation(key).await,
            Screen::ComposeFederation => self.on_compose_federation(key).await,
            Screen::ComposeBroadcast => self.on_compose_broadcast(key).await,
        }
    }

    // ---- Main menu -------------------------------------------------------

    async fn on_main_menu(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Up => self.menu_sel = self.menu_sel.saturating_sub(1),
            KeyCode::Down => {
                self.menu_sel = (self.menu_sel + 1).min(self.menu.len().saturating_sub(1))
            }
            KeyCode::Enter => self.activate_menu().await,
            // Classic command-menu hotkeys (#84): a letter jumps to and runs the
            // matching entry. Esc pops out of a submenu, or quits at the top
            // level; `q` quits via the Quit entry's default key unless rebound.
            KeyCode::Char(c) => {
                if let Some(idx) = self.menu.iter().position(|e| e.key == Some(c)) {
                    self.menu_sel = idx;
                    self.activate_menu().await;
                }
            }
            KeyCode::Esc | KeyCode::Left => self.escape_menu(),
            _ => {}
        }
    }

    /// Back out of the current menu: pop to the parent submenu (#86), or quit
    /// when already at the top level.
    fn escape_menu(&mut self) {
        if !self.pop_submenu() {
            self.should_quit = true;
        }
    }

    /// Run the currently-selected menu entry, dispatching on its [`MenuAction`]
    /// (#86): a built-in screen, a named door, a board opened directly, or a
    /// submenu to descend into.
    async fn activate_menu(&mut self) {
        match self.menu[self.menu_sel].action.clone() {
            MenuAction::Builtin(item) => self.activate_item(item).await,
            MenuAction::Door(name) => self.launch_door_by_name(&name),
            MenuAction::Board(name) => self.open_board_by_name(&name).await,
            MenuAction::Submenu(name) => self.push_submenu(&name),
        }
    }

    async fn activate_item(&mut self, item: MenuItem) {
        match item {
            MenuItem::Bulletins => self.open_bulletins().await,
            MenuItem::Boards => self.open_boards().await,
            MenuItem::Oneliners => self.open_oneliners().await,
            MenuItem::Timeline => self.open_timeline().await,
            MenuItem::RemoteBoards => self.open_remote_boards().await,
            MenuItem::Mail => {
                if self.user.is_guest() {
                    self.status =
                        "Guests cannot use private mail — register an account first.".into();
                } else {
                    self.open_mailbox().await;
                }
            }
            MenuItem::Who => self.open_who().await,
            MenuItem::Profile => self.open_profile(self.user.id, Screen::MainMenu).await,
            MenuItem::Stats => self.open_stats().await,
            MenuItem::Search => self.begin_search(),
            MenuItem::Doors => {
                self.door_sel = 0;
                self.screen = Screen::Doors;
            }
            MenuItem::Files => self.open_file_areas().await,
            MenuItem::Keys => self.open_keys().await,
            MenuItem::Register => {
                self.form = Form::new(vec![
                    Field::new("Username", false),
                    Field::new("Password", true),
                    Field::new("Confirm password", true),
                ]);
                self.screen = Screen::Register;
            }
            MenuItem::Admin => self.open_admin_users().await,
            MenuItem::Help => self.screen = Screen::Help,
            MenuItem::Quit => self.should_quit = true,
        }
    }

    /// Descend into a named submenu (#86), pushing the current menu onto the
    /// stack so Esc restores it. Rejected if the submenu is unknown, resolves to
    /// no available items for this user, or nesting would exceed
    /// [`MAX_MENU_DEPTH`] (a cycle guard).
    fn push_submenu(&mut self, name: &str) {
        if self.menu_stack.len() >= MAX_MENU_DEPTH {
            self.status = "Menu nesting is too deep.".into();
            return;
        }
        let Some(entries) = self.config.submenus.get(name).cloned() else {
            self.status = format!("No such submenu: {name}");
            return;
        };
        let group = build_menu_group(&entries, &self.config, &self.user);
        if group.is_empty() {
            self.status = format!("Submenu '{name}' has no available items.");
            return;
        }
        // The activated entry's label names the level we're entering.
        let title = self.menu[self.menu_sel].label.clone();
        let parent = std::mem::replace(&mut self.menu, group);
        self.menu_stack.push(MenuFrame {
            menu: parent,
            sel: self.menu_sel,
            title: self.menu_title.take(),
        });
        self.menu_sel = 0;
        self.menu_title = Some(title);
    }

    /// Pop back to the parent menu (#86). Returns false at the top level, where
    /// the caller treats Esc as "quit".
    fn pop_submenu(&mut self) -> bool {
        match self.menu_stack.pop() {
            Some(frame) => {
                self.menu = frame.menu;
                self.menu_sel = frame.sel;
                self.menu_title = frame.title;
                true
            }
            None => false,
        }
    }

    /// Launch a door named directly from the menu (#86). The run loop picks up
    /// `pending_door` and bridges the program's raw I/O.
    fn launch_door_by_name(&mut self, name: &str) {
        match self.config.doors.iter().position(|d| d.name == name) {
            Some(idx) => self.pending_door = Some(idx),
            None => self.status = format!("No such door: {name}"),
        }
    }

    /// Open a board named directly from the menu (#86). Board access is by
    /// readability, so an unknown or unreadable name reports "not found" rather
    /// than distinguishing the two. Populates the board list too, so Esc from
    /// the message view lands on the matching row.
    async fn open_board_by_name(&mut self, name: &str) {
        match boards::list_readable_boards(&self.pool, &self.user.role).await {
            Ok(list) => {
                self.boards = list;
                match self.boards.iter().position(|b| b.name == name) {
                    Some(idx) => {
                        self.board_sel = idx;
                        let board = self.boards[idx].clone();
                        self.refresh_unread().await;
                        self.open_board(board).await;
                    }
                    None => {
                        self.board_sel = 0;
                        self.status = format!("Board '{name}' not found or not accessible.");
                    }
                }
            }
            Err(e) => self.status = format!("Error loading boards: {e}"),
        }
    }

    // ---- Bulletins -------------------------------------------------------

    /// Load bulletins on startup; if any exist, land the session on the
    /// Bulletins screen (classic "shown after login" behavior).
    pub async fn load_startup_bulletins(&mut self) {
        if let Ok(list) = bulletins::list(&self.pool).await
            && !list.is_empty()
        {
            self.bulletins = list;
            self.bulletin_sel = 0;
            self.screen = Screen::Bulletins;
        }
    }

    async fn open_bulletins(&mut self) {
        match bulletins::list(&self.pool).await {
            Ok(list) => {
                self.bulletins = list;
                self.bulletin_sel = 0;
                self.screen = Screen::Bulletins;
            }
            Err(e) => self.status = format!("Error loading bulletins: {e}"),
        }
    }

    async fn on_bulletins(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Up => self.bulletin_sel = self.bulletin_sel.saturating_sub(1),
            KeyCode::Down => {
                self.bulletin_sel =
                    (self.bulletin_sel + 1).min(self.bulletins.len().saturating_sub(1))
            }
            KeyCode::Enter => {
                if let Some(b) = self.bulletins.get(self.bulletin_sel) {
                    self.current_bulletin = Some(b.clone());
                    self.screen = Screen::ReadBulletin;
                }
            }
            KeyCode::Esc | KeyCode::Left | KeyCode::Char('q') => self.screen = Screen::MainMenu,
            _ => {}
        }
    }

    // ---- Oneliners (graffiti wall) ---------------------------------------

    async fn open_oneliners(&mut self) {
        match oneliners::recent(&self.pool, 100).await {
            Ok(list) => {
                self.oneliners = list;
                self.screen = Screen::Oneliners;
            }
            Err(e) => self.status = format!("Error loading oneliners: {e}"),
        }
    }

    async fn on_oneliners(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('n') => {
                if self.user.is_guest() {
                    self.status = "Guests cannot post — register an account first.".into();
                } else {
                    self.form = Form::new(vec![Field::new("Oneliner", false)]);
                    self.screen = Screen::ComposeOneliner;
                }
            }
            KeyCode::Esc | KeyCode::Left | KeyCode::Char('q') => self.screen = Screen::MainMenu,
            _ => {}
        }
    }

    async fn on_compose_oneliner(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => self.screen = Screen::Oneliners,
            KeyCode::Enter => self.submit_oneliner().await,
            KeyCode::Backspace => self.form.backspace(),
            KeyCode::Char(c) => self.form.insert(c),
            _ => {}
        }
    }

    async fn submit_oneliner(&mut self) {
        let body = self.form.value(0).to_string();
        if body.is_empty() {
            self.status = "Say something first.".into();
            return;
        }
        match oneliners::add(
            &self.pool,
            &self.user,
            &body,
            &self.config.limits,
            &self.config.oneliners,
        )
        .await
        {
            Ok(id) => {
                self.fanout_status(id).await;
                self.open_oneliners().await;
                self.status = "Posted to the wall.".into();
            }
            Err(AppError::OnelinerLength(max)) => {
                self.status = format!("Oneliner must be 1–{max} characters.");
            }
            Err(AppError::RateLimited) => {
                self.status = "You're posting too quickly — please slow down.".into()
            }
            Err(e) => self.status = format!("Could not post: {e}"),
        }
    }

    /// Queue a freshly-posted status for delivery to the author's remote
    /// followers, when federation is on. Best-effort: this only enqueues (a
    /// background task signs and sends), and a failure here must never block the
    /// post from reaching the local wall.
    async fn fanout_status(&self, oneliner_id: i64) {
        let fed = &self.config.federation;
        if !fed.enabled {
            return;
        }
        let origin = match crate::services::federation::Origin::from_config(fed) {
            Ok(o) => o,
            Err(e) => {
                tracing::warn!("federation enabled but origin invalid, not delivering: {e:#}");
                return;
            }
        };
        match crate::services::federation::outbound::deliver_status(
            &self.pool,
            &origin,
            oneliner_id,
        )
        .await
        {
            Ok(0) => {}
            Ok(n) => tracing::info!("queued status {oneliner_id} to {n} follower inbox(es)"),
            Err(e) => tracing::warn!("could not queue status {oneliner_id} for delivery: {e:#}"),
        }
    }

    /// Announce a freshly-posted board message to the board Group's remote
    /// subscribers, when federation is on. Best-effort, like [`Self::fanout_status`].
    async fn fanout_board_post(&self, message_id: i64) {
        let fed = &self.config.federation;
        if !fed.enabled {
            return;
        }
        let origin = match crate::services::federation::Origin::from_config(fed) {
            Ok(o) => o,
            Err(e) => {
                tracing::warn!("federation enabled but origin invalid, not delivering: {e:#}");
                return;
            }
        };
        match crate::services::federation::outbound::deliver_board_post(
            &self.pool, &origin, message_id,
        )
        .await
        {
            Ok(0) => {}
            Ok(n) => tracing::info!("announced post {message_id} to {n} subscriber inbox(es)"),
            Err(e) => tracing::warn!("could not announce post {message_id}: {e:#}"),
        }
    }

    /// Fan an author's edit out to a federated board's subscribers as an
    /// `Announce{Update}` (#156), so their mirror refreshes. No-op when
    /// federation is off or the board has no remote followers.
    async fn fanout_board_update(&self, message_id: i64) {
        let fed = &self.config.federation;
        if !fed.enabled {
            return;
        }
        let origin = match crate::services::federation::Origin::from_config(fed) {
            Ok(o) => o,
            Err(e) => {
                tracing::warn!("federation enabled but origin invalid, not delivering edit: {e:#}");
                return;
            }
        };
        match crate::services::federation::outbound::deliver_board_update(
            &self.pool, &origin, message_id,
        )
        .await
        {
            Ok(0) => {}
            Ok(n) => tracing::info!("announced edit of {message_id} to {n} subscriber inbox(es)"),
            Err(e) => tracing::warn!("could not announce edit of {message_id}: {e:#}"),
        }
    }

    /// Build the `Announce{Delete}` for a board post that is *about* to be
    /// deleted (#133), so subscribers can drop it from their mirrors.
    ///
    /// Split from the dispatch half because the activity has to be built while
    /// the row still exists; the caller queues it only once the delete actually
    /// succeeded. `None` when there's nothing to announce.
    async fn prepare_board_delete(
        &self,
        message_id: i64,
    ) -> Option<crate::services::federation::outbound::Prepared> {
        let fed = &self.config.federation;
        if !fed.enabled {
            return None;
        }
        let origin = crate::services::federation::Origin::from_config(fed).ok()?;
        match crate::services::federation::outbound::prepare_board_delete(
            &self.pool, &origin, message_id,
        )
        .await
        {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!("could not build Delete announcement for {message_id}: {e:#}");
                None
            }
        }
    }

    /// Queue a prepared withdrawal now that the post is gone locally.
    async fn dispatch_board_delete(
        &self,
        prepared: Option<crate::services::federation::outbound::Prepared>,
        message_id: i64,
    ) {
        let Some(p) = prepared else { return };
        match crate::services::federation::outbound::dispatch(&self.pool, &p).await {
            Ok(n) => {
                tracing::info!("announced deletion of {message_id} to {n} subscriber inbox(es)")
            }
            Err(e) => tracing::warn!("could not announce deletion of {message_id}: {e:#}"),
        }
    }

    // ---- Timeline --------------------------------------------------------

    async fn open_timeline(&mut self) {
        match crate::services::federation::timeline::recent(&self.pool, 100).await {
            Ok(list) => {
                self.timeline = list;
                self.timeline_sel = 0;
                self.screen = Screen::Timeline;
            }
            Err(e) => self.status = format!("Error loading timeline: {e}"),
        }
    }

    async fn on_timeline(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                self.timeline_sel = self.timeline_sel.saturating_sub(1);
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if self.timeline_sel + 1 < self.timeline.len() {
                    self.timeline_sel += 1;
                }
            }
            KeyCode::Char('f') => {
                if self.user.is_guest() {
                    self.status = "Guests cannot follow — register an account first.".into();
                } else {
                    self.form = Form::new(vec![Field::new("Follow (user@host)", false)]);
                    self.screen = Screen::FollowRemote;
                }
            }
            KeyCode::Char('r') => self.open_timeline().await,
            KeyCode::Esc | KeyCode::Left | KeyCode::Char('q') => self.screen = Screen::MainMenu,
            _ => {}
        }
    }

    // ---- Remote boards (#132) --------------------------------------------
    //
    // A sibling of the local board screens, not a reuse of them. Mirrored posts
    // live outside `messages` on purpose — they're foreign objects we cache, and
    // sharing the board UI wholesale would blur a line worth keeping visible.

    async fn open_remote_boards(&mut self) {
        match crate::services::federation::mirror::boards(&self.pool).await {
            Ok(list) => {
                self.remote_boards = list;
                self.remote_board_sel = 0;
                self.screen = Screen::RemoteBoards;
            }
            Err(e) => self.status = format!("Error loading remote boards: {e}"),
        }
    }

    async fn on_remote_boards(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                self.remote_board_sel = self.remote_board_sel.saturating_sub(1);
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if self.remote_board_sel + 1 < self.remote_boards.len() {
                    self.remote_board_sel += 1;
                }
            }
            KeyCode::Enter | KeyCode::Right => self.open_remote_board().await,
            KeyCode::Char('r') => self.open_remote_boards().await,
            KeyCode::Esc | KeyCode::Left | KeyCode::Char('q') => self.screen = Screen::MainMenu,
            _ => {}
        }
    }

    async fn open_remote_board(&mut self) {
        let Some(board) = self.remote_boards.get(self.remote_board_sel).cloned() else {
            return;
        };
        match crate::services::federation::mirror::thread(&self.pool, &board.group_uri, 200).await {
            Ok(posts) => {
                let pending = crate::services::federation::remote_posting::pending(
                    &self.pool,
                    &board.group_uri,
                )
                .await
                .unwrap_or_default();
                self.mirror_rows = merge_pending(posts, pending);
                self.mirror_sel = 0;
                self.current_remote_board = Some(board);
                self.screen = Screen::RemoteBoardPosts;
            }
            Err(e) => self.status = format!("Error loading posts: {e}"),
        }
    }

    /// Start a submission to the open remote board (#131). The guards that would
    /// fail at submit are checked here instead, so the user learns why up front
    /// rather than after typing a post.
    /// `reply_to` is the selected post when replying (#139 Slice C), `None` for
    /// a new thread.
    fn begin_compose_remote_post(&mut self, reply_to: Option<&MirrorRow>) {
        if self.user.is_guest() {
            self.status = "Guests cannot post — register an account first.".into();
            return;
        }
        let Some(board) = self.current_remote_board.as_ref() else {
            return;
        };
        if board.state != "accepted" {
            self.status = format!(
                "{} hasn't accepted the subscription yet — nothing can be posted there.",
                board.handle
            );
            return;
        }
        // A reply to a post the board hasn't published yet has nothing stable to
        // point at: our submission's URI only becomes real to *them* once they
        // accept it, so a reply naming it would dangle on their side.
        if reply_to.is_some_and(|r| r.pending) {
            self.status =
                "That post hasn't been published by the board yet — wait for it to appear.".into();
            return;
        }
        let mut subject = Field::new("Subject", false);
        self.remote_reply_to = match reply_to {
            Some(row) => {
                subject.value = reply_subject(&row.subject);
                Some(row.ap_id.clone())
            }
            None => None,
        };
        self.form = Form::new(vec![subject, Field::new("Body", false)]);
        self.screen = Screen::ComposeRemotePost;
    }

    async fn on_compose_remote_post(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => self.screen = Screen::RemoteBoardPosts,
            KeyCode::Enter if self.form.on_last() => self.submit_remote_post().await,
            KeyCode::Enter | KeyCode::Tab | KeyCode::Down => self.form.next_field(),
            KeyCode::BackTab | KeyCode::Up => self.form.prev_field(),
            KeyCode::Backspace => self.form.backspace(),
            KeyCode::Char(c) => self.form.insert(c),
            _ => {}
        }
    }

    async fn submit_remote_post(&mut self) {
        let Some(board) = self.current_remote_board.clone() else {
            return;
        };
        let subject = self.form.value(0).to_string();
        let body = self.form.value(1).to_string();
        if subject.is_empty() || body.is_empty() {
            self.status = "Subject and body are both required.".into();
            return;
        }
        let origin = match crate::services::federation::Origin::from_config(&self.config.federation)
        {
            Ok(o) => o,
            Err(e) => {
                self.status = format!("Federation origin invalid: {e}");
                return;
            }
        };
        match crate::services::federation::remote_posting::submit(
            &self.pool,
            &origin,
            &self.user,
            &board.group_uri,
            &subject,
            &body,
            &self.config.limits,
            self.remote_reply_to.as_deref(),
        )
        .await
        {
            Ok(_) => {
                self.remote_reply_to = None;
                self.status = format!(
                    "Sent to {} — it appears here once the board publishes it.",
                    board.handle
                );
                self.open_remote_board().await;
            }
            Err(e) => self.status = format!("Could not post: {e}"),
        }
    }

    async fn on_remote_board_posts(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                self.mirror_sel = self.mirror_sel.saturating_sub(1);
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if self.mirror_sel + 1 < self.mirror_rows.len() {
                    self.mirror_sel += 1;
                }
            }
            KeyCode::Char('p') => self.begin_compose_remote_post(None),
            KeyCode::Char('r') => {
                let target = self.mirror_rows.get(self.mirror_sel).cloned();
                self.begin_compose_remote_post(target.as_ref());
            }
            KeyCode::Char('R') => self.open_remote_board().await,
            KeyCode::Esc | KeyCode::Left | KeyCode::Char('q') => {
                self.current_remote_board = None;
                self.screen = Screen::RemoteBoards;
            }
            _ => {}
        }
    }

    async fn on_follow_remote(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => self.screen = Screen::Timeline,
            KeyCode::Enter => self.submit_follow().await,
            KeyCode::Backspace => self.form.backspace(),
            KeyCode::Char(c) => self.form.insert(c),
            _ => {}
        }
    }

    async fn submit_follow(&mut self) {
        let handle = self.form.value(0).to_string();
        if handle.is_empty() {
            self.status = "Enter a handle like alice@mastodon.social.".into();
            return;
        }
        self.status = format!("Resolving {handle}…");
        match crate::web::ap_object::follow_handle(
            &self.pool,
            &self.config.federation,
            &self.user,
            &handle,
        )
        .await
        {
            Ok(remote) => {
                self.status = format!("Follow request sent to {remote} (pending until accepted).");
                self.open_timeline().await;
            }
            Err(e) => {
                self.status = format!("Could not follow {handle}: {e}");
                self.screen = Screen::Timeline;
            }
        }
    }

    // ---- Admin -----------------------------------------------------------

    async fn open_admin_users(&mut self) {
        match admin::list_users(&self.pool).await {
            Ok(users) => {
                self.admin_users = users;
                self.admin_user_sel = 0;
                self.screen = Screen::AdminUsers;
            }
            Err(e) => self.status = format!("Error loading users: {e}"),
        }
    }

    async fn on_admin_users(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Up => self.admin_user_sel = self.admin_user_sel.saturating_sub(1),
            KeyCode::Down => {
                self.admin_user_sel =
                    (self.admin_user_sel + 1).min(self.admin_users.len().saturating_sub(1))
            }
            KeyCode::Char('b') => self.admin_ban_selected().await,
            KeyCode::Char('u') => self.admin_unban_selected().await,
            KeyCode::Char('l') => self.open_admin_logins().await,
            KeyCode::Char('a') => self.open_admin_audit().await,
            KeyCode::Char('f') => self.open_admin_federation().await,
            KeyCode::Char('w') => {
                // Broadcast to everyone (#69) — "wall". Reuse the single-field
                // compose form.
                self.form = Form::new(vec![Field::new("Broadcast", false)]);
                self.screen = Screen::ComposeBroadcast;
            }
            KeyCode::Esc | KeyCode::Left | KeyCode::Char('q') => self.screen = Screen::MainMenu,
            _ => {}
        }
    }

    async fn on_compose_broadcast(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => self.screen = Screen::AdminUsers,
            KeyCode::Enter => self.submit_broadcast().await,
            KeyCode::Backspace => self.form.backspace(),
            KeyCode::Char(c) => self.form.insert(c),
            _ => {}
        }
    }

    /// Fan a sysop broadcast out to every live session immediately (#69). An
    /// in-BBS admin has the live presence registry to hand, so — like an in-BBS
    /// ban kicking at once — it delivers directly rather than via the durable
    /// queue the `bbsctl` path uses.
    async fn submit_broadcast(&mut self) {
        let text = self.form.value(0).trim().to_string();
        if text.is_empty() {
            self.status = "Nothing to broadcast.".into();
            return;
        }
        audit::log(
            &self.pool,
            &self.user.username,
            "broadcast",
            "all sessions",
            Some(&text),
        )
        .await;
        let n = self.presence.broadcast(Event::Broadcast { text }).await;
        // `n` includes this admin's own session, which also sees the toast.
        self.status = format!("Broadcast reached {n} session(s).");
        self.screen = Screen::AdminUsers;
    }

    async fn admin_ban_selected(&mut self) {
        let Some(target) = self.admin_users.get(self.admin_user_sel).cloned() else {
            return;
        };
        // Guardrails: never ban the shared guest account (that would lock out
        // read-only access for everyone) or your own account.
        if target.is_guest() {
            self.status = "Cannot ban the guest account.".into();
            return;
        }
        if target.id == self.user.id {
            self.status = "You cannot ban yourself.".into();
            return;
        }
        match admin::ban_user(&self.pool, &target.username).await {
            Ok(()) => {
                // Kick the banned user's live sessions immediately.
                let users = HashSet::from([target.username.clone()]);
                self.presence.kick(&users, &HashSet::new()).await;
                audit::log(
                    &self.pool,
                    &self.user.username,
                    "ban_user",
                    &target.username,
                    None,
                )
                .await;
                self.status = format!("Banned {}.", target.username);
                self.reload_admin_users().await;
            }
            Err(e) => self.status = format!("Could not ban: {e}"),
        }
    }

    async fn admin_unban_selected(&mut self) {
        let Some(target) = self.admin_users.get(self.admin_user_sel).cloned() else {
            return;
        };
        match admin::unban_user(&self.pool, &target.username).await {
            Ok(()) => {
                audit::log(
                    &self.pool,
                    &self.user.username,
                    "unban_user",
                    &target.username,
                    None,
                )
                .await;
                self.status = format!("Unbanned {}.", target.username);
                self.reload_admin_users().await;
            }
            Err(e) => self.status = format!("Could not unban: {e}"),
        }
    }

    async fn reload_admin_users(&mut self) {
        if let Ok(users) = admin::list_users(&self.pool).await {
            self.admin_user_sel = self.admin_user_sel.min(users.len().saturating_sub(1));
            self.admin_users = users;
        }
    }

    async fn open_admin_logins(&mut self) {
        match admin::recent_logins(&self.pool, None, 100).await {
            Ok(logins) => {
                self.admin_logins = logins;
                self.admin_login_sel = 0;
                self.screen = Screen::AdminLogins;
            }
            Err(e) => self.status = format!("Error loading logins: {e}"),
        }
    }

    async fn on_admin_logins(&mut self, key: KeyEvent) {
        // Move by a screenful when paging; the stateful list auto-scrolls to
        // keep the cursor visible.
        const PAGE: usize = 10;
        let last = self.admin_logins.len().saturating_sub(1);
        match key.code {
            KeyCode::Up => self.admin_login_sel = self.admin_login_sel.saturating_sub(1),
            KeyCode::Down => self.admin_login_sel = (self.admin_login_sel + 1).min(last),
            KeyCode::PageUp => self.admin_login_sel = self.admin_login_sel.saturating_sub(PAGE),
            KeyCode::PageDown => self.admin_login_sel = (self.admin_login_sel + PAGE).min(last),
            KeyCode::Home => self.admin_login_sel = 0,
            KeyCode::End => self.admin_login_sel = last,
            KeyCode::Esc | KeyCode::Left | KeyCode::Char('q') => self.screen = Screen::AdminUsers,
            _ => {}
        }
    }

    async fn open_admin_audit(&mut self) {
        match audit::recent(&self.pool, 200).await {
            Ok(entries) => {
                self.admin_audit = entries;
                self.admin_audit_sel = 0;
                self.screen = Screen::AdminAudit;
            }
            Err(e) => self.status = format!("Error loading audit log: {e}"),
        }
    }

    async fn on_admin_audit(&mut self, key: KeyEvent) {
        const PAGE: usize = 10;
        let last = self.admin_audit.len().saturating_sub(1);
        match key.code {
            KeyCode::Up => self.admin_audit_sel = self.admin_audit_sel.saturating_sub(1),
            KeyCode::Down => self.admin_audit_sel = (self.admin_audit_sel + 1).min(last),
            KeyCode::PageUp => self.admin_audit_sel = self.admin_audit_sel.saturating_sub(PAGE),
            KeyCode::PageDown => self.admin_audit_sel = (self.admin_audit_sel + PAGE).min(last),
            KeyCode::Home => self.admin_audit_sel = 0,
            KeyCode::End => self.admin_audit_sel = last,
            KeyCode::Esc | KeyCode::Left | KeyCode::Char('q') => self.screen = Screen::AdminUsers,
            _ => {}
        }
    }

    // ---- Federation domain policy (#159) ---------------------------------

    async fn open_admin_federation(&mut self) {
        use crate::services::federation::policy;
        let mut rows = Vec::new();
        for kind in ["allow", "block"] {
            match policy::list(&self.pool, kind).await {
                Ok(entries) => {
                    for (domain, reason, severity) in entries {
                        rows.push((kind.to_string(), domain, reason, severity));
                    }
                }
                Err(e) => {
                    self.status = format!("Error loading federation policy: {e}");
                    return;
                }
            }
        }
        self.fed_policy = rows;
        self.fed_sel = 0;
        self.screen = Screen::AdminFederation;
    }

    async fn reload_admin_federation(&mut self) {
        let keep = self.fed_sel;
        self.open_admin_federation().await;
        self.fed_sel = keep.min(self.fed_policy.len().saturating_sub(1));
    }

    async fn on_admin_federation(&mut self, key: KeyEvent) {
        let last = self.fed_policy.len().saturating_sub(1);
        match key.code {
            KeyCode::Up => self.fed_sel = self.fed_sel.saturating_sub(1),
            KeyCode::Down => self.fed_sel = (self.fed_sel + 1).min(last),
            // Add an entry — compose the domain, then apply the chosen action.
            KeyCode::Char('a') => self.begin_fed_entry("allow", "suspend"),
            KeyCode::Char('b') => self.begin_fed_entry("block", "suspend"),
            KeyCode::Char('s') => self.begin_fed_entry("block", "silence"),
            KeyCode::Char('d') => self.remove_fed_entry().await,
            KeyCode::Esc | KeyCode::Left | KeyCode::Char('q') => self.screen = Screen::AdminUsers,
            _ => {}
        }
    }

    fn begin_fed_entry(&mut self, kind: &'static str, severity: &'static str) {
        self.form = Form::new(vec![Field::new("Domain", false)]);
        self.fed_pending = Some((kind, severity));
        self.screen = Screen::ComposeFederation;
    }

    async fn on_compose_federation(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => {
                self.fed_pending = None;
                self.screen = Screen::AdminFederation;
            }
            KeyCode::Enter => self.submit_fed_entry().await,
            KeyCode::Backspace => self.form.backspace(),
            KeyCode::Char(c) => self.form.insert(c),
            _ => {}
        }
    }

    async fn submit_fed_entry(&mut self) {
        use crate::services::federation::policy;
        let Some((kind, severity)) = self.fed_pending.take() else {
            self.screen = Screen::AdminFederation;
            return;
        };
        let domain = self.form.value(0).trim().to_ascii_lowercase();
        if domain.is_empty() {
            self.status = "Enter a domain first.".into();
            self.fed_pending = Some((kind, severity)); // keep composing
            return;
        }
        match policy::set(&self.pool, &domain, kind, "", severity).await {
            Ok(()) => {
                let action = if kind == "allow" {
                    "fed_allow"
                } else {
                    "fed_block"
                };
                let detail = (kind == "block").then_some(severity);
                audit::log(&self.pool, &self.user.username, action, &domain, detail).await;
                self.status = if kind == "allow" {
                    format!("Allowing {domain}.")
                } else {
                    format!("Blocking {domain} ({severity}).")
                };
                self.reload_admin_federation().await;
            }
            Err(e) => self.status = format!("Could not update policy: {e}"),
        }
        self.screen = Screen::AdminFederation;
    }

    async fn remove_fed_entry(&mut self) {
        use crate::services::federation::policy;
        let Some((kind, domain, _, _)) = self.fed_policy.get(self.fed_sel).cloned() else {
            return;
        };
        match policy::unset(&self.pool, &domain, &kind).await {
            Ok(_) => {
                audit::log(
                    &self.pool,
                    &self.user.username,
                    "fed_remove",
                    &domain,
                    Some(&kind),
                )
                .await;
                self.status = format!("Removed {kind} for {domain}.");
                self.reload_admin_federation().await;
            }
            Err(e) => self.status = format!("Could not remove: {e}"),
        }
    }

    // ---- Boards ----------------------------------------------------------

    async fn open_boards(&mut self) {
        match boards::list_readable_boards(&self.pool, &self.user.role).await {
            Ok(b) => {
                self.boards = b;
                self.board_sel = 0;
                self.refresh_unread().await;
                self.screen = Screen::BoardList;
            }
            Err(e) => self.status = format!("Error loading boards: {e}"),
        }
    }

    /// Reload the board list in place (after a moderation change), keeping the
    /// selection in range.
    async fn reload_boards(&mut self) {
        if let Ok(b) = boards::list_readable_boards(&self.pool, &self.user.role).await {
            self.board_sel = self.board_sel.min(b.len().saturating_sub(1));
            self.boards = b;
        }
    }

    /// Refresh per-board unread counts ("new since last call") and the unread
    /// private-mail count. Guests share a single account with no meaningful
    /// watermark and can't receive mail, so they get no counts.
    async fn refresh_unread(&mut self) {
        if self.user.is_guest() {
            self.board_unread.clear();
            self.mail_unread = 0;
            return;
        }
        match boards::unread_counts(&self.pool, self.user.id).await {
            Ok(counts) => self.board_unread = counts,
            Err(e) => self.status = format!("Error loading unread: {e}"),
        }
        if self.config.features.private_mail {
            match mail::unread_count(&self.pool, self.user.id).await {
                Ok(n) => self.mail_unread = n,
                Err(e) => self.status = format!("Error loading mail count: {e}"),
            }
        } else {
            self.mail_unread = 0;
        }
    }

    async fn on_board_list(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Up => self.board_sel = self.board_sel.saturating_sub(1),
            KeyCode::Down => {
                self.board_sel = (self.board_sel + 1).min(self.boards.len().saturating_sub(1))
            }
            KeyCode::Enter => {
                if let Some(board) = self.boards.get(self.board_sel).cloned() {
                    self.open_board(board).await;
                }
            }
            // Admins can lock/unlock the selected board in place.
            KeyCode::Char('l') if self.user.is_admin() => self.toggle_board_lock().await,
            KeyCode::Esc | KeyCode::Left | KeyCode::Char('q') => self.screen = Screen::MainMenu,
            _ => {}
        }
    }

    async fn toggle_board_lock(&mut self) {
        let Some(board) = self.boards.get(self.board_sel).cloned() else {
            return;
        };
        let now_locked = !board.locked;
        match boards::set_locked(&self.pool, board.id, now_locked).await {
            Ok(()) => {
                audit::log(
                    &self.pool,
                    &self.user.username,
                    if now_locked {
                        "lock_board"
                    } else {
                        "unlock_board"
                    },
                    &board.name,
                    None,
                )
                .await;
                self.reload_boards().await;
                self.status = format!(
                    "{} {}.",
                    if now_locked { "Locked" } else { "Unlocked" },
                    board.name
                );
            }
            Err(e) => self.status = format!("Could not update board: {e}"),
        }
    }

    async fn open_board(&mut self, board: Board) {
        if self.load_board(board).await {
            self.msg_sel = 0;
            self.screen = Screen::MessageList;
        }
    }

    /// Load a board's threaded messages into state and advance its unread
    /// watermark, without changing the screen or selection. Returns whether the
    /// load succeeded. Shared by [`Self::open_board`] and search-result jumps.
    async fn load_board(&mut self, board: Board) -> bool {
        match boards::list_thread(&self.pool, board.id).await {
            Ok(m) => {
                // Capture the pre-visit watermark so this session's render can
                // highlight what's new, then advance it so the board list's
                // unread count clears on return. Guests don't track state.
                if self.user.is_guest() {
                    self.msg_seen_threshold = i64::MAX;
                } else {
                    self.msg_seen_threshold = boards::last_seen(&self.pool, self.user.id, board.id)
                        .await
                        .unwrap_or(0);
                    if let Err(e) =
                        boards::mark_board_seen(&self.pool, self.user.id, board.id, now_unix())
                            .await
                    {
                        self.status = format!("Error marking board seen: {e}");
                    }
                    self.board_unread.remove(&board.id);
                }
                self.messages = self.hide_blocked(m).await;
                self.current_board = Some(board);
                true
            }
            Err(e) => {
                self.status = format!("Error loading messages: {e}");
                false
            }
        }
    }

    /// Drop posts authored by users this reader has blocked (#97). Admins can't
    /// be blocked, so moderation/pinned notices always remain visible. Applied
    /// on every path that loads a board's messages, so a block takes effect
    /// immediately — on open and on reload.
    async fn hide_blocked(&self, mut m: Vec<boards::ThreadItem>) -> Vec<boards::ThreadItem> {
        if let Ok(blocked) = blocks::blocked_ids(&self.pool, self.user.id).await
            && !blocked.is_empty()
        {
            m.retain(|item| !blocked.contains(&item.message.author_id));
        }
        m
    }

    /// Test hook: reload the current board's message list, exercising the
    /// block/ignore filter (#97) on the exact code path the app uses.
    #[doc(hidden)]
    pub async fn reload_messages_for_test(&mut self) {
        self.reload_messages().await;
    }

    /// Reload the current board's messages in place (after posting or a
    /// moderation change), keeping the selection in range.
    async fn reload_messages(&mut self) {
        let Some(board_id) = self.current_board.as_ref().map(|b| b.id) else {
            return;
        };
        if let Ok(m) = boards::list_thread(&self.pool, board_id).await {
            let m = self.hide_blocked(m).await;
            self.msg_sel = self.msg_sel.min(m.len().saturating_sub(1));
            self.messages = m;
        }
    }

    // ---- Messages --------------------------------------------------------

    async fn on_message_list(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Up => self.msg_sel = self.msg_sel.saturating_sub(1),
            KeyCode::Down => {
                self.msg_sel = (self.msg_sel + 1).min(self.messages.len().saturating_sub(1))
            }
            KeyCode::Enter => {
                if let Some(item) = self.messages.get(self.msg_sel) {
                    match boards::get_message(&self.pool, item.message.id).await {
                        Ok(full) => {
                            self.current_msg_signature =
                                profiles::signature_of(&self.pool, full.author_id)
                                    .await
                                    .unwrap_or_default();
                            self.current_message = Some(full);
                            self.screen = Screen::ReadMessage;
                        }
                        Err(e) => self.status = format!("Error: {e}"),
                    }
                }
            }
            KeyCode::Char('n') => self.begin_compose_post(None),
            KeyCode::Char('r') => self.begin_reply(),
            // Edit / delete your own post (#92); admins can delete any, and pin.
            KeyCode::Char('e') => {
                if let Some(m) = self.messages.get(self.msg_sel).map(|i| i.message.clone())
                    && self.can_edit(&m)
                {
                    self.begin_edit_post(&m);
                }
            }
            KeyCode::Char('d') => {
                if let Some(m) = self.messages.get(self.msg_sel).map(|i| i.message.clone())
                    && self.can_delete(&m)
                {
                    self.delete_post(m.id, &m.subject, false).await;
                }
            }
            KeyCode::Char('p') if self.user.is_admin() => self.toggle_pin_selected().await,
            KeyCode::Esc | KeyCode::Left | KeyCode::Char('q') => {
                // Recompute unread so this board's count clears (and any posts
                // that arrived while reading are reflected) on the board list.
                self.refresh_unread().await;
                self.screen = Screen::BoardList;
            }
            _ => {}
        }
    }

    /// The read-a-message screen: reply, edit or delete your own post (#92),
    /// moderate as an admin, or go back.
    async fn on_read_message(&mut self, key: KeyEvent) {
        let Some(m) = self.current_message.clone() else {
            if matches!(
                key.code,
                KeyCode::Esc | KeyCode::Left | KeyCode::Char('q') | KeyCode::Enter
            ) {
                self.screen = Screen::MessageList;
            }
            return;
        };
        match key.code {
            KeyCode::Char('r') => {
                self.begin_compose_post(Some((m.id, reply_subject(&m.subject))));
            }
            KeyCode::Char('e') if self.can_edit(&m) => self.begin_edit_post(&m),
            KeyCode::Char('d') if self.can_delete(&m) => {
                self.delete_post(m.id, &m.subject, true).await;
            }
            KeyCode::Esc | KeyCode::Left | KeyCode::Char('q') | KeyCode::Enter => {
                self.screen = Screen::MessageList
            }
            _ => {}
        }
    }

    /// Whether the composer is editing an existing post rather than creating one
    /// (#92), for the UI to title the screen.
    pub fn is_editing_post(&self) -> bool {
        self.edit_target.is_some()
    }

    /// The user currently being paged, for the page composer's title (#68).
    pub fn page_target(&self) -> Option<&str> {
        self.page_target.as_deref()
    }

    /// Whether the current user may edit this post: its author, on an unlocked
    /// board. (Admins moderate via delete/pin, not by editing others' text.)
    fn can_edit(&self, m: &Message) -> bool {
        m.author_id == self.user.id && !self.current_board.as_ref().is_some_and(|b| b.locked)
    }

    /// Whether the current user may delete this post: an admin (any post), or
    /// the author on an unlocked board.
    fn can_delete(&self, m: &Message) -> bool {
        self.user.is_admin()
            || (m.author_id == self.user.id
                && !self.current_board.as_ref().is_some_and(|b| b.locked))
    }

    /// Open the composer to edit an existing post, prefilled with its text.
    fn begin_edit_post(&mut self, m: &Message) {
        let mut subject = Field::new("Subject", false);
        subject.value = m.subject.clone();
        self.form = Form::new(vec![subject]);
        self.body = crate::app::textarea::TextArea::from_text(&m.body);
        self.body_focused = false;
        self.reply_parent = None;
        self.edit_target = Some(m.id);
        self.screen = Screen::ComposePost;
    }

    /// Delete a post: the admin path is unscoped; a non-admin can only delete
    /// their own, on an unlocked board (`delete_own_message` enforces both). The
    /// federation withdrawal is built before the delete and dispatched after, so
    /// subscribers are told to drop it only once it's actually gone (#133).
    async fn delete_post(&mut self, id: i64, subject: &str, from_reader: bool) {
        let pending = self.prepare_board_delete(id).await;
        let is_mod = self.user.is_admin();
        let removed = if is_mod {
            boards::delete_message(&self.pool, id).await
        } else {
            boards::delete_own_message(&self.pool, id, &self.user).await
        };
        match removed {
            Ok(true) => {
                // Audit only moderation deletes — an author removing their own
                // post isn't an operator action.
                if is_mod {
                    audit::log(
                        &self.pool,
                        &self.user.username,
                        "delete_post",
                        subject,
                        Some(&format!("post #{id}")),
                    )
                    .await;
                }
                self.dispatch_board_delete(pending, id).await;
                self.reload_messages().await;
                if from_reader {
                    self.screen = Screen::MessageList;
                }
                self.status = format!("Deleted post '{}'.", truncate_status(subject));
            }
            Ok(false) => {
                self.status = if self.current_board.as_ref().is_some_and(|b| b.locked) {
                    "This board is locked.".into()
                } else {
                    "Post already gone.".into()
                };
            }
            Err(e) => self.status = format!("Could not delete: {e}"),
        }
    }

    /// Reply to the selected message (from the list): pre-fill an `Re:` subject
    /// and remember the parent so `submit_post` files it under that message.
    fn begin_reply(&mut self) {
        let Some(item) = self.messages.get(self.msg_sel) else {
            return;
        };
        let (parent_id, subject) = (item.message.id, item.message.subject.clone());
        self.begin_compose_post(Some((parent_id, reply_subject(&subject))));
    }

    /// Start composing a post (or a reply when `reply` is set), explaining up
    /// front why it's not allowed rather than failing only at submit.
    fn begin_compose_post(&mut self, reply: Option<(i64, String)>) {
        let Some(board) = self.current_board.as_ref() else {
            return;
        };
        if self.user.is_guest() {
            self.status = "Guests cannot post — register an account first.".into();
            return;
        }
        if board.locked && !self.user.is_admin() {
            self.status = "This board is locked.".into();
            return;
        }
        if !board.can_write(&self.user.role) {
            self.status = "You don't have permission to post to this board.".into();
            return;
        }
        let mut subject = Field::new("Subject", false);
        self.reply_parent = match reply {
            Some((pid, re_subject)) => {
                subject.value = re_subject;
                Some(pid)
            }
            None => None,
        };
        self.form = Form::new(vec![subject]);
        self.body = crate::app::textarea::TextArea::new();
        self.body_focused = false;
        self.screen = Screen::ComposePost;
    }

    async fn toggle_pin_selected(&mut self) {
        let Some(item) = self.messages.get(self.msg_sel).cloned() else {
            return;
        };
        let msg = item.message;
        let pin = !msg.pinned;
        match boards::set_pinned(&self.pool, msg.id, pin).await {
            Ok(()) => {
                audit::log(
                    &self.pool,
                    &self.user.username,
                    if pin { "pin_post" } else { "unpin_post" },
                    &msg.subject,
                    Some(&format!("post #{}", msg.id)),
                )
                .await;
                self.reload_messages().await;
                self.status = if pin { "Pinned." } else { "Unpinned." }.into();
            }
            Err(e) => self.status = format!("Could not update post: {e}"),
        }
    }

    /// Shared editing for the compose screens (#96): a single-line header form
    /// plus the multi-line [`TextArea`] body, with focus moving between them.
    /// Returns `true` when the send key (Ctrl-D) was pressed, so the caller can
    /// run its own submit.
    fn edit_compose(&mut self, key: KeyEvent) -> bool {
        // Ctrl-D sends from either focus — Enter can't, since in the body it
        // inserts a newline.
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('d') {
            return true;
        }
        let max = self.config.limits.max_body_chars;
        if self.body_focused {
            match key.code {
                // Leaving the top of the body steps back to the last header field.
                KeyCode::Up if self.body.cursor().0 == 0 => {
                    self.body_focused = false;
                    self.form.focus = self.form.fields.len().saturating_sub(1);
                }
                KeyCode::Enter => {
                    if max == 0 || self.body.char_count() < max {
                        self.body.insert_newline();
                    }
                }
                KeyCode::Char(c) => {
                    // Stop at the configured body limit rather than accept text
                    // the post would be rejected for at submit.
                    if max == 0 || self.body.char_count() < max {
                        self.body.insert_char(c);
                    } else {
                        self.status = format!("Body limit reached ({max} characters).");
                    }
                }
                KeyCode::Backspace => self.body.backspace(),
                KeyCode::Delete => self.body.delete(),
                KeyCode::Left => self.body.left(),
                KeyCode::Right => self.body.right(),
                KeyCode::Up => self.body.up(),
                KeyCode::Down => self.body.down(),
                KeyCode::Home => self.body.home(),
                KeyCode::End => self.body.end(),
                _ => {}
            }
        } else {
            match key.code {
                // Tab / Enter / Down off the last header field drops into the body.
                KeyCode::Enter | KeyCode::Tab | KeyCode::Down => {
                    if self.form.on_last() {
                        self.body_focused = true;
                    } else {
                        self.form.next_field();
                    }
                }
                KeyCode::Up => self.form.prev_field(),
                KeyCode::Backspace => self.form.backspace(),
                KeyCode::Char(c) => self.form.insert(c),
                _ => {}
            }
        }
        false
    }

    async fn on_compose_post(&mut self, key: KeyEvent) {
        if key.code == KeyCode::Esc {
            self.edit_target = None; // cancel an in-progress edit too
            self.screen = Screen::MessageList;
            return;
        }
        if self.edit_compose(key) {
            self.submit_post().await;
        }
    }

    async fn submit_post(&mut self) {
        let subject = self.form.value(0).to_string();
        let body = self.body.text().trim().to_string();
        if subject.is_empty() {
            self.status = "Subject cannot be empty.".into();
            return;
        }
        // An edit reuses this screen but goes through the author-scoped update
        // instead of creating a new post (#92).
        if let Some(id) = self.edit_target {
            self.submit_edit(id, &subject, &body).await;
            return;
        }
        let Some(board_id) = self.current_board.as_ref().map(|b| b.id) else {
            self.status = "No board selected.".into();
            return;
        };
        let parent_id = self.reply_parent;
        match boards::post_message(
            &self.pool,
            board_id,
            &self.user,
            &subject,
            &body,
            parent_id,
            &self.config.limits,
        )
        .await
        {
            Ok(id) => {
                self.reply_parent = None;
                self.fanout_board_post(id).await;
                self.reload_messages().await;
                self.msg_sel = 0;
                self.screen = Screen::MessageList;
                self.status = if parent_id.is_some() {
                    "Reply posted.".into()
                } else {
                    "Message posted.".into()
                };
            }
            Err(AppError::BoardLocked) => self.status = "This board is locked.".into(),
            Err(AppError::BoardWriteDenied) => {
                self.status = "You don't have permission to post to this board.".into()
            }
            Err(AppError::RateLimited) => {
                self.status = "You're posting too quickly — please slow down.".into()
            }
            Err(e) => self.status = format!("Could not post: {e}"),
        }
    }

    /// Apply an author's edit (#92). Returns to the board on success; a `false`
    /// result means the post is no longer the user's to edit (locked board, or
    /// deleted underneath them), which the message explains.
    async fn submit_edit(&mut self, id: i64, subject: &str, body: &str) {
        match boards::edit_own_message(
            &self.pool,
            id,
            &self.user,
            subject,
            body,
            &self.config.limits,
        )
        .await
        {
            Ok(true) => {
                self.edit_target = None;
                // Propagate the edit to a federated board's subscribers (#156),
                // the Update counterpart to `submit_post`'s Create fan-out.
                self.fanout_board_update(id).await;
                self.reload_messages().await;
                // Keep the reader in sync if it's still showing this post.
                if let Ok(fresh) = boards::get_message(&self.pool, id).await {
                    self.current_message = Some(fresh);
                }
                self.screen = Screen::MessageList;
                self.status = "Post edited.".into();
            }
            Ok(false) => {
                self.edit_target = None;
                self.status = if self.current_board.as_ref().is_some_and(|b| b.locked) {
                    "This board is locked.".into()
                } else {
                    "That post can no longer be edited.".into()
                };
                self.screen = Screen::MessageList;
            }
            Err(AppError::FieldTooLong(field, max)) => {
                self.status = format!("{field} is too long (max {max}) — shorten it.")
            }
            Err(e) => self.status = format!("Could not edit: {e}"),
        }
    }

    // ---- Mail ------------------------------------------------------------

    async fn open_mailbox(&mut self) {
        match mail::inbox(&self.pool, self.user.id).await {
            Ok(m) => {
                self.mails = m;
                self.mail_sel = 0;
                self.screen = Screen::Mailbox;
            }
            Err(e) => self.status = format!("Error loading mail: {e}"),
        }
    }

    async fn on_mailbox(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Up => self.mail_sel = self.mail_sel.saturating_sub(1),
            KeyCode::Down => {
                self.mail_sel = (self.mail_sel + 1).min(self.mails.len().saturating_sub(1))
            }
            KeyCode::Enter => {
                if let Some(m) = self.mails.get(self.mail_sel) {
                    match mail::read_mail(&self.pool, m.id, self.user.id).await {
                        Ok(full) => {
                            self.current_mail = Some(full);
                            self.screen = Screen::ReadMail;
                        }
                        Err(e) => self.status = format!("Error: {e}"),
                    }
                }
            }
            KeyCode::Char('n') => self.begin_compose_mail(None),
            KeyCode::Char('/') => self.begin_mail_search(),
            // Delete the selected message from the list, with a confirm.
            KeyCode::Char('d') => {
                if let Some(m) = self.mails.get(self.mail_sel).cloned() {
                    self.current_mail = Some(m);
                    self.mail_delete_return = Screen::Mailbox;
                    self.screen = Screen::ConfirmDeleteMail;
                }
            }
            KeyCode::Esc | KeyCode::Left | KeyCode::Char('q') => {
                // Reading a message cleared its `read_at`; refresh so the
                // main-menu badge reflects what's left.
                self.refresh_unread().await;
                self.screen = Screen::MainMenu;
            }
            _ => {}
        }
    }

    // ---- Mail full-text search (#93) -------------------------------------

    fn begin_mail_search(&mut self) {
        self.form = Form::new(vec![Field::new("Search mail", false)]);
        self.screen = Screen::MailSearchInput;
    }

    async fn on_mail_search_input(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => self.screen = Screen::Mailbox,
            KeyCode::Enter => self.submit_mail_search().await,
            KeyCode::Backspace => self.form.backspace(),
            KeyCode::Char(c) => self.form.insert(c),
            _ => {}
        }
    }

    async fn submit_mail_search(&mut self) {
        let query = self.form.value(0).to_string();
        if query.trim().is_empty() {
            self.status = "Enter a search term.".into();
            return;
        }
        match search::search_mail(&self.pool, self.user.id, &query, search::SEARCH_LIMIT).await {
            Ok(hits) => {
                if hits.is_empty() {
                    self.status = format!("No mail matches \"{query}\".");
                }
                self.mail_search = hits;
                self.mail_search_sel = 0;
                self.mail_search_query = query;
                self.screen = Screen::MailSearchResults;
            }
            Err(e) => self.status = format!("Search failed: {e}"),
        }
    }

    async fn on_mail_search_results(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Up => self.mail_search_sel = self.mail_search_sel.saturating_sub(1),
            KeyCode::Down => {
                self.mail_search_sel =
                    (self.mail_search_sel + 1).min(self.mail_search.len().saturating_sub(1))
            }
            KeyCode::Enter => {
                if let Some(m) = self.mail_search.get(self.mail_search_sel) {
                    match mail::read_mail(&self.pool, m.id, self.user.id).await {
                        Ok(full) => {
                            self.current_mail = Some(full);
                            // Returning from the reader goes to the mailbox; land
                            // there rather than back in a now-stale result list.
                            self.screen = Screen::ReadMail;
                        }
                        Err(e) => self.status = format!("Error: {e}"),
                    }
                }
            }
            // Refine: back to the input, prefilled with the last query.
            KeyCode::Char('/') => {
                self.form = Form::new(vec![Field::new("Search mail", false)]);
                self.form.fields[0].value = self.mail_search_query.clone();
                self.screen = Screen::MailSearchInput;
            }
            KeyCode::Esc | KeyCode::Left | KeyCode::Char('q') => self.screen = Screen::Mailbox,
            _ => {}
        }
    }

    /// Reading a single message (#70): reply, forward, delete, or go back.
    async fn on_read_mail(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('r') => self.begin_reply_mail(),
            KeyCode::Char('f') => self.begin_forward_mail(),
            KeyCode::Char('d') => {
                if self.current_mail.is_some() {
                    self.mail_delete_return = Screen::Mailbox;
                    self.screen = Screen::ConfirmDeleteMail;
                }
            }
            KeyCode::Esc | KeyCode::Left | KeyCode::Char('q') | KeyCode::Enter => {
                self.screen = Screen::Mailbox;
            }
            _ => {}
        }
    }

    /// Deleting mail is irreversible and the recipient's only copy, so it asks.
    async fn on_confirm_delete_mail(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('y') => {
                let Some(m) = self.current_mail.clone() else {
                    self.screen = self.mail_delete_return;
                    return;
                };
                match mail::delete_mail(&self.pool, m.id, self.user.id).await {
                    Ok(true) => {
                        self.current_mail = None;
                        self.refresh_unread().await;
                        self.open_mailbox().await; // reload the list and land there
                        self.status = "Message deleted.".into();
                    }
                    Ok(false) => {
                        self.screen = self.mail_delete_return;
                        self.status = "Message already gone.".into();
                    }
                    Err(e) => {
                        self.screen = self.mail_delete_return;
                        self.status = format!("Could not delete: {e}");
                    }
                }
            }
            _ => {
                self.screen = self.mail_delete_return;
                self.status = "Kept.".into();
            }
        }
    }

    /// Open the mail composer, optionally prefilled (reply/forward, #70).
    /// `prefill` is `(to, subject, body)`; a blank field is left for the user.
    fn begin_compose_mail(&mut self, prefill: Option<(String, String, String)>) {
        let (to, subject, body) = prefill.unwrap_or_default();
        let mut to_field = Field::new("To (username)", false);
        to_field.value = to;
        let mut subject_field = Field::new("Subject", false);
        subject_field.value = subject;
        self.form = Form::new(vec![to_field, subject_field]);
        self.body = crate::app::textarea::TextArea::from_text(&body);
        // Start on whichever field still needs input: the recipient for a
        // forward (blank To), the body for a reply (To and subject filled).
        self.body_focused = !self.form.value(0).is_empty() && !self.form.value(1).is_empty();
        self.screen = Screen::ComposeMail;
    }

    fn begin_reply_mail(&mut self) {
        let Some(m) = self.current_mail.clone() else {
            return;
        };
        self.begin_compose_mail(Some(mail::reply_prefill(&m)));
    }

    fn begin_forward_mail(&mut self) {
        let Some(m) = self.current_mail.clone() else {
            return;
        };
        let (subject, body) = mail::forward_prefill(&m);
        self.begin_compose_mail(Some((String::new(), subject, body)));
    }

    async fn on_compose_mail(&mut self, key: KeyEvent) {
        if key.code == KeyCode::Esc {
            self.screen = Screen::Mailbox;
            return;
        }
        if self.edit_compose(key) {
            self.submit_mail().await;
        }
    }

    async fn submit_mail(&mut self) {
        let to = self.form.value(0).to_string();
        let subject = self.form.value(1).to_string();
        let body = self.body.text().trim().to_string();
        if to.is_empty() || subject.is_empty() {
            self.status = "Recipient and subject are required.".into();
            return;
        }
        // A `@` in the recipient means a remote fediverse account — local
        // usernames can't contain one. Route it to the (opt-in, non-private)
        // remote-DM path instead of the local mailbox.
        if to.contains('@') {
            self.submit_remote_mail(&to, &subject, &body).await;
            return;
        }
        match mail::send_mail(
            &self.pool,
            &self.user,
            &to,
            &subject,
            &body,
            &self.config.limits,
        )
        .await
        {
            Ok(()) => {
                self.open_mailbox().await;
                self.status = format!("Mail sent to {to}.");
            }
            Err(AppError::RecipientNotFound) => {
                self.status = format!("No such user: {to}");
            }
            Err(AppError::RateLimited) => {
                self.status = "You're sending mail too quickly — please slow down.".into()
            }
            Err(e) => self.status = format!("Could not send: {e}"),
        }
    }

    /// Send a private message to a remote fediverse account (opt-in, not
    /// private). Resolving the handle hits the network, so this is its own path.
    async fn submit_remote_mail(&mut self, to: &str, subject: &str, body: &str) {
        self.status = format!("Sending to {to} over the fediverse…");
        match crate::web::ap_object::send_remote_dm(
            &self.pool,
            &self.config.federation,
            &self.user,
            to,
            subject,
            body,
            &self.config.limits,
        )
        .await
        {
            Ok(handle) => {
                self.open_mailbox().await;
                self.status = format!("Sent to {handle} — left the BBS, and was NOT private.");
            }
            Err(e) => {
                self.status = format!("Could not send to {to}: {e}");
                self.screen = Screen::Mailbox;
            }
        }
    }

    // ---- Who's online ----------------------------------------------------

    async fn open_who(&mut self) {
        self.online = self.presence.list().await;
        self.who_sel = 0;
        self.screen = Screen::WhoOnline;
    }

    async fn on_who(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Up => self.who_sel = self.who_sel.saturating_sub(1),
            KeyCode::Down => {
                self.who_sel = (self.who_sel + 1).min(self.online.len().saturating_sub(1))
            }
            KeyCode::Char('r') => {
                self.online = self.presence.list().await;
                self.who_sel = self.who_sel.min(self.online.len().saturating_sub(1));
            }
            KeyCode::Enter => {
                if let Some(u) = self.online.get(self.who_sel) {
                    let name = u.username.clone();
                    self.open_profile_by_name(&name, Screen::WhoOnline).await;
                }
            }
            KeyCode::Char('p') => self.begin_page(),
            KeyCode::Esc | KeyCode::Left | KeyCode::Char('q') => self.screen = Screen::MainMenu,
            _ => {}
        }
    }

    // ---- Paging ("yell") -------------------------------------------------

    /// Open the page composer for the selected online user (#68), explaining up
    /// front why it's not allowed rather than failing only at submit.
    fn begin_page(&mut self) {
        if self.user.is_guest() {
            self.status = "Guests cannot page — register an account first.".into();
            return;
        }
        let Some(target) = self.online.get(self.who_sel).map(|u| u.username.clone()) else {
            return;
        };
        // Paging yourself would just echo back to your own sessions; skip it.
        if target == self.user.username {
            self.status = "You can't page yourself.".into();
            return;
        }
        self.form = Form::new(vec![Field::new("Message", false)]);
        self.page_target = Some(target);
        self.screen = Screen::ComposePage;
    }

    async fn on_compose_page(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => {
                self.page_target = None;
                self.screen = Screen::WhoOnline;
            }
            KeyCode::Enter => self.submit_page().await,
            KeyCode::Backspace => self.form.backspace(),
            KeyCode::Char(c) => self.form.insert(c),
            _ => {}
        }
    }

    /// Deliver the page to every live session of the target user. A page is a
    /// transient toast, not stored — if the target has since disconnected, we
    /// say so rather than pretending it landed.
    async fn submit_page(&mut self) {
        let Some(target) = self.page_target.take() else {
            self.screen = Screen::WhoOnline;
            return;
        };
        let body = self.form.value(0).trim().to_string();
        if body.is_empty() {
            self.status = "Say something first.".into();
            self.page_target = Some(target); // keep composing
            return;
        }
        // Honor the target's block list (#97), unless the sender is a sysop.
        if !self.user.is_admin()
            && let Ok(Some(t)) = auth::find_user(&self.pool, &target).await
            && blocks::is_blocked(&self.pool, t.id, self.user.id)
                .await
                .unwrap_or(false)
        {
            self.status = format!("{target} isn't accepting pages from you.");
            self.screen = Screen::WhoOnline;
            return;
        }
        let event = Event::Paged {
            from: self.user.username.clone(),
            body,
        };
        let delivered = self.presence.send_to_user(&target, event).await;
        self.status = if delivered > 0 {
            format!("Paged {target}.")
        } else {
            format!("{target} is no longer online.")
        };
        // Refresh the roster in case they left, then return to it.
        self.online = self.presence.list().await;
        self.who_sel = self.who_sel.min(self.online.len().saturating_sub(1));
        self.screen = Screen::WhoOnline;
    }

    // ---- Profiles --------------------------------------------------------

    /// Note whether the viewer has blocked the profile now shown (#97), so the
    /// Profile screen can label the toggle. Own profile is never "blocked".
    async fn refresh_profile_block_state(&mut self) {
        self.current_profile_blocked = match self.current_profile.as_ref() {
            Some(p) if p.user_id != self.user.id => {
                blocks::is_blocked(&self.pool, self.user.id, p.user_id)
                    .await
                    .unwrap_or(false)
            }
            _ => false,
        };
    }

    /// Load and show a profile by user id, remembering where to return.
    async fn open_profile(&mut self, user_id: i64, back: Screen) {
        match profiles::get_profile(&self.pool, user_id).await {
            Ok(p) => {
                self.current_profile = Some(p);
                self.profile_back = back;
                self.refresh_profile_block_state().await;
                self.screen = Screen::Profile;
            }
            Err(e) => self.status = format!("Error loading profile: {e}"),
        }
    }

    /// Load and show a profile by username (e.g. from who's-online).
    async fn open_profile_by_name(&mut self, username: &str, back: Screen) {
        match profiles::get_profile_by_name(&self.pool, username).await {
            Ok(p) => {
                self.current_profile = Some(p);
                self.profile_back = back;
                self.refresh_profile_block_state().await;
                self.screen = Screen::Profile;
            }
            Err(e) => self.status = format!("Error loading profile: {e}"),
        }
    }

    /// True when the shown profile is the viewer's own (and thus editable).
    fn viewing_own_profile(&self) -> bool {
        self.current_profile
            .as_ref()
            .is_some_and(|p| p.user_id == self.user.id)
    }

    /// Whether the currently-shown profile can be edited (own, non-guest).
    pub fn can_edit_current_profile(&self) -> bool {
        self.viewing_own_profile() && !self.user.is_guest()
    }

    /// Whether the shown profile can be blocked/unblocked by the viewer (#97):
    /// someone else's, a real (non-guest) viewer, and not an admin's.
    pub fn can_block_current_profile(&self) -> bool {
        !self.user.is_guest()
            && self
                .current_profile
                .as_ref()
                .is_some_and(|p| p.user_id != self.user.id && p.role != "admin")
    }

    async fn on_profile(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('e') if self.viewing_own_profile() && !self.user.is_guest() => {
                self.begin_edit_profile()
            }
            KeyCode::Char('b') if self.can_block_current_profile() => {
                self.toggle_block_current_profile().await
            }
            KeyCode::Char('i') if self.viewing_own_profile() && !self.user.is_guest() => {
                self.open_ignore_list().await
            }
            KeyCode::Char('f') if self.viewing_own_profile() && !self.user.is_guest() => {
                self.toggle_finger_optout().await
            }
            KeyCode::Esc | KeyCode::Left | KeyCode::Char('q') => self.screen = self.profile_back,
            _ => {}
        }
    }

    /// Toggle the viewer's own finger visibility (#77) and refresh the shown
    /// profile so the marker updates in place.
    async fn toggle_finger_optout(&mut self) {
        let now_opted_out = !self
            .current_profile
            .as_ref()
            .is_some_and(|p| p.finger_optout);
        match profiles::set_finger_optout(&self.pool, self.user.id, now_opted_out).await {
            Ok(()) => {
                if let Some(p) = self.current_profile.as_mut() {
                    p.finger_optout = now_opted_out;
                }
                self.status = if now_opted_out {
                    "Hidden from finger.".into()
                } else {
                    "Listed in finger.".into()
                };
            }
            Err(e) => self.status = format!("Could not update finger setting: {e}"),
        }
    }

    async fn toggle_block_current_profile(&mut self) {
        let Some(p) = self.current_profile.clone() else {
            return;
        };
        if self.current_profile_blocked {
            match blocks::unblock(&self.pool, self.user.id, p.user_id).await {
                Ok(()) => {
                    self.current_profile_blocked = false;
                    self.status = format!("Unblocked {}.", p.username);
                }
                Err(e) => self.status = format!("Could not unblock: {e}"),
            }
        } else {
            // Resolve the target to a User so the service can apply its guards
            // (no admins, no self, no guests).
            let target = match auth::find_user(&self.pool, &p.username).await {
                Ok(Some(u)) => u,
                _ => {
                    self.status = "Could not find that user.".into();
                    return;
                }
            };
            match blocks::block(&self.pool, &self.user, &target).await {
                Ok(()) => {
                    self.current_profile_blocked = true;
                    self.status = format!(
                        "Blocked {}. You won't see their posts or hear from them.",
                        p.username
                    );
                }
                Err(e) => self.status = format!("Could not block: {e}"),
            }
        }
    }

    async fn open_ignore_list(&mut self) {
        match blocks::list_blocked(&self.pool, self.user.id).await {
            Ok(list) => {
                self.ignored = list;
                self.ignored_sel = 0;
                self.screen = Screen::IgnoreList;
            }
            Err(e) => self.status = format!("Error loading ignore list: {e}"),
        }
    }

    async fn on_ignore_list(&mut self, key: KeyEvent) {
        let last = self.ignored.len().saturating_sub(1);
        match key.code {
            KeyCode::Up => self.ignored_sel = self.ignored_sel.saturating_sub(1),
            KeyCode::Down => self.ignored_sel = (self.ignored_sel + 1).min(last),
            KeyCode::Char('u') | KeyCode::Enter => {
                if let Some((id, name)) = self.ignored.get(self.ignored_sel).cloned() {
                    match blocks::unblock(&self.pool, self.user.id, id).await {
                        Ok(()) => {
                            self.ignored.remove(self.ignored_sel);
                            self.ignored_sel =
                                self.ignored_sel.min(self.ignored.len().saturating_sub(1));
                            self.status = format!("Unblocked {name}.");
                        }
                        Err(e) => self.status = format!("Could not unblock: {e}"),
                    }
                }
            }
            KeyCode::Esc | KeyCode::Left | KeyCode::Char('q') => self.screen = Screen::Profile,
            _ => {}
        }
    }

    fn begin_edit_profile(&mut self) {
        // Edit is gated on a shown, own profile, so this is always Some.
        let Some(p) = self.current_profile.clone() else {
            return;
        };
        let mut fields = vec![
            Field::new("Real name", false),
            Field::new("Location", false),
            Field::new("Tagline", false),
            Field::new("Signature", false),
        ];
        fields[0].value = p.real_name;
        fields[1].value = p.location;
        fields[2].value = p.tagline;
        fields[3].value = p.signature;
        self.form = Form::new(fields);
        self.screen = Screen::EditProfile;
    }

    async fn on_edit_profile(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => self.screen = Screen::Profile,
            KeyCode::Enter if self.form.on_last() => self.submit_profile().await,
            KeyCode::Enter | KeyCode::Tab | KeyCode::Down => self.form.next_field(),
            KeyCode::BackTab | KeyCode::Up => self.form.prev_field(),
            KeyCode::Backspace => self.form.backspace(),
            KeyCode::Char(c) => self.form.insert(c),
            _ => {}
        }
    }

    async fn submit_profile(&mut self) {
        let (real_name, location, tagline, signature) = (
            self.form.value(0).to_string(),
            self.form.value(1).to_string(),
            self.form.value(2).to_string(),
            self.form.value(3).to_string(),
        );
        match profiles::update_profile(
            &self.pool,
            self.user.id,
            &real_name,
            &location,
            &tagline,
            &signature,
        )
        .await
        {
            Ok(()) => {
                self.open_profile(self.user.id, self.profile_back).await;
                self.status = "Profile updated.".into();
            }
            Err(e) => {
                self.status = format!("Could not update profile: {e}");
                // Stay on the form so the user can fix an over-long field.
            }
        }
    }

    // ---- Stats -----------------------------------------------------------

    async fn open_stats(&mut self) {
        match stats::gather(&self.pool, stats::LIST_LIMIT).await {
            Ok(s) => {
                self.stats = Some(s);
                self.screen = Screen::Stats;
            }
            Err(e) => self.status = format!("Error loading stats: {e}"),
        }
    }

    async fn on_stats(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('r') => {
                // Refresh in place; keep the old snapshot on error.
                if let Ok(s) = stats::gather(&self.pool, stats::LIST_LIMIT).await {
                    self.stats = Some(s);
                }
            }
            KeyCode::Esc | KeyCode::Left | KeyCode::Char('q') => self.screen = Screen::MainMenu,
            _ => {}
        }
    }

    // ---- Search ----------------------------------------------------------

    fn begin_search(&mut self) {
        self.form = Form::new(vec![Field::new("Search boards", false)]);
        self.screen = Screen::SearchInput;
    }

    async fn on_search_input(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => self.screen = Screen::MainMenu,
            KeyCode::Enter => self.submit_search().await,
            KeyCode::Backspace => self.form.backspace(),
            KeyCode::Char(c) => self.form.insert(c),
            _ => {}
        }
    }

    async fn submit_search(&mut self) {
        let query = self.form.value(0).to_string();
        if query.is_empty() {
            self.status = "Enter a search term.".into();
            return;
        }
        match search::search_messages(&self.pool, &self.user.role, &query, search::SEARCH_LIMIT)
            .await
        {
            Ok(hits) => {
                if hits.is_empty() {
                    self.status = format!("No messages match \"{query}\".");
                }
                self.search_results = hits;
                self.search_sel = 0;
                self.search_query = query;
                self.screen = Screen::SearchResults;
            }
            Err(e) => self.status = format!("Search failed: {e}"),
        }
    }

    async fn on_search_results(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Up => self.search_sel = self.search_sel.saturating_sub(1),
            KeyCode::Down => {
                self.search_sel =
                    (self.search_sel + 1).min(self.search_results.len().saturating_sub(1))
            }
            KeyCode::Enter => {
                if let Some(hit) = self.search_results.get(self.search_sel) {
                    let (board_id, message_id) = (hit.board_id, hit.id);
                    self.open_message_in_board(board_id, message_id).await;
                }
            }
            // Refine: go back to the input, prefilled with the last query.
            KeyCode::Char('/') => {
                self.form = Form::new(vec![Field::new("Search boards", false)]);
                self.form.fields[0].value = self.search_query.clone();
                self.screen = Screen::SearchInput;
            }
            KeyCode::Esc | KeyCode::Left | KeyCode::Char('q') => self.screen = Screen::MainMenu,
            _ => {}
        }
    }

    /// Jump from a search hit to the message in its board: load the board (so
    /// back-navigation lands on its message list) and open the message.
    async fn open_message_in_board(&mut self, board_id: i64, message_id: i64) {
        let board = match boards::get_board(&self.pool, board_id).await {
            Ok(b) => b,
            Err(e) => {
                self.status = format!("Cannot open board: {e}");
                return;
            }
        };
        // Defense in depth: the search already filtered by read ACL.
        if !board.can_read(&self.user.role) {
            self.status = "You can't read that board.".into();
            return;
        }
        if !self.load_board(board).await {
            return;
        }
        // Highlight the hit in the message list on return, if still present.
        self.msg_sel = self
            .messages
            .iter()
            .position(|t| t.message.id == message_id)
            .unwrap_or(0);
        match boards::get_message(&self.pool, message_id).await {
            Ok(full) => {
                self.current_msg_signature = profiles::signature_of(&self.pool, full.author_id)
                    .await
                    .unwrap_or_default();
                self.current_message = Some(full);
                self.screen = Screen::ReadMessage;
            }
            Err(e) => {
                // The message vanished (deleted) between search and open.
                self.status = format!("Message unavailable: {e}");
                self.screen = Screen::MessageList;
            }
        }
    }

    // ---- Doors -----------------------------------------------------------

    fn on_doors(&mut self, key: KeyEvent) {
        let last = self.config.doors.len().saturating_sub(1);
        match key.code {
            KeyCode::Up => self.door_sel = self.door_sel.saturating_sub(1),
            KeyCode::Down => self.door_sel = (self.door_sel + 1).min(last),
            // Signal the run loop to launch the door (it owns the terminal and
            // the raw byte streams needed to bridge the program's I/O).
            KeyCode::Enter => {
                if self.door_sel < self.config.doors.len() {
                    self.pending_door = Some(self.door_sel);
                }
            }
            KeyCode::Esc | KeyCode::Left | KeyCode::Char('q') => self.screen = Screen::MainMenu,
            _ => {}
        }
    }

    // ---- File areas ------------------------------------------------------

    async fn open_file_areas(&mut self) {
        match files::list_readable_areas(&self.pool, &self.user.role).await {
            Ok(list) => {
                self.file_areas = list;
                self.file_area_sel = 0;
                self.screen = Screen::FileAreas;
            }
            Err(e) => self.status = format!("Error loading file areas: {e}"),
        }
    }

    async fn on_file_areas(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Up => self.file_area_sel = self.file_area_sel.saturating_sub(1),
            KeyCode::Down => {
                self.file_area_sel =
                    (self.file_area_sel + 1).min(self.file_areas.len().saturating_sub(1))
            }
            KeyCode::Enter => {
                if let Some(area) = self.file_areas.get(self.file_area_sel).cloned() {
                    self.open_file_area(area).await;
                }
            }
            KeyCode::Esc | KeyCode::Left | KeyCode::Char('q') => self.screen = Screen::MainMenu,
            _ => {}
        }
    }

    async fn open_file_area(&mut self, area: FileArea) {
        match files::list_files(&self.pool, area.id).await {
            Ok(list) => {
                self.files = list;
                self.file_sel = 0;
                self.current_file_area = Some(area);
                self.screen = Screen::FileList;
            }
            Err(e) => self.status = format!("Error loading files: {e}"),
        }
    }

    async fn on_file_list(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Up => self.file_sel = self.file_sel.saturating_sub(1),
            KeyCode::Down => {
                self.file_sel = (self.file_sel + 1).min(self.files.len().saturating_sub(1))
            }
            KeyCode::Enter => {
                if let Some(file) = self.files.get(self.file_sel).cloned() {
                    self.current_file = Some(file);
                    self.screen = Screen::FileDetail;
                }
            }
            KeyCode::Esc | KeyCode::Left | KeyCode::Char('q') => self.screen = Screen::FileAreas,
            _ => {}
        }
    }

    /// Whether the current session may edit the current file's description
    /// (its uploader, or any admin).
    pub fn can_edit_current_file(&self) -> bool {
        self.current_file
            .as_ref()
            .is_some_and(|f| f.uploader_id == self.user.id || self.user.is_admin())
    }

    async fn on_file_detail(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('e') if self.can_edit_current_file() => {
                let current = self
                    .current_file
                    .as_ref()
                    .map(|f| f.description.clone())
                    .unwrap_or_default();
                let mut field = Field::new("Description", false);
                field.value = current;
                self.form = Form::new(vec![field]);
                self.screen = Screen::EditFileDesc;
            }
            KeyCode::Enter | KeyCode::Char('v') => self.open_current_file().await,
            KeyCode::Esc | KeyCode::Left | KeyCode::Char('q') => self.screen = Screen::FileList,
            _ => {}
        }
    }

    /// Open the current file for in-BBS viewing: archives (zip/tar.gz) show an
    /// entry list, `.gz`/plain files preview as text, and binaries are refused.
    /// The (bounded) decode runs on a blocking thread.
    async fn open_current_file(&mut self) {
        let Some(file) = self.current_file.clone() else {
            return;
        };
        let path = self.config.files.storage_dir.join(&file.storage_path);
        let filename = file.filename.clone();
        let cfg = self.config.files.clone();
        let result =
            tokio::task::spawn_blocking(move || archive::inspect(&path, &filename, &cfg)).await;
        match result {
            Ok(Ok(Preview::Archive { entries, truncated })) => {
                self.archive_entries = entries;
                self.archive_sel = 0;
                self.archive_truncated = truncated;
                self.screen = Screen::ArchiveList;
            }
            Ok(Ok(Preview::Text { content, truncated })) => {
                self.show_text(
                    file.filename.clone(),
                    content,
                    truncated,
                    Screen::FileDetail,
                );
            }
            Ok(Ok(Preview::Binary)) => {
                self.status = "Binary file — download it over SFTP to open.".into();
            }
            Ok(Err(e)) => self.status = format!("Cannot open file: {e}"),
            Err(_) => self.status = "Cannot open file.".into(),
        }
    }

    fn show_text(&mut self, title: String, content: String, truncated: bool, back: Screen) {
        self.file_view_title = title;
        self.file_view_body = content;
        self.file_view_truncated = truncated;
        self.file_view_scroll = 0;
        self.file_view_back = back;
        self.screen = Screen::FileView;
    }

    async fn on_archive_list(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Up => self.archive_sel = self.archive_sel.saturating_sub(1),
            KeyCode::Down => {
                self.archive_sel =
                    (self.archive_sel + 1).min(self.archive_entries.len().saturating_sub(1))
            }
            KeyCode::Enter => self.open_archive_entry().await,
            KeyCode::Esc | KeyCode::Left | KeyCode::Char('q') => self.screen = Screen::FileDetail,
            _ => {}
        }
    }

    async fn open_archive_entry(&mut self) {
        let Some(entry) = self.archive_entries.get(self.archive_sel) else {
            return;
        };
        if entry.is_dir {
            self.status = "That's a directory.".into();
            return;
        }
        let entry_name = entry.name.clone();
        let Some(file) = self.current_file.clone() else {
            return;
        };
        let path = self.config.files.storage_dir.join(&file.storage_path);
        let filename = file.filename.clone();
        let cfg = self.config.files.clone();
        let lookup = entry_name.clone();
        let result = tokio::task::spawn_blocking(move || {
            archive::read_entry(&path, &filename, &lookup, &cfg)
        })
        .await;
        match result {
            Ok(Ok(Preview::Text { content, truncated })) => {
                self.show_text(entry_name, content, truncated, Screen::ArchiveList);
            }
            Ok(Ok(Preview::Binary)) => {
                self.status = "Binary entry — download the archive over SFTP.".into();
            }
            Ok(Ok(Preview::Archive { .. })) => {
                self.status = "Nested archives aren't supported.".into();
            }
            Ok(Err(_)) => self.status = "Could not read that entry.".into(),
            Err(_) => self.status = "Could not read that entry.".into(),
        }
    }

    fn on_file_view(&mut self, key: KeyEvent) {
        // Cap scrolling near the end (line count is a lower bound with wrapping).
        let max = self.file_view_body.lines().count().saturating_sub(1) as u16;
        match key.code {
            KeyCode::Up => self.file_view_scroll = self.file_view_scroll.saturating_sub(1),
            KeyCode::Down => self.file_view_scroll = (self.file_view_scroll + 1).min(max),
            KeyCode::PageUp => self.file_view_scroll = self.file_view_scroll.saturating_sub(20),
            KeyCode::PageDown => self.file_view_scroll = (self.file_view_scroll + 20).min(max),
            KeyCode::Home => self.file_view_scroll = 0,
            KeyCode::Esc | KeyCode::Left | KeyCode::Char('q') => self.screen = self.file_view_back,
            _ => {}
        }
    }

    async fn on_edit_file_desc(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => self.screen = Screen::FileDetail,
            KeyCode::Enter => self.submit_file_desc().await,
            KeyCode::Backspace => self.form.backspace(),
            KeyCode::Char(c) => self.form.insert(c),
            _ => {}
        }
    }

    async fn submit_file_desc(&mut self) {
        let Some(file_id) = self.current_file.as_ref().map(|f| f.id) else {
            self.screen = Screen::FileList;
            return;
        };
        let description = self.form.value(0).to_string();
        match files::set_description(&self.pool, file_id, &description).await {
            Ok(_) => {
                // Refresh the detail view and the underlying list.
                if let Ok(updated) = files::get_file(&self.pool, file_id).await {
                    self.current_file = Some(updated);
                }
                if let Some(area_id) = self.current_file_area.as_ref().map(|a| a.id)
                    && let Ok(list) = files::list_files(&self.pool, area_id).await
                {
                    self.files = list;
                }
                self.screen = Screen::FileDetail;
                self.status = "Description updated.".into();
            }
            Err(e) => {
                self.status = format!("Could not update: {e}");
                self.screen = Screen::FileDetail;
            }
        }
    }

    // ---- SSH keys --------------------------------------------------------

    async fn open_keys(&mut self) {
        match keys::list_keys(&self.pool, self.user.id).await {
            Ok(list) => {
                self.user_keys = list;
                self.key_sel = 0;
                self.screen = Screen::Keys;
            }
            Err(e) => self.status = format!("Error loading keys: {e}"),
        }
    }

    async fn reload_keys(&mut self) {
        if let Ok(list) = keys::list_keys(&self.pool, self.user.id).await {
            self.key_sel = self.key_sel.min(list.len().saturating_sub(1));
            self.user_keys = list;
        }
    }

    async fn on_keys(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Up => self.key_sel = self.key_sel.saturating_sub(1),
            KeyCode::Down => {
                self.key_sel = (self.key_sel + 1).min(self.user_keys.len().saturating_sub(1))
            }
            KeyCode::Char('n') => {
                self.form = Form::new(vec![Field::new("Public key (ssh-… AAAA… comment)", false)]);
                self.screen = Screen::AddKey;
            }
            KeyCode::Char('d') => self.delete_selected_key().await,
            KeyCode::Esc | KeyCode::Left | KeyCode::Char('q') => self.screen = Screen::MainMenu,
            _ => {}
        }
    }

    async fn delete_selected_key(&mut self) {
        let Some(k) = self.user_keys.get(self.key_sel).cloned() else {
            return;
        };
        match keys::delete_key(&self.pool, self.user.id, k.id).await {
            Ok(true) => {
                self.reload_keys().await;
                self.status = "Key removed.".into();
            }
            Ok(false) => self.status = "Key already gone.".into(),
            Err(e) => self.status = format!("Could not remove key: {e}"),
        }
    }

    async fn on_add_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => self.screen = Screen::Keys,
            KeyCode::Enter => self.submit_key().await,
            KeyCode::Backspace => self.form.backspace(),
            KeyCode::Char(c) => self.form.insert(c),
            _ => {}
        }
    }

    async fn submit_key(&mut self) {
        let line = self.form.value(0).to_string();
        if line.is_empty() {
            self.status = "Paste a public key first.".into();
            return;
        }
        match pubkey::register(&self.pool, self.user.id, &line, "").await {
            Ok(parsed) => {
                self.open_keys().await;
                self.status = format!("Added {} key {}.", parsed.algorithm, parsed.fingerprint);
            }
            Err(AppError::KeyExists) => self.status = "You've already registered that key.".into(),
            Err(AppError::InvalidKey(e)) => self.status = format!("Not a valid public key: {e}"),
            Err(e) => self.status = format!("Could not add key: {e}"),
        }
    }

    // ---- Register --------------------------------------------------------

    async fn on_register(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => self.screen = Screen::MainMenu,
            KeyCode::Enter if self.form.on_last() => self.submit_register().await,
            KeyCode::Enter | KeyCode::Tab | KeyCode::Down => self.form.next_field(),
            KeyCode::BackTab | KeyCode::Up => self.form.prev_field(),
            KeyCode::Backspace => self.form.backspace(),
            KeyCode::Char(c) => self.form.insert(c),
            _ => {}
        }
    }

    async fn submit_register(&mut self) {
        if !self.config.features.registration {
            self.status = "Registration is disabled.".into();
            return;
        }
        let username = self.form.value(0).to_string();
        let password = self.form.value(1).to_string();
        let confirm = self.form.value(2).to_string();
        if username.is_empty() || password.is_empty() {
            self.status = "Username and password are required.".into();
            return;
        }
        if password != confirm {
            self.status = "Passwords do not match.".into();
            return;
        }
        match auth::register_user(&self.pool, &username, &password, &self.config.accounts).await {
            Ok(_) => {
                self.screen = Screen::MainMenu;
                self.status =
                    format!("Account '{username}' created — reconnect over SSH as that user.");
            }
            Err(AppError::UsernameTaken) => self.status = "That username is taken.".into(),
            Err(AppError::UsernameReserved) => {
                self.status = "That username is reserved — please choose another.".into()
            }
            Err(e) => self.status = format!("Could not register: {e}"),
        }
    }

    // ---- Generic reader (message / mail / help) --------------------------

    fn on_reader(&mut self, key: KeyEvent, back: Screen) {
        match key.code {
            KeyCode::Esc | KeyCode::Left | KeyCode::Char('q') | KeyCode::Enter => {
                self.screen = back
            }
            _ => {}
        }
    }
}

/// Every screen that can carry `[art.screens]` art: the canonical config key
/// and a human label.
///
/// The authoritative list, kept immediately above the matcher that consumes it
/// so the two can't quietly drift. `bbscfg` renders this rather than keeping its
/// own copy (#146) — a second list would eventually offer keys the server
/// ignores, which fails silently: the screen simply renders without art.
///
/// The matcher below also accepts shorter aliases (`boards`, `mail`, `who`,
/// `files`, `messages`) for configs written by hand; those are deliberately not
/// offered here, since a UI should suggest one spelling rather than several.
pub const ART_SCREEN_KEYS: &[(&str, &str)] = &[
    ("main_menu", "Main menu"),
    ("bulletins", "Bulletins"),
    ("board_list", "Board list"),
    ("message_list", "Message list"),
    ("mailbox", "Mailbox"),
    ("who_online", "Who's online"),
    ("profile", "Profile"),
    ("stats", "Stats"),
    ("search", "Search results"),
    ("file_areas", "File areas"),
    ("file_list", "File list"),
    ("keys", "SSH keys"),
    ("help", "Help"),
    ("admin", "Admin"),
];

/// Map an `[art.screens]` config key to the screen it heads. Unknown keys
/// return `None` (skipped with a warning).
pub fn screen_from_art_key(key: &str) -> Option<Screen> {
    Some(match key.trim().to_ascii_lowercase().as_str() {
        "main_menu" => Screen::MainMenu,
        "bulletins" => Screen::Bulletins,
        "board_list" | "boards" => Screen::BoardList,
        "message_list" | "messages" => Screen::MessageList,
        "mailbox" | "mail" => Screen::Mailbox,
        "who_online" | "who" => Screen::WhoOnline,
        "profile" => Screen::Profile,
        "stats" => Screen::Stats,
        "search" => Screen::SearchResults,
        "file_areas" | "files" => Screen::FileAreas,
        "file_list" => Screen::FileList,
        "keys" => Screen::Keys,
        "help" => Screen::Help,
        "admin" => Screen::AdminUsers,
        _ => return None,
    })
}

/// Load operator art into per-screen [`Text`]. `welcome` heads the main menu;
/// `screens` maps keys to files. Missing/oversized files are logged and skipped
/// so a bad art path never breaks a session. Files are read once at login.
fn load_art(cfg: &crate::config::Art) -> HashMap<Screen, Text<'static>> {
    /// Cap on a single art file, so a pathological file can't blow up memory.
    const MAX_ART_BYTES: u64 = 256 * 1024;

    let mut out = HashMap::new();
    let mut load = |screen: Screen, file: &str| {
        let file = file.trim();
        if file.is_empty() {
            return;
        }
        let path = cfg.dir.join(file);
        match std::fs::metadata(&path) {
            Ok(m) if m.len() > MAX_ART_BYTES => {
                tracing::warn!(
                    "art file {} exceeds {MAX_ART_BYTES} bytes; skipping",
                    path.display()
                );
                return;
            }
            Ok(_) => {}
            Err(e) => {
                tracing::warn!("cannot read art file {}: {e}", path.display());
                return;
            }
        }
        match std::fs::read(&path) {
            Ok(bytes) => {
                out.insert(screen, ansi::to_text(&bytes));
            }
            Err(e) => tracing::warn!("cannot read art file {}: {e}", path.display()),
        }
    };

    load(Screen::MainMenu, &cfg.welcome);
    for (key, file) in &cfg.screens {
        match screen_from_art_key(key) {
            Some(screen) => load(screen, file),
            None => tracing::warn!("unknown [art.screens] key: {key:?}"),
        }
    }
    out
}

/// Merge our not-yet-published submissions into a mirrored board's thread.
///
/// A pending reply is placed directly after the post it answers, one level
/// deeper; a pending root (or one whose parent we don't hold) goes at the top,
/// since it's the newest thing the user did. Everything else keeps the order
/// `mirror::thread` produced.
fn merge_pending(
    posts: Vec<crate::services::federation::mirror::ThreadedPost>,
    pending: Vec<crate::services::federation::remote_posting::Pending>,
) -> Vec<MirrorRow> {
    let mut rows: Vec<MirrorRow> = posts
        .into_iter()
        .map(|t| MirrorRow {
            ap_id: t.post.ap_id,
            subject: t.post.subject,
            author_handle: t.post.author_handle,
            published: t.post.published,
            body: t.post.content,
            depth: t.depth,
            pending: false,
        })
        .collect();

    for p in pending {
        let row = MirrorRow {
            ap_id: p.ap_id,
            subject: p.subject,
            author_handle: p.author_handle,
            published: p.created_at,
            body: p.body,
            depth: 0,
            pending: true,
        };
        match p
            .in_reply_to
            .as_deref()
            .and_then(|parent| rows.iter().position(|r| r.ap_id == parent))
        {
            Some(at) => {
                let depth = rows[at].depth + 1;
                rows.insert(at + 1, MirrorRow { depth, ..row });
            }
            None => rows.insert(0, row),
        }
    }
    rows
}

/// Trim a subject/title for inclusion in the one-line status bar.
fn truncate_status(s: &str) -> String {
    const MAX: usize = 40;
    if s.chars().count() <= MAX {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(MAX - 1).collect();
        out.push('…');
        out
    }
}

/// The transport-agnostic event loop: draw, wait for an event, apply it, repeat.
/// Generic over any `Write` sink so SSH and the WebSocket frontend share it.
///
/// `raw_out` writes bytes straight to the client (bypassing ratatui) — used to
/// bridge a door program's output while the TUI is suspended.
pub async fn run<W: std::io::Write>(
    mut app: App,
    mut terminal: Terminal<CrosstermBackend<W>>,
    mut events: Receiver<Event>,
    raw_out: tokio::sync::mpsc::UnboundedSender<Vec<u8>>,
) -> anyhow::Result<()> {
    // Show bulletins after login when any exist.
    app.load_startup_bulletins().await;
    // Populate the unread counts (board badges + mail badge) for the first
    // frame, and surface a one-shot "new mail" notice in the status bar. The
    // status line always renders, so this shows even when bulletins take over
    // the landing screen; it clears as soon as the user presses a key.
    app.refresh_unread().await;
    if app.mail_unread > 0 {
        app.status = format!(
            "\u{1F4EC} You have {} unread message(s) \u{2014} open Private Mail to read.",
            app.mail_unread
        );
    }
    terminal.draw(|f| ui::draw(f, &app))?;

    while let Some(event) = events.recv().await {
        match event {
            Event::Key(key) => app.handle_key(key).await,
            Event::Resize(w, h) => {
                // Best-effort: ratatui's `resize` may fail when the backend has
                // no controlling tty to query, but it still updates the buffers
                // and viewport, so drawing continues correctly.
                let _ = terminal.resize(Rect::new(0, 0, w, h));
            }
            Event::Quit => app.should_quit = true,
            // A page from another user: ring the bell and surface it as a toast
            // in the status bar, wherever the recipient currently is. It clears
            // on their next keypress, like other status messages.
            Event::Paged { from, body } => {
                let _ = raw_out.send(b"\x07".to_vec());
                app.status = format!("\u{1F4DF} {from} pages you: {body}");
            }
            // A sysop broadcast to everyone (#69): same toast treatment as a
            // page, with a bell.
            Event::Broadcast { text } => {
                let _ = raw_out.send(b"\x07".to_vec());
                app.status = format!("\u{1F4E2} Broadcast: {text}");
            }
        }

        // A door launch was requested: suspend the TUI, bridge the program's
        // raw I/O, then resume.
        if let Some(idx) = app.pending_door.take() {
            let size = terminal
                .size()
                .map(|s| (s.width, s.height))
                .unwrap_or((80, 24));
            let outcome = door::run(
                &app.config.doors[idx],
                &app.user,
                app.session_id,
                size,
                &app.config.bbs.name,
                &app.config.bbs.sysop,
                &raw_out,
                &mut events,
            )
            .await;
            // Reset attributes and force a full repaint of the TUI on return.
            let _ = raw_out.send(b"\x1b[0m".to_vec());
            let _ = terminal.clear();
            match outcome {
                door::DoorExit::Quit => app.should_quit = true,
                door::DoorExit::Returned => app.screen = Screen::MainMenu,
                door::DoorExit::Failed(msg) => {
                    // The repaint would wipe anything the door wrote, so surface
                    // the reason in the status bar instead.
                    app.screen = Screen::MainMenu;
                    app.status = msg;
                }
            }
        }

        if app.should_quit {
            break;
        }
        terminal.draw(|f| ui::draw(f, &app))?;
    }

    app.presence.leave(app.session_id).await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::federation::mirror::{Post, ThreadedPost};
    use crate::services::federation::remote_posting::Pending;

    fn post(ap_id: &str, subject: &str, depth: u16) -> ThreadedPost {
        ThreadedPost {
            post: Post {
                id: 1,
                ap_id: ap_id.into(),
                group_uri: "https://remote.social/c/x".into(),
                group_handle: "x@remote.social".into(),
                author_handle: "bob@remote.social".into(),
                subject: subject.into(),
                content: "body".into(),
                url: None,
                published: 100,
                in_reply_to: None,
            },
            depth,
        }
    }

    fn pending(ap_id: &str, subject: &str, in_reply_to: Option<&str>) -> Pending {
        Pending {
            id: 1,
            ap_id: ap_id.into(),
            group_uri: "https://remote.social/c/x".into(),
            author_handle: "alice".into(),
            subject: subject.into(),
            body: "mine".into(),
            created_at: 200,
            in_reply_to: in_reply_to.map(Into::into),
        }
    }

    /// A pending reply sits under the post it answers, not at the top — the
    /// whole point of merging the two lists (#139 Slice C).
    #[test]
    fn a_pending_reply_is_placed_under_its_parent() {
        let rows = merge_pending(
            vec![post("p/1", "Root", 0), post("p/2", "Re: Root", 1)],
            vec![pending("mine/1", "Re: Root", Some("p/1"))],
        );
        let ids: Vec<&str> = rows.iter().map(|r| r.ap_id.as_str()).collect();
        assert_eq!(
            ids,
            vec!["p/1", "mine/1", "p/2"],
            "inserted after its parent"
        );
        assert_eq!(rows[1].depth, 1, "one level deeper than the parent");
        assert!(rows[1].pending);
        assert!(!rows[0].pending);
    }

    /// A pending *root* goes to the top: it's the newest thing the user did and
    /// answers nothing.
    #[test]
    fn a_pending_root_goes_first() {
        let rows = merge_pending(
            vec![post("p/1", "Root", 0)],
            vec![pending("mine/1", "New thread", None)],
        );
        assert_eq!(rows[0].ap_id, "mine/1");
        assert_eq!(rows[0].depth, 0);
    }

    /// A pending reply to a post we don't hold can't be nested, so it surfaces
    /// at the top rather than vanishing — same rule the mirror uses for an
    /// orphaned remote reply.
    #[test]
    fn a_pending_reply_to_an_unknown_parent_still_appears() {
        let rows = merge_pending(
            vec![post("p/1", "Root", 0)],
            vec![pending("mine/1", "Re: something", Some("p/999"))],
        );
        assert_eq!(rows.len(), 2, "nothing is dropped");
        assert_eq!(rows[0].ap_id, "mine/1");
        assert_eq!(rows[0].depth, 0);
    }

    /// Nesting under a reply keeps going deeper.
    #[test]
    fn a_pending_reply_to_a_reply_nests_two_deep() {
        let rows = merge_pending(
            vec![post("p/1", "Root", 0), post("p/2", "Re: Root", 1)],
            vec![pending("mine/1", "Re: Re: Root", Some("p/2"))],
        );
        let mine = rows.iter().find(|r| r.pending).unwrap();
        assert_eq!(mine.depth, 2);
    }
}
