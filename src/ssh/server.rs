//! russh server: one [`SessionHandler`] per connection, bridging SSH events to
//! the transport-agnostic [`app`] loop.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use arc_swap::ArcSwap;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::Rect;
use ratatui::{Terminal, TerminalOptions, Viewport};
use russh::keys::ssh_key::PublicKey;
use russh::server::{Auth, ChannelOpenHandle, Config, Handler, Msg, Server, Session};
use russh::{Channel, ChannelId, Pty};
use sqlx::sqlite::SqlitePool;
use tokio::sync::mpsc;

use crate::app::{self, App};
use crate::config::Settings;
use crate::db::models::User;
use crate::input;
use crate::services::presence::Presence;
use crate::services::{admin, auth, keys};
use crate::ssh::pubkey;
use crate::ssh::sftp::SftpSession;
use crate::ssh::terminal::{SshTerminal, TerminalHandle};
use crate::transport::{Event, Transport};

/// Shared server state; cloned into each per-connection handler.
#[derive(Clone)]
struct BbsServer {
    pool: SqlitePool,
    presence: Presence,
    /// Hot-swappable settings; each connection snapshots the current value.
    config: Arc<ArcSwap<Settings>>,
    /// Session-id source shared with the web frontend so ids never collide.
    next_id: Arc<AtomicUsize>,
}

impl Server for BbsServer {
    type Handler = SessionHandler;

    fn new_client(&mut self, addr: Option<SocketAddr>) -> SessionHandler {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        SessionHandler {
            pool: self.pool.clone(),
            presence: self.presence.clone(),
            // Snapshot the current settings for this session's lifetime.
            config: self.config.load_full(),
            id,
            peer: addr,
            user: None,
            terminal: None,
            raw_out: None,
            events_tx: None,
            input_buf: Vec::new(),
            channels: HashMap::new(),
        }
    }
}

/// Per-connection handler.
struct SessionHandler {
    pool: SqlitePool,
    presence: Presence,
    config: Arc<Settings>,
    id: usize,
    /// The client's address, captured at connection for login logging and IP bans.
    peer: Option<SocketAddr>,
    user: Option<User>,
    /// Built at channel open, moved into the app task at shell request.
    terminal: Option<SshTerminal>,
    /// Raw byte sink to the client (bypasses ratatui); used to bridge doors.
    raw_out: Option<mpsc::UnboundedSender<Vec<u8>>>,
    /// Feeds decoded input events to the running app loop.
    events_tx: Option<mpsc::Sender<Event>>,
    /// Carries incomplete escape/UTF-8 sequences between `data` callbacks.
    input_buf: Vec<u8>,
    /// Open channels held so an SFTP subsystem request can take the raw stream.
    /// Dropped on `shell_request` (the TUI uses the `data`/handle callback path,
    /// and holding a channel would fill its buffer and stall input).
    channels: HashMap<ChannelId, Channel<Msg>>,
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
        // The guest account can be disabled by config; record the rejected
        // attempt for the audit trail and refuse.
        if user == "guest" && !self.config.features.guest {
            let _ = admin::record_login(&self.pool, user, ip.as_deref(), false).await;
            return Ok(Auth::reject());
        }
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

    /// Pre-signature probe: accept only keys already registered to `user`, so a
    /// client isn't prompted to sign with keys we'd reject anyway. Ownership is
    /// still proven later in [`Self::auth_publickey`].
    async fn auth_publickey_offered(
        &mut self,
        user: &str,
        key: &PublicKey,
    ) -> Result<Auth, Self::Error> {
        if !self.config.features.pubkey_auth {
            return Ok(Auth::reject());
        }
        let fpr = pubkey::fingerprint(key);
        match keys::is_authorized(&self.pool, user, &fpr).await {
            Ok(true) => Ok(Auth::Accept),
            _ => Ok(Auth::reject()),
        }
    }

    /// Called after russh has verified the client owns `key` (signature check).
    /// Authenticate iff the key is registered to `user` and the account/IP is
    /// not banned.
    async fn auth_publickey(&mut self, user: &str, key: &PublicKey) -> Result<Auth, Self::Error> {
        if !self.config.features.pubkey_auth {
            return Ok(Auth::reject());
        }
        let ip = self.ip();
        let fpr = pubkey::fingerprint(key);
        match auth::attempt_pubkey_login(&self.pool, user, &fpr, ip.as_deref()).await {
            Ok(Some(u)) => {
                self.user = Some(u);
                Ok(Auth::Accept)
            }
            Ok(None) => Ok(Auth::reject()),
            Err(e) => {
                tracing::warn!("pubkey auth failed for {user:?}: {e}");
                Ok(Auth::reject())
            }
        }
    }

