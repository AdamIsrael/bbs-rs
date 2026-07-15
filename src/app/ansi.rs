//! Render operator-supplied ANSI/text art into ratatui [`Text`].
//!
//! Handles the two things BBS art files are in practice: modern UTF-8 text with
//! ANSI SGR color escapes, and classic CP437-encoded `.ans` files. Bytes are
//! decoded (UTF-8 if valid, else CP437), then a small parser turns `ESC[…m`
//! color runs into styled spans. Non-color escape sequences (cursor moves) are
//! skipped, so static art renders correctly and animations degrade gracefully.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};

/// Unicode for CP437 bytes 0x80–0xFF (the box-drawing / accented glyph half).
const CP437_HIGH: &str = "\
ÇüéâäàåçêëèïîìÄÅÉæÆôöòûùÿÖÜ¢£¥₧ƒáíóúñÑªº¿⌐¬½¼¡«»\
░▒▓│┤╡╢╖╕╣║╗╝╜╛┐└┴┬├─┼╞╟╚╔╩╦╠═╬╧╨╤╥╙╘╒╓╫╪┘┌█▄▌▐▀\
αßΓπΣσµτΦΘΩδ∞φε∩≡±≥≤⌠⌡÷≈°∙·√ⁿ²■ ";

/// Decode raw art bytes to a `String`: valid UTF-8 is kept as-is; otherwise
/// each byte is mapped through CP437 (low half is ASCII).
fn decode(bytes: &[u8]) -> String {
    if let Ok(s) = std::str::from_utf8(bytes) {
        return s.to_string();
    }
    let high: Vec<char> = CP437_HIGH.chars().collect();
    bytes
        .iter()
        .map(|&b| {
            if b < 0x80 {
                b as char
            } else {
                high.get((b - 0x80) as usize).copied().unwrap_or('?')
            }
        })
        .collect()
}

const STD: [Color; 8] = [
    Color::Black,
    Color::Red,
    Color::Green,
    Color::Yellow,
    Color::Blue,
    Color::Magenta,
    Color::Cyan,
    Color::Gray,
];
const BRIGHT: [Color; 8] = [
    Color::DarkGray,
    Color::LightRed,
    Color::LightGreen,
    Color::LightYellow,
    Color::LightBlue,
    Color::LightMagenta,
    Color::LightCyan,
    Color::White,
];

/// Apply one `ESC[…m` SGR sequence (`params` = the text between `[` and `m`).
fn apply_sgr(params: &str, style: &mut Style) {
    let codes: Vec<i64> = params.split(';').map(|p| p.parse().unwrap_or(0)).collect();
    let mut i = 0;
    while i < codes.len() {
        match codes[i] {
            0 => *style = Style::default(),
            1 => *style = style.add_modifier(Modifier::BOLD),
            22 => *style = style.remove_modifier(Modifier::BOLD),
            n @ 30..=37 => *style = style.fg(STD[(n - 30) as usize]),
            n @ 90..=97 => *style = style.fg(BRIGHT[(n - 90) as usize]),
            39 => *style = style.fg(Color::Reset),
            n @ 40..=47 => *style = style.bg(STD[(n - 40) as usize]),
            n @ 100..=107 => *style = style.bg(BRIGHT[(n - 100) as usize]),
            49 => *style = style.bg(Color::Reset),
            sel @ (38 | 48) => {
                // Extended color: 5;N (indexed) or 2;R;G;B (truecolor).
                let color = match codes.get(i + 1) {
                    Some(5) => codes.get(i + 2).map(|&n| {
                        i += 2;
                        Color::Indexed(n as u8)
                    }),
                    Some(2) if i + 4 < codes.len() => {
                        let c =
                            Color::Rgb(codes[i + 2] as u8, codes[i + 3] as u8, codes[i + 4] as u8);
                        i += 4;
                        Some(c)
                    }
                    _ => None,
                };
                if let Some(c) = color {
                    *style = if sel == 38 { style.fg(c) } else { style.bg(c) };
                }
            }
            _ => {}
        }
        i += 1;
    }
}

