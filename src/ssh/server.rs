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
use crate::services::auth;
use crate::services::presence::Presence;
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

    fn new_client(&mut self, _addr: Option<SocketAddr>) -> SessionHandler {
        self.next_id += 1;
        SessionHandler {
            pool: self.pool.clone(),
            presence: self.presence.clone(),
            id: self.next_id,
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
    user: Option<User>,
    /// Built at channel open, moved into the app task at shell request.
    terminal: Option<SshTerminal>,
    /// Feeds decoded input events to the running app loop.
    events_tx: Option<mpsc::Sender<Event>>,
    /// Carries incomplete escape/UTF-8 sequences between `data` callbacks.
    input_buf: Vec<u8>,
}

impl Handler for SessionHandler {
    type Error = anyhow::Error;

    async fn auth_password(&mut self, user: &str, password: &str) -> Result<Auth, Self::Error> {
        match auth::verify_login(&self.pool, user, password).await {
            Ok(Some(u)) => {
                self.user = Some(u);
                Ok(Auth::Accept)
            }
            Ok(None) => Ok(Auth::reject()),
            Err(e) => {
                tracing::warn!("auth lookup failed: {e}");
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
            terminal.resize(Rect::new(0, 0, w, h))?;
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
                self.events_tx = Some(tx);
                self.presence.join(self.id, user.username.clone()).await;

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

/// Bind and serve the SSH BBS until the process is stopped.
pub async fn run(cfg: &config::Config, pool: SqlitePool) -> anyhow::Result<()> {
    let presence = Presence::new();
    let key = load_or_generate_host_key(&cfg.host_key)?;

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