    async fn channel_open_session(
        &mut self,
        channel: Channel<Msg>,
        reply: ChannelOpenHandle,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        let id = channel.id();
        let handle = TerminalHandle::start(session.handle(), id).await;
        self.raw_out = Some(handle.raw_sender());
        let backend = CrosstermBackend::new(handle);
        // Correct size is applied on the pty request.
        let options = TerminalOptions {
            viewport: Viewport::Fixed(Rect::default()),
        };
        self.terminal = Some(Terminal::with_options(backend, options)?);
        // Keep the channel so an SFTP subsystem request can take its stream; a
        // `shell_request` drops it and uses the callback path instead.
        self.channels.insert(id, channel);
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
        let (w, h) = clamp_size(col_width, row_height, &self.config);
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
        // The TUI uses the `data`/handle callback path; drop the stored channel
        // so russh doesn't also buffer input into it (which would stall).
        self.channels.remove(&channel);
        match (self.terminal.take(), self.raw_out.take(), self.user.clone()) {
            (Some(terminal), Some(raw_out), Some(user)) => {
                let (tx, rx) = mpsc::channel::<Event>(64);
                self.events_tx = Some(tx.clone());
                self.presence
                    .join(self.id, user.username.clone(), self.ip(), tx)
                    .await;

                let app = App::new(
                    self.pool.clone(),
                    self.presence.clone(),
                    self.config.clone(),
                    user,
                    self.id,
                    Transport::Ssh,
                );
                let id = self.id;
                // The app loop is transport-agnostic; when it returns (user quit
                // or disconnect), close the SSH channel here so the client exits.
                let handle = session.handle();
                tokio::spawn(async move {
                    if let Err(e) = app::run(app, terminal, rx, raw_out).await {
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

    /// Serve the `sftp` subsystem over the channel's raw stream, mapping file
    /// areas to a small virtual filesystem (see [`SftpSession`]). Other
    /// subsystems, or sftp when file areas are disabled, are refused.
    async fn subsystem_request(
        &mut self,
        channel: ChannelId,
        name: &str,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        let is_sftp = name == "sftp" && self.config.features.file_areas;
        match (is_sftp, self.channels.remove(&channel), self.user.clone()) {
            (true, Some(chan), Some(user)) => {
                let sftp = SftpSession::new(self.pool.clone(), self.config.clone(), user);
                session.channel_success(channel)?;
                russh_sftp::server::run(chan.into_stream(), sftp).await;
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
            let (w, h) = clamp_size(col_width, row_height, &self.config);
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
/// (and non-interactive PTYs) report 0x0; fall back to the configured default.
fn clamp_size(cols: u32, rows: u32, config: &Settings) -> (u16, u16) {
    let w = if cols == 0 {
        config.network.default_cols
    } else {
        cols.min(u16::MAX as u32) as u16
    };
    let h = if rows == 0 {
        config.network.default_rows
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

/// Periodic maintenance loop: expire old IP bans, auto-ban IPs with repeated
/// failed logins, then drop live sessions belonging to banned users or IPs.
/// This is also how bans applied out-of-process (by `bbsctl`) reach active
/// sessions; in-BBS admin bans also kick immediately.
async fn ban_sweeper(pool: SqlitePool, presence: Presence, config: Arc<ArcSwap<Settings>>) {
    // The sweep interval is fixed at startup; the abuse policy is re-read each
    // tick, so tightening/loosening it hot-reloads without a restart.
    let mut ticker = tokio::time::interval(config.load().network.ban_sweep_interval());
    // High-water mark for sysop broadcasts (#69): seed with the current max so
    // messages queued before this process started aren't replayed.
    let mut last_broadcast = admin::latest_broadcast_id(&pool).await.unwrap_or(0);
    loop {
        ticker.tick().await;

        // Deliver any broadcasts queued out-of-process (by `bbsctl`) since the
        // last tick. Done before the ban logic's early-continue so a broadcast
        // still goes out on a tick with no bans to sweep.
        match admin::broadcasts_after(&pool, last_broadcast).await {
            Ok(pending) => {
                for (id, text) in pending {
                    let n = presence.broadcast(Event::Broadcast { text }).await;
                    tracing::info!("broadcast #{id} delivered to {n} session(s)");
                    last_broadcast = id;
                }
            }
            Err(e) => tracing::warn!("broadcast poll failed: {e}"),
        }

        let _ = admin::purge_expired_ip_bans(&pool).await;
        auto_ban(&pool, &config.load().abuse).await;

        let (users, ips) =
            match tokio::try_join!(admin::banned_usernames(&pool), admin::banned_ips(&pool)) {
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

/// Ban IPs that crossed the failed-login threshold within the window.
async fn auto_ban(pool: &SqlitePool, abuse: &crate::config::Abuse) {
    if !abuse.enabled() {
        return;
    }
    let now = crate::util::now_unix();
    let since = now - abuse.window_secs;
    let offenders =
        match admin::ips_over_failure_threshold(pool, abuse.max_failures as i64, since).await {
            Ok(ips) => ips,
            Err(e) => {
                tracing::warn!("auto-ban query failed: {e}");
                return;
            }
        };
    for ip in offenders {
        let expires = (abuse.ban_secs > 0).then_some(now + abuse.ban_secs);
        let reason = format!(
            "auto: {}+ failed logins in {}s",
            abuse.max_failures, abuse.window_secs
        );
        match admin::ban_ip(pool, &ip, &reason, expires).await {
            Ok(()) => tracing::warn!(
                "auto-banned {ip} ({}+ failed logins in {}s)",
                abuse.max_failures,
                abuse.window_secs
            ),
            Err(e) => tracing::warn!("auto-ban of {ip} failed: {e}"),
        }
    }
}

/// The SSH auth methods to advertise — only the ones we actually accept, never
/// russh's default `MethodSet::all()`.
///
/// Advertising methods we don't implement (hostbased, keyboard-interactive, or
/// publickey when it's disabled) makes clients offer credentials we'd reject;
/// russh delays each rejection by `auth_rejection_time`, so a full ssh-agent
/// means 30–60s of dead air before the password prompt — a login that looks
/// hung but isn't. Publickey is advertised only when `features.pubkey_auth` is
/// on (and even then a client only offers its own keys).
fn advertised_methods(config: &Settings) -> russh::MethodSet {
    let mut methods = russh::MethodSet::empty();
    methods.push(russh::MethodKind::Password);
    if config.features.pubkey_auth {
        methods.push(russh::MethodKind::PublicKey);
    }
    methods
}

/// Bind and serve the SSH BBS until the process is stopped. `presence` and
/// `next_id` are shared with the (optional) web frontend.
pub async fn run(
    config: Arc<ArcSwap<Settings>>,
    pool: SqlitePool,
    presence: Presence,
    next_id: Arc<AtomicUsize>,
) -> anyhow::Result<()> {
    // The SSH listener, host key, advertised methods, and timeouts are bound
    // once from this boot snapshot; changing them needs a restart (the reload
    // task warns when they change). Per-session settings are snapshotted fresh
    // in `new_client`, so they hot-reload.
    let boot = config.load_full();
    let net = &boot.network;
    let key = load_or_generate_host_key(&net.host_key)?;

    tokio::spawn(ban_sweeper(pool.clone(), presence.clone(), config.clone()));

    let ssh_config = Config {
        methods: advertised_methods(&boot),
        inactivity_timeout: Some(net.inactivity_timeout()),
        auth_rejection_time: net.auth_rejection_time(),
        auth_rejection_time_initial: Some(Duration::from_secs(0)),
        keys: vec![key],
        nodelay: true,
        ..Default::default()
    };

    let addr = (net.host.clone(), net.port);
    let mut server = BbsServer {
        pool,
        presence,
        config,
        next_id,
    };
    server.run_on_address(Arc::new(ssh_config), addr).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn never_advertises_methods_we_dont_implement() {
        // Regression guard for the "hung login" bug: we must not advertise auth
        // methods we don't accept, or clients waste 30–60s offering keys.
        for pubkey in [true, false] {
            let mut cfg = Settings::default();
            cfg.features.pubkey_auth = pubkey;
            let m = advertised_methods(&cfg);
            assert!(m.contains(&russh::MethodKind::Password));
            assert!(!m.contains(&russh::MethodKind::KeyboardInteractive));
            assert!(!m.contains(&russh::MethodKind::HostBased));
            assert!(!m.contains(&russh::MethodKind::None));
            // Publickey is advertised iff the feature is enabled.
            assert_eq!(m.contains(&russh::MethodKind::PublicKey), pubkey);
        }
    }
}
