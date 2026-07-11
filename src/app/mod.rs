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

use crate::db::models::{Board, Login, Mail, Message, User};
use crate::error::AppError;
use crate::services::presence::{OnlineUser, Presence};
use crate::services::{admin, auth, boards, mail};
use crate::transport::Event;

use state::{Field, Form, MenuItem, Screen};

/// All per-session state.
pub struct App {
    pool: SqlitePool,
    presence: Presence,
    pub user: User,
    session_id: usize,

    pub screen: Screen,
    pub should_quit: bool,
    pub status: String,

    // Main menu
    pub menu: Vec<MenuItem>,
    pub menu_sel: usize,

    // Boards
    pub boards: Vec<Board>,
    pub board_sel: usize,
    current_board_id: Option<i64>,
    pub current_board_name: String,

    // Messages
    pub messages: Vec<Message>,
    pub msg_sel: usize,
    pub current_message: Option<Message>,

    // Mail
    pub mails: Vec<Mail>,
    pub mail_sel: usize,
    pub current_mail: Option<Mail>,

    // Who's online
    pub online: Vec<OnlineUser>,

    // Admin
    pub admin_users: Vec<User>,
    pub admin_user_sel: usize,
    pub admin_logins: Vec<Login>,

    // Shared form for compose/register screens
    pub form: Form,
}

impl App {
    pub fn new(pool: SqlitePool, presence: Presence, user: User, session_id: usize) -> Self {
        // Registration is the newcomer bootstrap path — only the guest account,
        // which is how newcomers get in, needs it. Registered users don't.
        let mut menu = vec![MenuItem::Boards, MenuItem::Mail, MenuItem::Who];
        if user.is_guest() {
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
            user,
            session_id,
            screen: Screen::MainMenu,
            should_quit: false,
            status: String::new(),
            menu,
            menu_sel: 0,
            boards: Vec::new(),
            board_sel: 0,
            current_board_id: None,
            current_board_name: String::new(),
            messages: Vec::new(),
            msg_sel: 0,
            current_message: None,
            mails: Vec::new(),
            mail_sel: 0,
            current_mail: None,
            online: Vec::new(),
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
            Screen::BoardList => self.on_board_list(key).await,
            Screen::MessageList => self.on_message_list(key).await,
            Screen::ReadMessage => self.on_reader(key, Screen::MessageList),
            Screen::ComposePost => self.on_compose_post(key).await,
            Screen::Mailbox => self.on_mailbox(key).await,
            Screen::ReadMail => self.on_reader(key, Screen::Mailbox),
            Screen::ComposeMail => self.on_compose_mail(key).await,
            Screen::WhoOnline => self.on_who(key).await,
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
            MenuItem::Boards => self.open_boards().await,
            MenuItem::Mail => {
                if self.user.is_guest() {
                    self.status =
                        "Guests cannot use private mail — register an account first.".into();
                } else {
                    self.open_mailbox().await;
                }
            }
            MenuItem::Who => self.open_who().await,
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
        match boards::list_boards(&self.pool).await {
            Ok(b) => {
                self.boards = b;
                self.board_sel = 0;
                self.screen = Screen::BoardList;
            }
            Err(e) => self.status = format!("Error loading boards: {e}"),
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
            KeyCode::Esc | KeyCode::Left | KeyCode::Char('q') => self.screen = Screen::MainMenu,
            _ => {}
        }
    }

    async fn open_board(&mut self, board: Board) {
        match boards::list_messages(&self.pool, board.id).await {
            Ok(m) => {
                self.messages = m;
                self.msg_sel = 0;
                self.current_board_id = Some(board.id);
                self.current_board_name = board.name;
                self.screen = Screen::MessageList;
            }
            Err(e) => self.status = format!("Error loading messages: {e}"),
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
                if let Some(msg) = self.messages.get(self.msg_sel) {
                    match boards::get_message(&self.pool, msg.id).await {
                        Ok(full) => {
                            self.current_message = Some(full);
                            self.screen = Screen::ReadMessage;
                        }
                        Err(e) => self.status = format!("Error: {e}"),
                    }
                }
            }
            KeyCode::Char('n') => {
                if self.user.is_guest() {
                    self.status = "Guests cannot post — register an account first.".into();
                } else {
                    self.form = Form::new(vec![
                        Field::new("Subject", false),
                        Field::new("Body", false),
                    ]);
                    self.screen = Screen::ComposePost;
                }
            }
            KeyCode::Esc | KeyCode::Left | KeyCode::Char('q') => self.screen = Screen::BoardList,
            _ => {}
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
        let Some(board_id) = self.current_board_id else {
            self.status = "No board selected.".into();
            return;
        };
        match boards::post_message(&self.pool, board_id, &self.user, &subject, &body).await {
            Ok(()) => {
                if let Ok(m) = boards::list_messages(&self.pool, board_id).await {
                    self.messages = m;
                    self.msg_sel = 0;
                }
                self.screen = Screen::MessageList;
                self.status = "Message posted.".into();
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
        match mail::send_mail(&self.pool, &self.user, &to, &subject, &body).await {
            Ok(()) => {
                self.open_mailbox().await;
                self.status = format!("Mail sent to {to}.");
            }
            Err(AppError::RecipientNotFound) => {
                self.status = format!("No such user: {to}");
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
        match auth::register_user(&self.pool, &username, &password).await {
            Ok(_) => {
                self.screen = Screen::MainMenu;
                self.status =
                    format!("Account '{username}' created — reconnect over SSH as that user.");
            }
            Err(AppError::UsernameTaken) => self.status = "That username is taken.".into(),
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

/// The transport-agnostic event loop: draw, wait for an event, apply it, repeat.
/// Generic over any `Write` sink so SSH and a future WebSocket share it.
pub async fn run<W: std::io::Write>(
    mut app: App,
    mut terminal: Terminal<CrosstermBackend<W>>,
    mut events: Receiver<Event>,
) -> anyhow::Result<()> {
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
