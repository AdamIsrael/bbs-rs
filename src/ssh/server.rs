//! russh server: one [`SessionHandler`] per connection, bridging SSH events to
//! the transport-agnostic [`app`] loop.

use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use ratatui::backend::CrosstermBackend;
use ratatui::layout::Rect;
use ratatui::{Terminal, TerminalOptions, Viewport};
use russh::keys::ssh_key::PublicKey;
use russh::server::{Auth, ChannelOpenHandle, Config, Handler, Msg, Server, Session};
use russh::{Channel, ChannelId, Pty};
use sqlx::sqlite::SqlitePool;
use tokio::sync::mpsc;

use crate::app::{self, App};
use crate::config;
use crate::db::models::User;
use crate::input;
use crate::services::presence::Presence;
use crate::services::{admin, auth};
use crate::ssh::terminal::{SshTerminal, TerminalHandle};
use crate::transport::Event;

/// Shared server state; cloned into each per-connection handler.
#[derive(Clone)]
struct BbsServer {
    pool: SqlitePool,
    presence: Presence,
    next_id: usize,
}

impl Server for BbsServer {
    type Handler = SessionHandler;

    fn new_client(&mut self, addr: Option<SocketAddr>) -> SessionHandler {
        self.next_id += 1;
        SessionHandler {
            pool: self.pool.clone(),
            presence: self.presence.clone(),
            id: self.next_id,
            peer: addr,
            user: None,
            terminal: None,
            events_tx: None,
            input_buf: Vec::new(),
        }
    }
}

/// Per-connection handler.
struct SessionHandler {
    pool: SqlitePool,
    presence: Presence,
    id: usize,
    /// The client's address, captured at connection for login logging and IP bans.
    peer: Option<SocketAddr>,
    user: Option<User>,
    /// Built at channel open, moved into the app task at shell request.
    terminal: Option<SshTerminal>,
    /// Feeds decoded input events to the running app loop.
    events_tx: Option<mpsc::Sender<Event>>,
    /// Carries incomplete escape/UTF-8 sequences between `data` callbacks.
    input_buf: Vec<u8>,
}

impl SessionHandler {
    /// The peer IP (without port) as a string, for logging and IP-ban checks.
    fn ip(&self) -> Option<String> {
        self.peer.map(|a| a.ip().to_string())
    }
}

impl Handler for SessionHandler {
    type Error = anyhow::Error;

    async fn auth_password(&mut self, user: &str, password: &str) -> Result<Auth, Self::Error> {
        let ip = self.ip();
        // `attempt_login` handles IP-ban and account-ban checks and records the
        // attempt (success or failure) in the login audit trail.
        match auth::attempt_login(&self.pool, user, password, ip.as_deref()).await {
            Ok(Some(u)) => {
                self.user = Some(u);
                Ok(Auth::Accept)
            }
            Ok(None) => Ok(Auth::reject()),
            Err(e) => {
                tracing::warn!("auth failed for {user:?}: {e}");
                Ok(Auth::reject())
            }
        }
    }

    // Public-key auth is not offered yet; password only for now.
    async fn auth_publickey(&mut self, _user: &str, _key: &PublicKey) -> Result<Auth, Self::Error> {
        Ok(Auth::reject())
    }

    async fn channel_open_session(
        &mut self,
        channel: Channel<Msg>,
        reply: ChannelOpenHandle,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        let handle = TerminalHandle::start(session.handle(), channel.id()).await;
        let backend = CrosstermBackend::new(handle);
        // Correct size is applied on the pty request.
        let options = TerminalOptions {
            viewport: Viewport::Fixed(Rect::default()),
        };
        self.terminal = Some(Terminal::with_options(backend, options)?);
        reply.accept().await;
        Ok(())
    }

    async fn pty_request(
        &mut self,
        channel: ChannelId,
        _term: &str,
        col_width: u32,
        row_height: u32,
        _pix_width: u32,
        _pix_height: u32,
        _modes: &[(Pty, u32)],
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        let (w, h) = clamp_size(col_width, row_height);
        if let Some(terminal) = self.terminal.as_mut() {
            resize_terminal(terminal, w, h);
        }
        session.channel_success(channel)?;
        Ok(())
    }

    async fn shell_request(
        &mut self,
        channel: ChannelId,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        match (self.terminal.take(), self.user.clone()) {
            (Some(terminal), Some(user)) => {
                let (tx, rx) = mpsc::channel::<Event>(64);
                self.events_tx = Some(tx.clone());
                self.presence
                    .join(self.id, user.username.clone(), self.ip(), tx)
                    .await;

                let app = App::new(self.pool.clone(), self.presence.clone(), user, self.id);
                let id = self.id;
                // The app loop is transport-agnostic; when it returns (user quit
                // or disconnect), close the SSH channel here so the client exits.
                let handle = session.handle();
                tokio::spawn(async move {
                    if let Err(e) = app::run(app, terminal, rx).await {
                        tracing::warn!("session {id} ended with error: {e}");
                    }
                    let _ = handle.exit_status_request(channel, 0).await;
                    let _ = handle.eof(channel).await;
                    let _ = handle.close(channel).await;
                });
                session.channel_success(channel)?;
            }
            _ => {
                session.channel_failure(channel)?;
            }
        }
        Ok(())
    }

