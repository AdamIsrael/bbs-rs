//! Decode a raw terminal byte stream (from SSH `data`, later WebSocket) into
//! [`Event`]s. This is a deliberately small hand-rolled parser covering the
//! keys the BBS uses; `termwiz`'s `InputParser` is the natural upgrade path if
//! richer input handling is needed.
//!
//! Parsing is *stateful*: [`drain`] consumes complete tokens from a caller-owned
//! buffer and leaves any trailing incomplete escape sequence in place, so an
//! arrow sequence split across two `data` packets is reassembled rather than
//! misread as a stray `Esc` (which could otherwise trigger an unintended
//! action). A lone `Esc` keypress — which a terminal delivers as its own
//! single-byte packet — is still emitted immediately.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::transport::Event;

fn key(code: KeyCode) -> Event {
    Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
}

fn ctrl(c: char) -> Event {
    Event::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL))
}

/// Number of bytes in a UTF-8 sequence given its leading byte.
fn utf8_len(b: u8) -> usize {
    if b < 0x80 {
        1
    } else if b >= 0xF0 {
        4
    } else if b >= 0xE0 {
        3
    } else if b >= 0xC0 {
        2
    } else {
        1
    }
}

/// Consume complete key events from the front of `buf`, draining the bytes that
/// were interpreted. A trailing incomplete escape or UTF-8 sequence is retained
/// for the next call.
pub fn drain(buf: &mut Vec<u8>) -> Vec<Event> {
    let mut out = Vec::new();
    let mut i = 0;
    let len = buf.len();

    while i < len {
        let b = buf[i];
        match b {
            0x1b => {
                if i + 1 >= len {
                    // Trailing ESC. If it's the whole buffer, it's a real
                    // Escape keypress (delivered as its own packet); emit it.
                    // Otherwise it may be the head of a split sequence — hold.
                    if i == 0 {
                        out.push(key(KeyCode::Esc));
                        i += 1;
                    }
                    break;
                }
                match buf[i + 1] {
                    b'[' | b'O' => {
                        match buf.get(i + 2) {
                            Some(b'A') => {
                                out.push(key(KeyCode::Up));
                                i += 3;
                            }
                            Some(b'B') => {
                                out.push(key(KeyCode::Down));
                                i += 3;
                            }
                            Some(b'C') => {
                                out.push(key(KeyCode::Right));
                                i += 3;
                            }
                            Some(b'D') => {
                                out.push(key(KeyCode::Left));
                                i += 3;
                            }
                            Some(b'H') => {
                                out.push(key(KeyCode::Home));
                                i += 3;
                            }
                            Some(b'F') => {
                                out.push(key(KeyCode::End));
                                i += 3;
                            }
                            Some(b'Z') => {
                                out.push(key(KeyCode::BackTab));
                                i += 3;
                            }
                            Some(b'3') => match buf.get(i + 3) {
                                Some(b'~') => {
                                    out.push(key(KeyCode::Delete));
                                    i += 4;
                                }
                                Some(_) => {
                                    // Unknown ESC [ 3 x — skip it.
                                    i += 4;
                                }
                                None => break, // incomplete, wait for more
                            },
                            Some(_) => {
                                // Unknown CSI final byte — skip the 3 bytes.
                                i += 3;
                            }
                            None => break, // incomplete CSI, wait for more
                        }
                    }
                    _ => {
                        // ESC followed by a non-CSI byte: treat as a bare Esc,
                        // leaving the following byte to be parsed next.
                        out.push(key(KeyCode::Esc));
                        i += 1;
                    }
                }
            }
            b'\r' | b'\n' => {
                out.push(key(KeyCode::Enter));
                i += 1;
            }
            0x7f | 0x08 => {
                out.push(key(KeyCode::Backspace));
                i += 1;
            }
            b'\t' => {
                out.push(key(KeyCode::Tab));
                i += 1;
            }
            0x03 => {
                out.push(ctrl('c'));
                i += 1;
            }
            0x04 => {
                out.push(ctrl('d'));
                i += 1;
            }
            // Other C0 control bytes are ignored.
            0x00..=0x1f => {
                i += 1;
            }
            _ => {
                let clen = utf8_len(b);
                if i + clen > len {
                    break; // incomplete UTF-8, wait for more
                }
                if let Ok(s) = std::str::from_utf8(&buf[i..i + clen])
                    && let Some(c) = s.chars().next()
                {
                    out.push(key(KeyCode::Char(c)));
                }
                i += clen;
            }
        }
    }

    buf.drain(0..i);
    out
}

/// Stateless convenience wrapper: parse a complete byte slice in one shot.
/// Any trailing incomplete sequence is dropped (callers needing correct
/// handling of split sequences should use [`drain`] with a persistent buffer).
pub fn parse(bytes: &[u8]) -> Vec<Event> {
    let mut buf = bytes.to_vec();
    drain(&mut buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn codes(bytes: &[u8]) -> Vec<KeyCode> {
        parse(bytes)
            .into_iter()
            .filter_map(|e| match e {
                Event::Key(k) => Some(k.code),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn plain_chars() {
        assert_eq!(codes(b"hi"), vec![KeyCode::Char('h'), KeyCode::Char('i')]);
    }

    #[test]
    fn arrows_and_enter() {
        assert_eq!(
            codes(b"\x1b[A\x1b[B\r"),
            vec![KeyCode::Up, KeyCode::Down, KeyCode::Enter]
        );
    }

    #[test]
    fn backspace_and_tab() {
        assert_eq!(codes(b"\x7f\t"), vec![KeyCode::Backspace, KeyCode::Tab]);
    }

    #[test]
    fn ctrl_c_has_modifier() {
        let evs = parse(b"\x03");
        match &evs[0] {
            Event::Key(k) => {
                assert_eq!(k.code, KeyCode::Char('c'));
                assert!(k.modifiers.contains(KeyModifiers::CONTROL));
            }
            _ => panic!("expected key"),
        }
    }

    #[test]
    fn utf8_multibyte() {
        assert_eq!(codes("é".as_bytes()), vec![KeyCode::Char('é')]);
    }

    #[test]
    fn lone_escape_emitted() {
        assert_eq!(codes(b"\x1b"), vec![KeyCode::Esc]);
    }

    #[test]
    fn split_arrow_sequence_reassembled() {
        // A run of arrows split mid-sequence must not produce a stray Esc.
        let mut buf: Vec<u8> = Vec::new();
        buf.extend_from_slice(b"\x1b[B\x1b[B\x1b"); // trailing bare ESC held
        let first: Vec<KeyCode> = drain(&mut buf)
            .into_iter()
            .filter_map(|e| match e {
                Event::Key(k) => Some(k.code),
                _ => None,
            })
            .collect();
        assert_eq!(first, vec![KeyCode::Down, KeyCode::Down]);
        assert_eq!(buf, b"\x1b"); // ESC retained, not emitted as a key

        buf.extend_from_slice(b"[B\r"); // continuation arrives
        let second: Vec<KeyCode> = drain(&mut buf)
            .into_iter()
            .filter_map(|e| match e {
                Event::Key(k) => Some(k.code),
                _ => None,
            })
            .collect();
        assert_eq!(second, vec![KeyCode::Down, KeyCode::Enter]);
        assert!(buf.is_empty());
    }
}
