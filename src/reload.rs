//! Hot-reload of the config file.
//!
//! The server holds its settings in an [`ArcSwap`] so they can be replaced
//! atomically at runtime. When `bbs.toml` changes on disk (a `notify` file
//! watcher) — or on `SIGHUP` — the file is re-parsed and swapped in. **New**
//! sessions snapshot the current settings at connect time, so they pick up the
//! change without a restart; existing sessions keep the settings they started
//! with.
//!
//! Some settings can't change without a restart because they're bound/opened
//! once at startup (the SSH/web listeners, host key, database, and one-time
//! seeding). A reload that touches those is applied to the shared settings but
//! logged as needing a restart to take effect.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use arc_swap::ArcSwap;

use crate::config::{Cli, Settings};

/// Start watching the config file and reloading on change. Spawns a `notify`
/// watcher and (on Unix) a `SIGHUP` handler; both funnel into [`reload`].
/// Watcher/handler setup failures are logged and downgrade to "no hot reload"
/// rather than stopping the server.
pub fn spawn(cli: Cli, config: Arc<ArcSwap<Settings>>) {
    spawn_file_watcher(cli.clone(), config.clone());
    #[cfg(unix)]
    spawn_sighup(cli, config);
}

fn spawn_file_watcher(cli: Cli, config: Arc<ArcSwap<Settings>>) {
    use notify::{RecursiveMode, Watcher};

    let path = cli.config.clone();
    // Watch the parent directory, not the file itself: editors often save via
    // write-temp-then-rename, which replaces the inode a file watch is bound to.
    let dir = match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
        _ => Path::new(".").to_path_buf(),
    };
    let want = path.file_name().map(|n| n.to_os_string());

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<()>();
    let watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        if let Ok(ev) = res {
            // Only react to events that mention our config file name.
            let hit = want
                .as_ref()
                .map(|w| ev.paths.iter().any(|p| p.file_name() == Some(w)))
                .unwrap_or(false);
            if hit {
                let _ = tx.send(());
            }
        }
    });
    let mut watcher = match watcher {
        Ok(w) => w,
        Err(e) => {
            tracing::warn!("config file watch unavailable ({e}); hot reload disabled");
            return;
        }
    };
    if let Err(e) = watcher.watch(&dir, RecursiveMode::NonRecursive) {
        tracing::warn!("cannot watch {} ({e}); hot reload disabled", dir.display());
        return;
    }

    tokio::spawn(async move {
        let _watcher = watcher; // keep the watcher alive for the task's lifetime
        while rx.recv().await.is_some() {
            // Debounce a burst of events (an editor may emit several per save).
            tokio::time::sleep(Duration::from_millis(300)).await;
            while rx.try_recv().is_ok() {}
            reload(&cli, &config);
        }
    });
}

#[cfg(unix)]
fn spawn_sighup(cli: Cli, config: Arc<ArcSwap<Settings>>) {
    use tokio::signal::unix::{SignalKind, signal};
    tokio::spawn(async move {
        let mut sig = match signal(SignalKind::hangup()) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!("cannot install SIGHUP handler ({e})");
                return;
            }
        };
        while sig.recv().await.is_some() {
            tracing::info!("SIGHUP received; reloading config");
            reload(&cli, &config);
        }
    });
}

/// Re-read the config file and swap it in. On a parse error the current settings
/// are kept unchanged.
fn reload(cli: &Cli, config: &ArcSwap<Settings>) {
    match Settings::load(cli) {
        Ok(new) => {
            let old = config.load();
            warn_restart_only(&old, &new);
            config.store(Arc::new(new));
            tracing::info!("reloaded config from {}", cli.config.display());
        }
        Err(e) => tracing::error!("config reload failed, keeping current settings: {e:#}"),
    }
}

/// Log a warning when a reload changed settings that only take effect after a
/// restart (the listeners, host key, database, and one-time seeding).
fn warn_restart_only(old: &Settings, new: &Settings) {
    let mut changed = Vec::new();
    if old.network != new.network {
        changed.push("[network]");
    }
    if old.web != new.web {
        changed.push("[web]");
    }
    if old.federation != new.federation {
        changed.push("[federation]");
    }
    if old.seed != new.seed {
        changed.push("[seed]");
    }
    if !changed.is_empty() {
        tracing::warn!(
            "config reloaded, but changes to {} take effect only after a restart",
            changed.join(", ")
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path, body: &str) {
        std::fs::write(path, body).unwrap();
    }

    #[test]
    fn reload_swaps_in_new_settings() {
        let dir = std::env::temp_dir().join(format!("bbs_reload_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("bbs.toml");
        write(&path, "[bbs]\nname = \"First\"\n");

        let cli = Cli {
            config: path.clone(),
            host: None,
            port: None,
            database_url: None,
            host_key: None,
            migrate: false,
        };
        let config = Arc::new(ArcSwap::from_pointee(Settings::load(&cli).unwrap()));
        assert_eq!(config.load().bbs.name, "First");

        // Edit the file and reload — the swap is visible to readers.
        write(&path, "[bbs]\nname = \"Second\"\n");
        reload(&cli, &config);
        assert_eq!(config.load().bbs.name, "Second");

        // A broken file keeps the current settings.
        write(&path, "this is not valid toml {{{");
        reload(&cli, &config);
        assert_eq!(config.load().bbs.name, "Second");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn restart_only_diff_detects_listener_changes() {
        let mut a = Settings::default();
        let mut b = a.clone();
        // A reloadable change (branding) is not flagged.
        b.bbs.name = "New".into();
        assert_eq!(a.network, b.network);
        // A restart-only change (port) differs in [network].
        a.network.port = 2222;
        b.network.port = 2323;
        assert_ne!(a.network, b.network);
    }
}
