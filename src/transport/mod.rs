//! The seam that keeps the TUI transport-agnostic.
//!
//! The app loop never talks to russh (or, later, axum/WebSocket) directly. It
//! renders through a `ratatui::Terminal<CrosstermBackend<W>>` — any `W: Write`
//! byte sink — and consumes decoded [`Event`]s from an `mpsc` channel. Each
//! transport is responsible for:
//!   1. providing a `Write` sink that ships bytes to the client, and
//!   2. decoding its raw input bytes into [`Event`]s (see [`crate::input`]).
//!
//! A future `web` module implements the same contract over a WebSocket +
//! xterm.js, reusing the entire `app` unchanged.

use crossterm::event::KeyEvent;

/// A decoded client-side event fed into the app loop.
#[derive(Debug, Clone)]
pub enum Event {
    /// A key press decoded from the input byte stream.
    Key(KeyEvent),
    /// Terminal was resized to (cols, rows).
    Resize(u16, u16),
    /// The transport asked the session to end. (Part of the transport contract;
    /// SSH ends sessions by dropping the event channel, but a future WebSocket
    /// frontend can signal an explicit close here.)
    #[allow(dead_code)]
    Quit,
}
