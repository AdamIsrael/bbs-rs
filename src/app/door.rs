//! Launch an external "door" program on a pseudo-terminal and bridge its raw
//! I/O to the client for the duration of the session.
//!
//! The TUI is suspended while a door runs. The program gets a real PTY (so
//! full-screen ANSI / `isatty()` work), the user's info in the environment (and
//! an optional classic drop file), and an optional wall-clock time limit. The
//! client's decoded key events are re-encoded to bytes for the program's input
//! — the normal input path is untouched — and the program's output is written
//! straight to the client, bypassing ratatui.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use portable_pty::{CommandBuilder, PtySize, native_pty_system};
use tokio::sync::mpsc::{Receiver, UnboundedSender, unbounded_channel};

use crate::config::Door;
use crate::db::models::User;
use crate::transport::Event;

/// How a door session ended.
#[derive(Debug, PartialEq, Eq)]
pub enum DoorExit {
    /// The program ran and exited (or timed out); return to the BBS.
    Returned,
    /// The client asked to disconnect while in the door.
    Quit,
    /// The program could not be launched; the message is shown in the BBS
    /// status bar (the screen is repainted on return, which would otherwise
    /// wipe anything the door wrote to the client).
    Failed(String),
}

/// Run `door` on a PTY, bridging bytes to/from the client until it exits, the
/// time limit elapses, or the client disconnects.
#[allow(clippy::too_many_arguments)]
pub async fn run(
    door: &Door,
    user: &User,
    session_id: usize,
    size: (u16, u16),
    bbs_name: &str,
    sysop: &str,
    raw_out: &UnboundedSender<Vec<u8>>,
    events: &mut Receiver<Event>,
) -> DoorExit {
    let (cols, rows) = size;
    let say = |s: &str| {
        let _ = raw_out.send(s.as_bytes().to_vec());
    };

    let cwd = door.cwd.clone().unwrap_or_else(|| PathBuf::from("."));
    // Ensure the working directory exists so the program can be spawned there
    // and the drop file written; a configured but missing dir is created rather
    // than failing the launch.
    if let Err(e) = std::fs::create_dir_all(&cwd) {
        let msg = format!(
            "Cannot prepare working dir {} for door {:?}: {e}",
            cwd.display(),
            door.name
        );
        tracing::warn!("{msg}");
        return DoorExit::Failed(msg);
    }
    let drop_path = write_drop_file(door, user, bbs_name, sysop, &cwd);

    // Open a PTY sized to the client's terminal.
    let pair = match native_pty_system().openpty(PtySize {
        rows,
        cols,
        pixel_width: 0,
        pixel_height: 0,
    }) {
        Ok(p) => p,
        Err(e) => {
            cleanup_drop(&drop_path);
            let msg = format!("Cannot open a terminal for door {:?}: {e}", door.name);
            tracing::warn!("{msg}");
            return DoorExit::Failed(msg);
        }
    };

    let mut cmd = CommandBuilder::new(&door.command);
    for a in &door.args {
        cmd.arg(a);
    }
    cmd.cwd(&cwd);
    cmd.env("TERM", "xterm-256color");
    cmd.env("BBS_USER", &user.username);
    cmd.env("BBS_USER_ROLE", &user.role);
    cmd.env("BBS_NODE", session_id.to_string());
    cmd.env("BBS_COLS", cols.to_string());
    cmd.env("BBS_ROWS", rows.to_string());
    cmd.env("BBS_TIME_LEFT_SECS", door.time_limit_secs.to_string());

    let mut child = match pair.slave.spawn_command(cmd) {
        Ok(c) => c,
        Err(e) => {
            cleanup_drop(&drop_path);
            let msg = format!("Cannot launch door {:?} ({}): {e}", door.name, door.command);
            tracing::warn!("{msg}");
            return DoorExit::Failed(msg);
        }
    };
    drop(pair.slave); // close our slave handle so master sees EOF on child exit

    let mut reader = pair.master.try_clone_reader().expect("pty reader");
    let mut writer = pair.master.take_writer().expect("pty writer");
    let master = pair.master; // kept for resize
    let mut killer = child.clone_killer();

    // PTY output -> tokio channel (dropping the sender signals EOF/exit).
    let (out_tx, mut out_rx) = unbounded_channel::<Vec<u8>>();
    std::thread::spawn(move || {
        let mut buf = [0u8; 8192];
        while let Ok(n) = reader.read(&mut buf) {
            if n == 0 || out_tx.send(buf[..n].to_vec()).is_err() {
                break;
            }
        }
    });
    // Client input -> PTY.
    let (in_tx, in_rx) = std::sync::mpsc::channel::<Vec<u8>>();
    std::thread::spawn(move || {
        for bytes in in_rx {
            if writer.write_all(&bytes).is_err() || writer.flush().is_err() {
                break;
            }
        }
    });
    // Reap the child so it doesn't linger as a zombie.
    std::thread::spawn(move || {
        let _ = child.wait();
    });

    let deadline = (door.time_limit_secs > 0)
        .then(|| Instant::now() + Duration::from_secs(door.time_limit_secs));
    let mut quit = false;
    let mut timed_out = false;

    loop {
        let timer = async {
            match deadline {
                Some(d) => tokio::time::sleep_until(tokio::time::Instant::from_std(d)).await,
                None => std::future::pending::<()>().await,
            }
        };
        tokio::select! {
            biased;
            chunk = out_rx.recv() => match chunk {
                Some(bytes) => { let _ = raw_out.send(bytes); }
                None => break, // program exited
            },
            ev = events.recv() => match ev {
                Some(Event::Key(k)) => {
                    let b = encode_key(k);
                    if !b.is_empty() {
                        let _ = in_tx.send(b);
                    }
                }
                Some(Event::Resize(w, h)) => {
                    let _ = master.resize(PtySize { rows: h, cols: w, pixel_width: 0, pixel_height: 0 });
                }
                Some(Event::Quit) | None => {
                    quit = true;
                    break;
                }
            },
            _ = timer => {
                timed_out = true;
                break;
            }
        }
    }

    let _ = killer.kill();
    // Flush any output already buffered.
    while let Ok(bytes) = out_rx.try_recv() {
        let _ = raw_out.send(bytes);
    }
    cleanup_drop(&drop_path);
    if timed_out {
        say("\r\n\x1b[33m*** Your time in the door is up. ***\x1b[0m\r\n");
    }

    if quit {
        DoorExit::Quit
    } else {
        DoorExit::Returned
    }
}