    async fn data(
        &mut self,
        _channel: ChannelId,
        data: &[u8],
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        if let Some(tx) = &self.events_tx {
            self.input_buf.extend_from_slice(data);
            for event in input::drain(&mut self.input_buf) {
                let _ = tx.send(event).await;
            }
        }
        Ok(())
    }

    async fn window_change_request(
        &mut self,
        _channel: ChannelId,
        col_width: u32,
        row_height: u32,
        _pix_width: u32,
        _pix_height: u32,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        if let Some(tx) = &self.events_tx {
            let (w, h) = clamp_size(col_width, row_height);
            let _ = tx.send(Event::Resize(w, h)).await;
        }
        Ok(())
    }
}

/// Resize the ratatui terminal, best-effort.
///
/// ratatui's `resize` clears the viewport, which queries the *backend's*
/// terminal size (`crossterm::terminal::size()` — an ioctl on the server's own
/// stdout). When the server has no controlling tty (run as a daemon / stdout
/// redirected), that query fails. The buffers and viewport are already resized
/// before that call, so a failure is harmless for our full-redraw Fixed
/// viewport: we log it and carry on.
fn resize_terminal(terminal: &mut SshTerminal, w: u16, h: u16) {
    if let Err(e) = terminal.resize(Rect::new(0, 0, w, h)) {
        tracing::debug!("terminal resize was non-fatal: {e}");
    }
}

/// Clamp a client-reported terminal size to something drawable. Some clients
/// (and non-interactive PTYs) report 0x0; fall back to a conventional 80x24.
fn clamp_size(cols: u32, rows: u32) -> (u16, u16) {
    let w = if cols == 0 {
        80
    } else {
        cols.min(u16::MAX as u32) as u16
    };
    let h = if rows == 0 {
        24
    } else {
        rows.min(u16::MAX as u32) as u16
    };
    (w, h)
}

impl Drop for SessionHandler {
    fn drop(&mut self) {
        // Backstop presence cleanup; the app loop also deregisters on exit.
        let presence = self.presence.clone();
        let id = self.id;
        tokio::spawn(async move {
            presence.leave(id).await;
        });
    }
}

/// Load a persisted ed25519 host key, generating and saving one on first run.
///
/// We seed the keypair from the OS RNG via `getrandom` rather than
/// `PrivateKey::random`, sidestepping the several incompatible `rand_core`
/// versions in the dependency graph.
fn load_or_generate_host_key(path: &Path) -> anyhow::Result<russh::keys::PrivateKey> {
    use russh::keys::ssh_key::LineEnding;
    use russh::keys::ssh_key::private::Ed25519Keypair;

    if path.exists() {
        Ok(russh::keys::load_secret_key(path, None)?)
    } else {
        let mut seed = [0u8; 32];
        getrandom::fill(&mut seed).map_err(|e| anyhow::anyhow!("getrandom failed: {e}"))?;
        let key = russh::keys::PrivateKey::from(Ed25519Keypair::from_seed(&seed));
        key.write_openssh_file(path, LineEnding::LF)?;
        tracing::info!("generated new SSH host key at {}", path.display());
        Ok(key)
    }
}

/// How often the ban sweeper checks live sessions against the ban lists.
const BAN_SWEEP_INTERVAL: Duration = Duration::from_secs(10);

/// Periodically drop live sessions belonging to banned users or IPs. This is
/// how bans applied out-of-process (by `bbsctl`) reach active sessions; in-BBS
/// admin bans also kick immediately.
async fn ban_sweeper(pool: SqlitePool, presence: Presence) {
    let mut ticker = tokio::time::interval(BAN_SWEEP_INTERVAL);
    loop {
        ticker.tick().await;
        let (users, ips) =
            match tokio::try_join!(admin::banned_usernames(&pool), admin::banned_ips(&pool),) {
                Ok(pair) => pair,
                Err(e) => {
                    tracing::warn!("ban sweeper query failed: {e}");
                    continue;
                }
            };
        if users.is_empty() && ips.is_empty() {
            continue;
        }
        let kicked = presence.kick(&users, &ips).await;
        if kicked > 0 {
            tracing::info!("ban sweeper kicked {kicked} session(s)");
        }
    }
}

/// Bind and serve the SSH BBS until the process is stopped.
pub async fn run(cfg: &config::Config, pool: SqlitePool) -> anyhow::Result<()> {
    let presence = Presence::new();
    let key = load_or_generate_host_key(&cfg.host_key)?;

    tokio::spawn(ban_sweeper(pool.clone(), presence.clone()));

    let ssh_config = Config {
        inactivity_timeout: Some(Duration::from_secs(3600)),
        auth_rejection_time: Duration::from_secs(2),
        auth_rejection_time_initial: Some(Duration::from_secs(0)),
        keys: vec![key],
        nodelay: true,
        ..Default::default()
    };

    let mut server = BbsServer {
        pool,
        presence,
        next_id: 0,
    };
    server
        .run_on_address(Arc::new(ssh_config), (cfg.host.as_str(), cfg.port))
        .await?;
    Ok(())
}
