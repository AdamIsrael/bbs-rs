//! Raw-terminal I/O: key input and ANSI output, with the tty restored on drop.

use std::io::{Stdout, Write, stdout};

use crossterm::event::{Event, KeyCode, KeyEventKind, read};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode};

/// A decoded key press.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Char(char),
    Enter,
    Esc,
    Backspace,
    Up,
    Down,
    Left,
    Right,
    Other,
}

/// A small palette of ANSI colors for [`Terminal::color`].
#[derive(Debug, Clone, Copy)]
pub enum Color {
    Black,
    Red,
    Green,
    Yellow,
    Blue,
    Magenta,
    Cyan,
    White,
    Reset,
}

impl Color {
    fn code(self) -> &'static str {
        match self {
            Color::Black => "30",
            Color::Red => "31",
            Color::Green => "32",
            Color::Yellow => "33",
            Color::Blue => "34",
            Color::Magenta => "35",
            Color::Cyan => "36",
            Color::White => "37",
            Color::Reset => "0",
        }
    }
}

/// A raw-mode terminal handle. Enables raw mode on creation and restores the
/// terminal (attributes, cursor, cooked mode) when dropped — so the calling BBS
/// screen returns cleanly whether the door exits normally or is interrupted.
pub struct Terminal {
    out: Stdout,
    raw: bool,
}

impl Terminal {
    /// Enter raw mode and hide the cursor.
    pub fn new() -> std::io::Result<Self> {
        enable_raw_mode()?;
        let mut t = Self {
            out: stdout(),
            raw: true,
        };
        let _ = t.write("\x1b[?25l"); // hide cursor
        Ok(t)
    }

    fn write(&mut self, s: &str) -> std::io::Result<()> {
        self.out.write_all(s.as_bytes())?;
        self.out.flush()
    }

    /// Clear the screen and home the cursor.
    pub fn clear(&mut self) -> std::io::Result<()> {
        self.write("\x1b[2J\x1b[H")
    }

    /// Print text as-is (may contain the caller's own ANSI).
    pub fn print(&mut self, s: &str) -> std::io::Result<()> {
        self.write(s)
    }

    /// Print text followed by CRLF (needed in raw mode).
    pub fn println(&mut self, s: &str) -> std::io::Result<()> {
        self.out.write_all(s.as_bytes())?;
        self.out.write_all(b"\r\n")?;
        self.out.flush()
    }

    /// Move the cursor to (1-based) row, col.
    pub fn goto(&mut self, row: u16, col: u16) -> std::io::Result<()> {
        self.write(&format!("\x1b[{row};{col}H"))
    }

    /// Set the foreground color.
    pub fn color(&mut self, c: Color) -> std::io::Result<()> {
        self.write(&format!("\x1b[{}m", c.code()))
    }

    /// Enable bold.
    pub fn bold(&mut self) -> std::io::Result<()> {
        self.write("\x1b[1m")
    }

    /// Reset all attributes.
    pub fn reset(&mut self) -> std::io::Result<()> {
        self.write("\x1b[0m")
    }

    /// Print `text` in `c` (bold), then reset — a common one-liner.
    pub fn say(&mut self, c: Color, text: &str) -> std::io::Result<()> {
        self.write(&format!("\x1b[1;{}m{text}\x1b[0m\r\n", c.code()))
    }

    /// Block until a key is pressed and return it.
    pub fn read_key(&mut self) -> std::io::Result<Key> {
        loop {
            if let Event::Key(k) = read()?
                && k.kind != KeyEventKind::Release
            {
                return Ok(map_key(k.code));
            }
        }
    }

    /// Wait for any key (e.g. "press any key to continue").
    pub fn pause(&mut self) -> std::io::Result<()> {
        self.read_key().map(|_| ())
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        let _ = self.out.write_all(b"\x1b[0m\x1b[?25h"); // reset attrs, show cursor
        let _ = self.out.flush();
        if self.raw {
            let _ = disable_raw_mode();
        }
    }
}

fn map_key(code: KeyCode) -> Key {
    match code {
        KeyCode::Char(c) => Key::Char(c),
        KeyCode::Enter => Key::Enter,
        KeyCode::Esc => Key::Esc,
        KeyCode::Backspace => Key::Backspace,
        KeyCode::Up => Key::Up,
        KeyCode::Down => Key::Down,
        KeyCode::Left => Key::Left,
        KeyCode::Right => Key::Right,
        _ => Key::Other,
    }
}