/// Re-encode a decoded key event back into the terminal bytes a program expects
/// on stdin (xterm-style). Unhandled keys produce no bytes.
fn encode_key(k: KeyEvent) -> Vec<u8> {
    let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
    match k.code {
        KeyCode::Char(c) => {
            if ctrl {
                let up = c.to_ascii_uppercase();
                if up.is_ascii_uppercase() {
                    vec![up as u8 - b'A' + 1] // Ctrl-A..Ctrl-Z -> 1..26
                } else {
                    match c {
                        ' ' => vec![0],
                        _ => c.to_string().into_bytes(),
                    }
                }
            } else {
                c.to_string().into_bytes()
            }
        }
        KeyCode::Enter => vec![b'\r'],
        KeyCode::Backspace => vec![0x7f],
        KeyCode::Tab => vec![b'\t'],
        KeyCode::BackTab => b"\x1b[Z".to_vec(),
        KeyCode::Esc => vec![0x1b],
        KeyCode::Up => b"\x1b[A".to_vec(),
        KeyCode::Down => b"\x1b[B".to_vec(),
        KeyCode::Right => b"\x1b[C".to_vec(),
        KeyCode::Left => b"\x1b[D".to_vec(),
        KeyCode::Home => b"\x1b[H".to_vec(),
        KeyCode::End => b"\x1b[F".to_vec(),
        KeyCode::PageUp => b"\x1b[5~".to_vec(),
        KeyCode::PageDown => b"\x1b[6~".to_vec(),
        KeyCode::Insert => b"\x1b[2~".to_vec(),
        KeyCode::Delete => b"\x1b[3~".to_vec(),
        _ => vec![],
    }
}

/// Write the configured drop file into `cwd`, returning its path (to clean up
/// afterward). Best effort — a failure is logged and skipped.
fn write_drop_file(
    door: &Door,
    user: &User,
    bbs_name: &str,
    sysop: &str,
    cwd: &Path,
) -> Option<PathBuf> {
    let kind = door.drop_file.as_deref()?.trim().to_ascii_lowercase();
    if kind.is_empty() {
        return None;
    }
    let minutes = if door.time_limit_secs > 0 {
        (door.time_limit_secs / 60).max(1)
    } else {
        999
    };
    let sec_level = if user.is_admin() { 100 } else { 30 };
    let (fname, body) = match kind.as_str() {
        "door.sys" => ("DOOR.SYS", door_sys(user, bbs_name, minutes, sec_level)),
        "dorinfo1.def" | "dorinfo.def" => (
            "DORINFO1.DEF",
            dorinfo1(user, bbs_name, sysop, minutes, sec_level),
        ),
        other => {
            tracing::warn!(
                "door {:?}: unknown drop_file {other:?}; skipping",
                door.name
            );
            return None;
        }
    };
    let path = cwd.join(fname);
    match std::fs::write(&path, body) {
        Ok(()) => Some(path),
        Err(e) => {
            tracing::warn!("door {:?}: cannot write {}: {e}", door.name, path.display());
            None
        }
    }
}

fn cleanup_drop(path: &Option<PathBuf>) {
    if let Some(p) = path {
        let _ = std::fs::remove_file(p);
    }
}

/// A classic RemoteAccess/QuickBBS `DORINFO1.DEF` drop file.
fn dorinfo1(user: &User, bbs_name: &str, sysop: &str, minutes: u64, sec: u32) -> String {
    let (sys_first, sys_last) = split_name(if sysop.is_empty() { "Sysop" } else { sysop });
    let (first, last) = split_name(&user.username);
    format!(
        "{bbs_name}\n{sys_first}\n{sys_last}\nCOM0\n0 BAUD\n0\n{first}\n{last}\n\n1\n{sec}\n{minutes}\n-1\n"
    )
}

