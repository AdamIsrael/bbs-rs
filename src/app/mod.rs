//! The transport-agnostic TUI application: state, the async event loop, and
//! per-screen key handling. Rendering lives in [`ui`].

pub mod state;
pub mod ui;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::Rect;
use sqlx::sqlite::SqlitePool;
use tokio::sync::mpsc::Receiver;

use std::collections::HashSet;
use std::sync::Arc;

use crate::config::Settings;
use crate::db::models::{
    Board, Bulletin, FileArea, FileEntry, Login, Mail, Message, Oneliner, User, UserKey,
};
use crate::error::AppError;
use crate::services::archive::{self, ArchiveEntry, Preview};
use crate::services::boards::ThreadItem;
use crate::services::presence::{OnlineUser, Presence};
use crate::services::{admin, auth, boards, bulletins, files, keys, mail, oneliners};
use crate::ssh::pubkey;
use crate::transport::Event;

use state::{Field, Form, MenuItem, Screen};

/// All per-session state.
pub struct App {
    pool: SqlitePool,
    presence: Presence,
    pub config: Arc<Settings>,
    pub user: User,
    session_id: usize,

    pub screen: Screen,
    pub should_quit: bool,
    pub status: String,

    // Main menu
    pub menu: Vec<MenuItem>,
    pub menu_sel: usize,

    // Bulletins
    pub bulletins: Vec<Bulletin>,
    pub bulletin_sel: usize,
    pub current_bulletin: Option<Bulletin>,

    // Oneliners (graffiti wall)
    pub oneliners: Vec<Oneliner>,

    // Boards
    pub boards: Vec<Board>,
    pub board_sel: usize,
    pub current_board: Option<Board>,

    // Messages (threaded: each item carries its reply depth)
    pub messages: Vec<ThreadItem>,
    pub msg_sel: usize,
    pub current_message: Option<Message>,
    /// When composing, the message being replied to (None = new thread).
    reply_parent: Option<i64>,

    // Mail
    pub mails: Vec<Mail>,
    pub mail_sel: usize,
    pub current_mail: Option<Mail>,

    // Who's online
    pub online: Vec<OnlineUser>,

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

    // Shared form for compose/register screens
    pub form: Form,
}

impl App {
    pub fn new(
        pool: SqlitePool,
        presence: Presence,
        config: Arc<Settings>,
        user: User,
        session_id: usize,
    ) -> Self {
        // Menu honors the feature toggles. Registration is the newcomer
        // bootstrap path, so it's only offered to the guest account.
        let f = &config.features;
        let mut menu = vec![MenuItem::Bulletins, MenuItem::Boards];
        if f.oneliners {
            menu.push(MenuItem::Oneliners);
        }
        if f.private_mail {
            menu.push(MenuItem::Mail);
        }
        if f.who_online {
            menu.push(MenuItem::Who);
        }
        if f.file_areas {
            menu.push(MenuItem::Files);
        }
        // Key management is for real accounts (guests can't own keys).
        if f.pubkey_auth && !user.is_guest() {
            menu.push(MenuItem::Keys);
        }
        if user.is_guest() && f.registration {
            menu.push(MenuItem::Register);
        }
        if user.is_admin() {
            menu.push(MenuItem::Admin);
        }
        menu.push(MenuItem::Help);
        menu.push(MenuItem::Quit);
        Self {
            pool,
            presence,
            config,
            user,
            session_id,
            screen: Screen::MainMenu,
            should_quit: false,
            status: String::new(),
            menu,
            menu_sel: 0,
            bulletins: Vec::new(),
            bulletin_sel: 0,
            current_bulletin: None,
            oneliners: Vec::new(),
            boards: Vec::new(),
            board_sel: 0,
            current_board: None,
            messages: Vec::new(),
            msg_sel: 0,
            current_message: None,
            reply_parent: None,
            mails: Vec::new(),
            mail_sel: 0,
            current_mail: None,
            online: Vec::new(),
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
            form: Form::new(Vec::new()),
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
            Screen::BoardList => self.on_board_list(key).await,
            Screen::MessageList => self.on_message_list(key).await,
            Screen::ReadMessage => self.on_reader(key, Screen::MessageList),
            Screen::ComposePost => self.on_compose_post(key).await,
            Screen::Mailbox => self.on_mailbox(key).await,
            Screen::ReadMail => self.on_reader(key, Screen::Mailbox),
            Screen::ComposeMail => self.on_compose_mail(key).await,
            Screen::WhoOnline => self.on_who(key).await,
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
            KeyCode::Char('q') | KeyCode::Esc => self.should_quit = true,
            _ => {}
        }
    }

