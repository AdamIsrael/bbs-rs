//! A ratatui backend that knows the *client's* terminal size (#198).
//!
//! [`CrosstermBackend::size`] calls `crossterm::terminal::size()`, which asks
//! the **server process's own controlling terminal** how big it is. For a
//! remote session that is the wrong terminal entirely, and under systemd there
//! is no controlling tty at all, so the call simply fails.
//!
//! That is not cosmetic. ratatui's `Terminal::resize` asks the backend for its
//! size in order to clear the screen before the next frame; when the call
//! errors it skips both the clear *and* the back-buffer reset that would force
//! a full repaint. The viewport still updates, so drawing carries on looking
//! healthy — while remnants of the previous frame stay on the client's screen
//! wherever the new frame's diff doesn't happen to overwrite them.
//!
//! Both transports already learn the real size from the client (an SSH pty
//! request or window-change, a WebSocket resize message). This wrapper simply
//! remembers what they were told and answers with it, delegating everything
//! else to `CrosstermBackend`.

use ratatui::backend::{Backend, ClearType, CrosstermBackend, WindowSize};
use ratatui::buffer::Cell;
use ratatui::layout::{Position, Size};

/// Wraps [`CrosstermBackend`] so `size()` reports the connected client's
/// terminal rather than the server's.
pub struct RemoteBackend<W: std::io::Write> {
    inner: CrosstermBackend<W>,
    size: Size,
}

impl<W: std::io::Write> RemoteBackend<W> {
    /// Wrap `writer`, starting at the size the client reported at connect.
    pub fn new(writer: W, cols: u16, rows: u16) -> Self {
        Self {
            inner: CrosstermBackend::new(writer),
            size: Size::new(cols, rows),
        }
    }

    /// Record a new client size. Call this *before* `Terminal::resize`, so the
    /// clear-before-redraw inside it sees the size the client actually has.
    pub fn set_size(&mut self, cols: u16, rows: u16) {
        self.size = Size::new(cols, rows);
    }
}

impl<W: std::io::Write> Backend for RemoteBackend<W> {
    type Error = std::io::Error;

    fn draw<'a, I>(&mut self, content: I) -> Result<(), Self::Error>
    where
        I: Iterator<Item = (u16, u16, &'a Cell)>,
    {
        self.inner.draw(content)
    }

    fn hide_cursor(&mut self) -> Result<(), Self::Error> {
        self.inner.hide_cursor()
    }

    fn show_cursor(&mut self) -> Result<(), Self::Error> {
        self.inner.show_cursor()
    }

    fn get_cursor_position(&mut self) -> Result<Position, Self::Error> {
        self.inner.get_cursor_position()
    }

    fn set_cursor_position<P: Into<Position>>(&mut self, position: P) -> Result<(), Self::Error> {
        self.inner.set_cursor_position(position)
    }

    fn clear(&mut self) -> Result<(), Self::Error> {
        self.inner.clear()
    }

    fn clear_region(&mut self, clear_type: ClearType) -> Result<(), Self::Error> {
        self.inner.clear_region(clear_type)
    }

    fn append_lines(&mut self, n: u16) -> Result<(), Self::Error> {
        self.inner.append_lines(n)
    }

    /// The whole point of this wrapper: the client's size, not the server's.
    fn size(&self) -> Result<Size, Self::Error> {
        Ok(self.size)
    }

    /// Character cells only. Pixel dimensions would need an escape-sequence
    /// round-trip to the client, and nothing in the TUI uses them.
    fn window_size(&mut self) -> Result<WindowSize, Self::Error> {
        Ok(WindowSize {
            columns_rows: self.size,
            pixels: Size::new(0, 0),
        })
    }

    fn flush(&mut self) -> Result<(), Self::Error> {
        self.inner.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The regression guard for #198: the backend must answer with the size it
    /// was told, never by querying the local process's terminal.
    ///
    /// The ghosting itself lives in the *client's* screen versus ratatui's
    /// internal model, so no in-process assertion can see it — but this is the
    /// property the fix rests on, and it's the part that can drift.
    #[test]
    fn reports_the_size_it_was_given() {
        let mut backend = RemoteBackend::new(Vec::new(), 80, 24);
        assert_eq!(backend.size().unwrap(), Size::new(80, 24));

        backend.set_size(203, 51);
        assert_eq!(
            backend.size().unwrap(),
            Size::new(203, 51),
            "a resize must be visible to ratatui's clear-before-redraw"
        );
        assert_eq!(
            backend.window_size().unwrap().columns_rows,
            Size::new(203, 51)
        );
    }

    /// `size()` must not depend on the environment — this test runs without a
    /// controlling tty in CI, which is exactly the case that broke.
    #[test]
    fn works_without_a_controlling_terminal() {
        let backend = RemoteBackend::new(Vec::new(), 132, 43);
        assert_eq!(backend.size().unwrap(), Size::new(132, 43));
    }
}
