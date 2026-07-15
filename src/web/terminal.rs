//! The `Write` byte-sink for the web transport: ratatui's `CrosstermBackend`
//! writes ANSI bytes here; on `flush` they're pushed through a channel to the
//! task that forwards them as WebSocket binary frames. Mirrors the SSH
//! [`crate::ssh::terminal::TerminalHandle`].

use tokio::sync::mpsc::UnboundedSender;

pub struct WebTerminalHandle {
    sender: UnboundedSender<Vec<u8>>,
    sink: Vec<u8>,
}

impl WebTerminalHandle {
    pub fn new(sender: UnboundedSender<Vec<u8>>) -> Self {
        Self {
            sender,
            sink: Vec::new(),
        }
    }
}

impl std::io::Write for WebTerminalHandle {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.sink.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        let data = std::mem::take(&mut self.sink);
        self.sender
            .send(data)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::BrokenPipe, e.to_string()))
    }
}