    async fn activate_menu(&mut self) {
        match self.menu[self.menu_sel] {
            MenuItem::Bulletins => self.open_bulletins().await,
            MenuItem::Boards => self.open_boards().await,
            MenuItem::Oneliners => self.open_oneliners().await,
            MenuItem::Mail => {
                if self.user.is_guest() {
                    self.status =
                        "Guests cannot use private mail — register an account first.".into();
                } else {
                    self.open_mailbox().await;
                }
            }
            MenuItem::Who => self.open_who().await,
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
        match oneliners::add(&self.pool, &self.user, &body, &self.config.limits).await {
            Ok(()) => {
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
            KeyCode::Esc | KeyCode::Left | KeyCode::Char('q') => self.screen = Screen::MainMenu,
            _ => {}
        }
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
                self.screen = Screen::AdminLogins;
            }
            Err(e) => self.status = format!("Error loading logins: {e}"),
        }
    }

    async fn on_admin_logins(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc | KeyCode::Left | KeyCode::Char('q') => self.screen = Screen::AdminUsers,
            _ => {}
        }
    }

    // ---- Boards ----------------------------------------------------------

    async fn open_boards(&mut self) {
        match boards::list_readable_boards(&self.pool, &self.user.role).await {
            Ok(b) => {
                self.boards = b;
                self.board_sel = 0;
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
        match boards::list_thread(&self.pool, board.id).await {
            Ok(m) => {
                self.messages = m;
                self.msg_sel = 0;
                self.current_board = Some(board);
                self.screen = Screen::MessageList;
            }
            Err(e) => self.status = format!("Error loading messages: {e}"),
        }
    }

    /// Reload the current board's messages in place (after posting or a
    /// moderation change), keeping the selection in range.
    async fn reload_messages(&mut self) {
        let Some(board_id) = self.current_board.as_ref().map(|b| b.id) else {
            return;
        };
        if let Ok(m) = boards::list_thread(&self.pool, board_id).await {
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
                            self.current_message = Some(full);
                            self.screen = Screen::ReadMessage;
                        }
                        Err(e) => self.status = format!("Error: {e}"),
                    }
                }
            }
            KeyCode::Char('n') => self.begin_compose_post(None),
            KeyCode::Char('r') => self.begin_reply(),
            // Admin moderation on the selected post.
            KeyCode::Char('d') if self.user.is_admin() => self.delete_selected_message().await,
            KeyCode::Char('p') if self.user.is_admin() => self.toggle_pin_selected().await,
            KeyCode::Esc | KeyCode::Left | KeyCode::Char('q') => self.screen = Screen::BoardList,
            _ => {}
        }
    }

    /// Reply to the selected message: pre-fill an `Re:` subject and remember the
    /// parent so `submit_post` files it under that message.
    fn begin_reply(&mut self) {
        let Some(item) = self.messages.get(self.msg_sel) else {
            return;
        };
        let parent = &item.message;
        let re = if parent.subject.to_ascii_lowercase().starts_with("re:") {
            parent.subject.clone()
        } else {
            format!("Re: {}", parent.subject)
        };
        let parent_id = parent.id;
        self.begin_compose_post(Some((parent_id, re)));
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
        self.form = Form::new(vec![subject, Field::new("Body", false)]);
        self.screen = Screen::ComposePost;
    }

    async fn delete_selected_message(&mut self) {
        let Some(item) = self.messages.get(self.msg_sel).cloned() else {
            return;
        };
        let msg = item.message;
        match boards::delete_message(&self.pool, msg.id).await {
            Ok(true) => {
                self.reload_messages().await;
                self.status = format!("Deleted post '{}'.", truncate_status(&msg.subject));
            }
            Ok(false) => self.status = "Post already gone.".into(),
            Err(e) => self.status = format!("Could not delete: {e}"),
        }
    }

    async fn toggle_pin_selected(&mut self) {
        let Some(item) = self.messages.get(self.msg_sel).cloned() else {
            return;
        };
        let msg = item.message;
        let pin = !msg.pinned;
        match boards::set_pinned(&self.pool, msg.id, pin).await {
            Ok(()) => {
                self.reload_messages().await;
                self.status = if pin { "Pinned." } else { "Unpinned." }.into();
            }
            Err(e) => self.status = format!("Could not update post: {e}"),
        }
    }

    async fn on_compose_post(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => self.screen = Screen::MessageList,
            KeyCode::Enter if self.form.on_last() => self.submit_post().await,
            KeyCode::Enter | KeyCode::Tab | KeyCode::Down => self.form.next_field(),
            KeyCode::BackTab | KeyCode::Up => self.form.prev_field(),
            KeyCode::Backspace => self.form.backspace(),
            KeyCode::Char(c) => self.form.insert(c),
            _ => {}
        }
    }

    async fn submit_post(&mut self) {
        let subject = self.form.value(0).to_string();
        let body = self.form.value(1).to_string();
        if subject.is_empty() {
            self.status = "Subject cannot be empty.".into();
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
            Ok(()) => {
                self.reply_parent = None;
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
            KeyCode::Char('n') => {
                self.form = Form::new(vec![
                    Field::new("To (username)", false),
                    Field::new("Subject", false),
                    Field::new("Body", false),
                ]);
                self.screen = Screen::ComposeMail;
            }
            KeyCode::Esc | KeyCode::Left | KeyCode::Char('q') => self.screen = Screen::MainMenu,
            _ => {}
        }
    }

    async fn on_compose_mail(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => self.screen = Screen::Mailbox,
            KeyCode::Enter if self.form.on_last() => self.submit_mail().await,
            KeyCode::Enter | KeyCode::Tab | KeyCode::Down => self.form.next_field(),
            KeyCode::BackTab | KeyCode::Up => self.form.prev_field(),
            KeyCode::Backspace => self.form.backspace(),
            KeyCode::Char(c) => self.form.insert(c),
            _ => {}
        }
    }

    async fn submit_mail(&mut self) {
        let to = self.form.value(0).to_string();
        let subject = self.form.value(1).to_string();
        let body = self.form.value(2).to_string();
        if to.is_empty() || subject.is_empty() {
            self.status = "Recipient and subject are required.".into();
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

    // ---- Who's online ----------------------------------------------------

    async fn open_who(&mut self) {
        self.online = self.presence.list().await;
        self.screen = Screen::WhoOnline;
    }

    async fn on_who(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('r') => self.online = self.presence.list().await,
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
/// Generic over any `Write` sink so SSH and a future WebSocket share it.
pub async fn run<W: std::io::Write>(
    mut app: App,
    mut terminal: Terminal<CrosstermBackend<W>>,
    mut events: Receiver<Event>,
) -> anyhow::Result<()> {
    // Show bulletins after login when any exist.
    app.load_startup_bulletins().await;
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
        }
        if app.should_quit {
            break;
        }
        terminal.draw(|f| ui::draw(f, &app))?;
    }

    app.presence.leave(app.session_id).await;
    Ok(())
}
