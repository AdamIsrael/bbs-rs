//! `doorkit` — a tiny, BBS-agnostic toolkit for writing terminal "door" games.
//!
//! A door program talks to the calling BBS in two universal ways: a **drop
//! file** (`DOOR.SYS` / `DORINFO1.DEF`) describing the user, and **raw terminal
//! I/O** over stdin/stdout. `doorkit` handles both so a game can focus on being
//! a game:
//!
//! - [`Session`] resolves the user's name, node, security level, terminal size,
//!   and remaining time from a drop file in the working directory, with
//!   environment-variable overrides (bbs-rs sets `BBS_USER`, `BBS_NODE`,
//!   `BBS_TIME_LEFT_SECS`, `BBS_COLS`, `BBS_ROWS`, …).
//! - [`Terminal`] puts the tty in raw mode (restoring it on drop, so the BBS
//!   screen comes back cleanly), reads keys, and offers a few ANSI helpers.
//!
//! It has no dependency on any particular BBS, so a door built on it runs under
//! bbs-rs or any other BBS that speaks the drop-file + terminal contract.

mod session;
mod term;

pub use session::Session;
pub use term::{Color, Key, Terminal};