/// A best-effort `DOOR.SYS` drop file (52 lines). Fields most doors read are
/// populated; the rest carry sensible local-terminal defaults.
fn door_sys(user: &User, _bbs_name: &str, minutes: u64, sec: u32) -> String {
    let name = &user.username;
    let seconds = minutes * 60;
    let lines: Vec<String> = vec![
        "COM0:".into(),      // 1  comm port (0 = local)
        "0".into(),          // 2  baud
        "8".into(),          // 3  data bits
        "1".into(),          // 4  node number
        "0".into(),          // 5  DTE rate
        "Y".into(),          // 6  screen display
        "Y".into(),          // 7  printer toggle
        "N".into(),          // 8  page bell
        "N".into(),          // 9  caller alarm
        name.clone(),        // 10 user full name
        "".into(),           // 11 location
        "".into(),           // 12 home phone
        "".into(),           // 13 work phone
        "".into(),           // 14 password
        sec.to_string(),     // 15 security level
        "1".into(),          // 16 total times on
        "01/01/00".into(),   // 17 last date called
        seconds.to_string(), // 18 seconds remaining this call
        minutes.to_string(), // 19 minutes remaining this call
        "GR".into(),         // 20 graphics mode (GR = ANSI)
        "24".into(),         // 21 page length
        "N".into(),          // 22 expert mode
        "".into(),           // 23 conferences registered in
        "".into(),           // 24 conference exited to door from
        "01/01/99".into(),   // 25 expiration date
        "1".into(),          // 26 user record number
        "Z".into(),          // 27 default protocol
        "0".into(),          // 28 total uploads
        "0".into(),          // 29 total downloads
        "0".into(),          // 30 daily download K
        "0".into(),          // 31 daily download max K
        "01/01/00".into(),   // 32 birthday
        "C:\\".into(),       // 33 path to main files
        "C:\\".into(),       // 34 path to gen files
        "Sysop".into(),      // 35 sysop's name
        name.clone(),        // 36 alias/handle
        "00:00".into(),      // 37 event time
        "Y".into(),          // 38 error-correcting connection
        "Y".into(),          // 39 ANSI supported
        "Y".into(),          // 40 use record locking
        "7".into(),          // 41 default color
        minutes.to_string(), // 42 time credits in minutes
        "01/01/00".into(),   // 43 last new-files date
        seconds.to_string(), // 44 time online today (secs)
        "0".into(),          // 45 uploaded K today
        "0".into(),          // 46 downloaded K today
        "N".into(),          // 47 comment
        "0".into(),          // 48 total doors opened
        "0".into(),          // 49 total messages left
        "".into(),           // 50 spare
        "".into(),           // 51 spare
        "".into(),           // 52 spare
    ];
    let mut s = lines.join("\r\n");
    s.push_str("\r\n");
    s
}

/// Split a single name into (first, last) for drop-file fields.
fn split_name(name: &str) -> (String, String) {
    match name.split_once([' ', '.', '_']) {
        Some((a, b)) => (a.to_string(), b.to_string()),
        None => (name.to_string(), String::new()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyEventKind;

    fn key(code: KeyCode, ctrl: bool) -> KeyEvent {
        KeyEvent {
            code,
            modifiers: if ctrl {
                KeyModifiers::CONTROL
            } else {
                KeyModifiers::NONE
            },
            kind: KeyEventKind::Press,
            state: crossterm::event::KeyEventState::NONE,
        }
    }

    #[test]
    fn encodes_common_keys() {
        assert_eq!(encode_key(key(KeyCode::Char('a'), false)), b"a");
        assert_eq!(encode_key(key(KeyCode::Enter, false)), b"\r");
        assert_eq!(encode_key(key(KeyCode::Backspace, false)), vec![0x7f]);
        assert_eq!(encode_key(key(KeyCode::Up, false)), b"\x1b[A");
        assert_eq!(encode_key(key(KeyCode::Esc, false)), vec![0x1b]);
        // Ctrl-C -> 0x03, so a door (not the BBS) receives it.
        assert_eq!(encode_key(key(KeyCode::Char('c'), true)), vec![0x03]);
    }

    #[test]
    fn dorinfo1_has_user_and_time() {
        let user = User {
            id: 1,
            username: "alice".into(),
            password_hash: String::new(),
            role: "user".into(),
            created_at: 0,
            banned_at: None,
        };
        let body = dorinfo1(&user, "My BBS", "Jane Sysop", 30, 30);
        let lines: Vec<&str> = body.lines().collect();
        assert_eq!(lines[0], "My BBS");
        assert_eq!(lines[1], "Jane"); // sysop first
        assert_eq!(lines[6], "alice"); // user first name
        assert_eq!(lines[11], "30"); // minutes left
    }
}