/// Convert ANSI/text art bytes into styled ratatui [`Text`]. Each output line
/// corresponds to a source line; color runs become spans.
pub fn to_text(bytes: &[u8]) -> Text<'static> {
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut buf = String::new();
    let mut buf_style = Style::default();
    let mut style = Style::default();

    let mut chars = decode(bytes)
        .chars()
        .collect::<Vec<_>>()
        .into_iter()
        .peekable();
    while let Some(c) = chars.next() {
        match c {
            '\n' => {
                flush(&mut buf, &mut buf_style, &mut spans);
                lines.push(Line::from(std::mem::take(&mut spans)));
            }
            '\r' => {}
            '\t' => {
                for _ in 0..4 {
                    push(' ', style, &mut buf, &mut buf_style, &mut spans);
                }
            }
            '\x1b' => {
                // Only CSI (`ESC[`) sequences are understood; consume the rest.
                if chars.peek() == Some(&'[') {
                    chars.next();
                    let mut params = String::new();
                    let mut final_byte = None;
                    while let Some(&pc) = chars.peek() {
                        if ('\u{20}'..='\u{3f}').contains(&pc) {
                            params.push(pc);
                            chars.next();
                        } else if ('\u{40}'..='\u{7e}').contains(&pc) {
                            final_byte = Some(pc);
                            chars.next();
                            break;
                        } else {
                            break; // malformed; give up on this sequence
                        }
                    }
                    if final_byte == Some('m') {
                        apply_sgr(&params, &mut style);
                    }
                }
            }
            c if (c as u32) < 0x20 => {} // drop other control bytes
            c => push(c, style, &mut buf, &mut buf_style, &mut spans),
        }
    }
    flush(&mut buf, &mut buf_style, &mut spans);
    if !spans.is_empty() {
        lines.push(Line::from(spans));
    }
    Text::from(lines)
}

/// Append `c` under the current style, coalescing runs of the same style.
fn push(
    c: char,
    cur: Style,
    buf: &mut String,
    buf_style: &mut Style,
    spans: &mut Vec<Span<'static>>,
) {
    if buf.is_empty() {
        *buf_style = cur;
    } else if *buf_style != cur {
        spans.push(Span::styled(std::mem::take(buf), *buf_style));
        *buf_style = cur;
    }
    buf.push(c);
}

/// Flush any buffered run into a span.
fn flush(buf: &mut String, buf_style: &mut Style, spans: &mut Vec<Span<'static>>) {
    if !buf.is_empty() {
        spans.push(Span::styled(std::mem::take(buf), *buf_style));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_text_lines() {
        let t = to_text(b"hello\nworld");
        assert_eq!(t.lines.len(), 2);
        assert_eq!(t.lines[0].spans[0].content, "hello");
    }

    #[test]
    fn sgr_sets_color_runs() {
        // red "hi", reset, plain " there"
        let t = to_text(b"\x1b[31mhi\x1b[0m there");
        let spans = &t.lines[0].spans;
        assert_eq!(spans[0].content, "hi");
        assert_eq!(spans[0].style.fg, Some(Color::Red));
        assert_eq!(spans[1].content, " there");
        assert_eq!(spans[1].style.fg, None);
    }

    #[test]
    fn cursor_moves_are_ignored() {
        // ESC[2J (clear) and ESC[10;5H (move) must not appear as text.
        let t = to_text(b"\x1b[2J\x1b[10;5Hok");
        assert_eq!(t.lines.len(), 1);
        assert_eq!(t.lines[0].spans[0].content, "ok");
    }

    #[test]
    fn cp437_high_bytes_decode_to_box_glyphs() {
        // 0xC9 ╔  0xCD ═  0xBB ╗  — not valid UTF-8, so the CP437 path runs.
        let t = to_text(&[0xC9, 0xCD, 0xBB]);
        let line: String = t.lines[0]
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect();
        assert_eq!(line, "╔═╗");
    }

    #[test]
    fn truecolor_and_indexed() {
        let t = to_text(b"\x1b[38;2;255;136;0mX\x1b[38;5;200mY");
        let spans = &t.lines[0].spans;
        assert_eq!(spans[0].style.fg, Some(Color::Rgb(255, 136, 0)));
        assert_eq!(spans[1].style.fg, Some(Color::Indexed(200)));
    }
}
