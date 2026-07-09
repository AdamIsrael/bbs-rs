//! Bridges ratatui's synchronous `Write` output to russh's async channel.
//!
//! `CrosstermBackend` writes ANSI bytes into `TerminalHandle`; those are
//! buffered and, on `flush`, pushed through an unbounded channel to a task that
//! calls `Handle::data(channel_id, ..)`. This decouples the sync `Write` impl
//! from russh's async send. Pattern follows russh's `ratatui_app` example.

use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use russh::ChannelId;
use russh::server::Handle;
use tokio::sync::mpsc::{UnboundedSender, unbounded_channel};

/// A ratatui terminal whose backend writes to an SSH channel.
pub type SshTerminal = Terminal<CrosstermBackend<TerminalHandle>>;

pub struct TerminalHandle {
    sender: UnboundedSender<Vec<u8>>,
    sink: Vec<u8>,
}

impl TerminalHandle {
    pub async fn start(handle: Handle, channel_id: ChannelId) -> Self {
        let (sender, mut receiver) = unbounded_channel::<Vec<u8>>();
        tokio::spawn(async move {
            while let Some(data) = receiver.recv().await {
                if handle.data(channel_id, data).await.is_err() {
                    // Client went away; stop draining.
                    break;
                }
            }
        });
        Self {
            sender,
            sink: Vec::new(),
        }
    }
}

impl std::io::Write for TerminalHandle {
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
