//! The seam that keeps the TUI transport-agnostic.
//!
//! The app loop never talks to russh or axum/WebSocket directly. It renders
//! through a `ratatui::Terminal<CrosstermBackend<W>>` — any `W: Write` byte
//! sink — and consumes decoded [`Event`]s from an `mpsc` channel. Each
//! transport is responsible for:
//!   1. providing a `Write` sink that ships bytes to the client, and
//!   2. decoding its raw input bytes into [`Event`]s (see [`crate::input`]).
//!
//! [`crate::ssh`] and [`crate::web`] both implement that contract, so the whole
//! `app` is shared between them. [`Transport`] is the one thing the app knows
//! about *how* a session arrived — enough to tell a user the other way in.

use crossterm::event::KeyEvent;

/// How a session reached the BBS.
///
/// The app is otherwise transport-agnostic; this exists so the UI can adapt the
/// few places where the connection method actually matters (e.g. pointing a
/// browser user at the SSH address, and vice-versa).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transport {
    /// An SSH session ([`crate::ssh`]).
    Ssh,
    /// A browser session over the WebSocket frontend ([`crate::web`]).
    Web,
}

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
    /// Another online user paged ("yelled at") this session (#68). Delivered
    /// through the presence fan-out and surfaced as a toast wherever the
    /// recipient currently is, regardless of their screen.
    Paged { from: String, body: String },
    /// A sysop broadcast to every live session (#69), e.g. a maintenance
    /// notice. Fanned out via [`crate::services::presence::Presence::broadcast`]
    /// and surfaced as a toast like [`Event::Paged`].
    Broadcast { text: String },
}
