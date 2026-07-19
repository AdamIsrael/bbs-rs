//! A small multi-line text buffer for the compose editor (#96).
//!
//! Headless on purpose, like the config editor: it owns the buffer, the cursor,
//! and every edit, and knows nothing about ratatui. The renderer asks it for
//! wrapped display rows and where the cursor sits; a test drives it by method
//! call and asserts on the text. That's what lets the tricky parts — backspace
//! merging lines, the cursor's column surviving a move onto a shorter line, the
//! mapping through word-wrap — be tested without a terminal.
//!
//! Lines are stored as `Vec<char>` rather than `String` so the cursor column is
//! a plain index: no char-boundary arithmetic, and a multi-byte character is
//! one column like it looks on screen.

/// A multi-line editable buffer with a cursor.
#[derive(Debug, Clone)]
pub struct TextArea {
    /// Never empty — an empty buffer is one empty line, so the cursor always has
    /// somewhere to be.
    lines: Vec<Vec<char>>,
    /// Cursor line, `0..lines.len()`.
    row: usize,
    /// Cursor column in characters, `0..=lines[row].len()` (one past the end is
    /// the append position).
    col: usize,
}

impl Default for TextArea {
    fn default() -> Self {
        Self {
            lines: vec![Vec::new()],
            row: 0,
            col: 0,
        }
    }
}

impl TextArea {
    /// An empty buffer.
    pub fn new() -> Self {
        Self::default()
    }

    /// Seed from existing text, cursor at the end — for editing a draft.
    pub fn from_text(text: &str) -> Self {
        let mut lines: Vec<Vec<char>> = text.split('\n').map(|l| l.chars().collect()).collect();
        if lines.is_empty() {
            lines.push(Vec::new());
        }
        let row = lines.len() - 1;
        let col = lines[row].len();
        Self { lines, row, col }
    }

    /// The buffer as text, lines joined with `\n`.
    pub fn text(&self) -> String {
        self.lines
            .iter()
            .map(|l| l.iter().collect::<String>())
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Whether the buffer holds nothing (one empty line).
    pub fn is_empty(&self) -> bool {
        self.lines.len() == 1 && self.lines[0].is_empty()
    }

    /// Total character count, newlines included — for a length limit.
    pub fn char_count(&self) -> usize {
        // Sum of line lengths, plus one newline between each pair of lines.
        self.lines.iter().map(|l| l.len()).sum::<usize>() + self.lines.len().saturating_sub(1)
    }

    pub fn cursor(&self) -> (usize, usize) {
        (self.row, self.col)
    }

    pub fn line_count(&self) -> usize {
        self.lines.len()
    }

    // ---- editing ---------------------------------------------------------

    pub fn insert_char(&mut self, c: char) {
        self.lines[self.row].insert(self.col, c);
        self.col += 1;
    }

    /// Split the current line at the cursor, moving the tail onto a new line.
    pub fn insert_newline(&mut self) {
        let tail = self.lines[self.row].split_off(self.col);
        self.lines.insert(self.row + 1, tail);
        self.row += 1;
        self.col = 0;
    }

    /// Delete the character before the cursor. At the start of a line this
    /// merges it onto the end of the previous line — the join-across-lines case
    /// a single-line field can't do.
    pub fn backspace(&mut self) {
        if self.col > 0 {
            self.col -= 1;
            self.lines[self.row].remove(self.col);
        } else if self.row > 0 {
            let mut line = self.lines.remove(self.row);
            self.row -= 1;
            self.col = self.lines[self.row].len();
            self.lines[self.row].append(&mut line);
        }
    }

    /// Delete the character under the cursor. At end of line this pulls the next
    /// line up.
    pub fn delete(&mut self) {
        if self.col < self.lines[self.row].len() {
            self.lines[self.row].remove(self.col);
        } else if self.row + 1 < self.lines.len() {
            let mut next = self.lines.remove(self.row + 1);
            self.lines[self.row].append(&mut next);
        }
    }

    // ---- cursor movement -------------------------------------------------

    pub fn left(&mut self) {
        if self.col > 0 {
            self.col -= 1;
        } else if self.row > 0 {
            self.row -= 1;
            self.col = self.lines[self.row].len();
        }
    }

    pub fn right(&mut self) {
        if self.col < self.lines[self.row].len() {
            self.col += 1;
        } else if self.row + 1 < self.lines.len() {
            self.row += 1;
            self.col = 0;
        }
    }

    pub fn up(&mut self) {
        if self.row > 0 {
            self.row -= 1;
            // Keep the column, clamped: moving onto a shorter line lands at its
            // end rather than off it.
            self.col = self.col.min(self.lines[self.row].len());
        }
    }

    pub fn down(&mut self) {
        if self.row + 1 < self.lines.len() {
            self.row += 1;
            self.col = self.col.min(self.lines[self.row].len());
        }
    }

    pub fn home(&mut self) {
        self.col = 0;
    }

    pub fn end(&mut self) {
        self.col = self.lines[self.row].len();
    }

    // ---- display ---------------------------------------------------------

    /// Wrap the buffer to `width` columns and report where the cursor lands.
    ///
    /// Returns the display rows (each already ≤ width) and the cursor's
    /// `(display_row, display_col)`. Word-wrapping is applied per logical line:
    /// a run longer than `width` breaks at the last space that fits, and a
    /// single word wider than the whole area is hard-split so nothing is lost.
    /// The cursor is mapped through the same wrapping so it stays on the
    /// character it's actually before.
    pub fn display(&self, width: usize) -> (Vec<String>, (usize, usize)) {
        let width = width.max(1);
        let mut rows = Vec::new();
        let mut cursor = (0usize, 0usize);

        for (r, line) in self.lines.iter().enumerate() {
            let segments = wrap_segments(line, width);
            let first_row = rows.len();
            for (seg_idx, (start, end)) in segments.iter().enumerate() {
                rows.push(line[*start..*end].iter().collect());
                // The cursor sits on this row when its column falls within the
                // segment — or, for the last segment, at the segment's very end
                // (the append position).
                if r == self.row {
                    let is_last = seg_idx + 1 == segments.len();
                    let within =
                        self.col >= *start && (self.col < *end || (is_last && self.col == *end));
                    if within {
                        cursor = (first_row + seg_idx, self.col - start);
                    }
                }
            }
        }
        // A trailing empty logical line still needs a row to place the cursor on.
        if rows.is_empty() {
            rows.push(String::new());
        }
        (rows, cursor)
    }
}

/// Split one logical line into `(start, end)` char-index ranges, each at most
/// `width` wide, breaking at spaces where possible.
fn wrap_segments(line: &[char], width: usize) -> Vec<(usize, usize)> {
    if line.is_empty() {
        return vec![(0, 0)];
    }
    let mut segments = Vec::new();
    let mut start = 0;
    while start < line.len() {
        let remaining = line.len() - start;
        if remaining <= width {
            segments.push((start, line.len()));
            break;
        }
        // The widest slice that fits, then back up to the last space in it so we
        // break between words. If there's no space, hard-split at `width` so a
        // single long word can't stall.
        let hard_end = start + width;
        let break_at = line[start..hard_end]
            .iter()
            .rposition(|c| *c == ' ')
            .map(|rel| start + rel);
        match break_at {
            // Break after the space (the space stays on the ending row).
            Some(sp) if sp > start => {
                segments.push((start, sp + 1));
                start = sp + 1;
            }
            _ => {
                segments.push((start, hard_end));
                start = hard_end;
            }
        }
    }
    segments
}

#[cfg(test)]
mod tests {
    use super::*;

    fn typed(text: &str) -> TextArea {
        let mut ta = TextArea::new();
        for c in text.chars() {
            if c == '\n' {
                ta.insert_newline();
            } else {
                ta.insert_char(c);
            }
        }
        ta
    }

    #[test]
    fn typing_and_text_round_trip() {
        let ta = typed("hello");
        assert_eq!(ta.text(), "hello");
        assert_eq!(ta.cursor(), (0, 5));
        assert!(!ta.is_empty());
    }

    #[test]
    fn a_fresh_buffer_is_empty() {
        let ta = TextArea::new();
        assert!(ta.is_empty());
        assert_eq!(ta.text(), "");
        assert_eq!(ta.line_count(), 1);
    }

    #[test]
    fn newline_splits_the_line_at_the_cursor() {
        let mut ta = typed("helloworld");
        for _ in 0..5 {
            ta.left();
        }
        ta.insert_newline();
        assert_eq!(ta.text(), "hello\nworld");
        assert_eq!(
            ta.cursor(),
            (1, 0),
            "cursor moves to the start of the new line"
        );
    }

    #[test]
    fn backspace_at_line_start_merges_onto_the_previous_line() {
        let mut ta = typed("hello\nworld");
        ta.home(); // col 0 of "world"
        ta.backspace();
        assert_eq!(ta.text(), "helloworld", "the two lines join");
        assert_eq!(ta.cursor(), (0, 5), "cursor lands where the join happened");
    }

    #[test]
    fn backspace_within_a_line_removes_one_char() {
        let mut ta = typed("hello");
        ta.backspace();
        assert_eq!(ta.text(), "hell");
        assert_eq!(ta.cursor(), (0, 4));
    }

    #[test]
    fn delete_at_line_end_pulls_the_next_line_up() {
        let mut ta = typed("hello\nworld");
        ta.up();
        ta.end(); // end of "hello"
        ta.delete();
        assert_eq!(ta.text(), "helloworld");
        assert_eq!(ta.cursor(), (0, 5), "cursor stays put");
    }

    #[test]
    fn moving_down_onto_a_shorter_line_clamps_the_column() {
        let mut ta = typed("longline\nx");
        ta.up();
        ta.end(); // col 8 on "longline"
        ta.down();
        assert_eq!(ta.cursor(), (1, 1), "clamped to the end of the short line");
    }

    #[test]
    fn left_at_line_start_wraps_to_the_end_of_the_previous_line() {
        let mut ta = typed("ab\ncd");
        ta.home(); // (1,0)
        ta.left();
        assert_eq!(ta.cursor(), (0, 2), "moved up to the end of 'ab'");
    }

    #[test]
    fn right_at_line_end_wraps_to_the_start_of_the_next_line() {
        let mut ta = typed("ab\ncd");
        ta.up();
        ta.end(); // (0,2)
        ta.right();
        assert_eq!(ta.cursor(), (1, 0));
    }

    #[test]
    fn char_count_includes_newlines() {
        let ta = typed("ab\ncd"); // 4 chars + 1 newline
        assert_eq!(ta.char_count(), 5);
    }

    #[test]
    fn from_text_places_the_cursor_at_the_end() {
        let ta = TextArea::from_text("first\nsecond");
        assert_eq!(ta.line_count(), 2);
        assert_eq!(ta.cursor(), (1, 6));
        assert_eq!(ta.text(), "first\nsecond");
    }

    #[test]
    fn unicode_columns_are_characters_not_bytes() {
        let mut ta = typed("café");
        assert_eq!(ta.cursor(), (0, 4), "é is one column");
        ta.backspace();
        assert_eq!(ta.text(), "caf", "backspace removes the whole é");
    }

    // ---- display / word-wrap --------------------------------------------

    #[test]
    fn a_short_line_is_one_display_row() {
        let ta = typed("hello");
        let (rows, cursor) = ta.display(20);
        assert_eq!(rows, vec!["hello"]);
        assert_eq!(cursor, (0, 5));
    }

    #[test]
    fn a_long_line_wraps_at_a_space() {
        let ta = typed("the quick brown fox");
        let (rows, _) = ta.display(10);
        // Breaks between words, no word split.
        assert_eq!(rows, vec!["the quick ", "brown fox"]);
    }

    #[test]
    fn a_word_wider_than_the_area_is_hard_split() {
        let ta = typed("supercalifragilistic");
        let (rows, _) = ta.display(8);
        assert_eq!(rows, vec!["supercal", "ifragili", "stic"]);
        assert_eq!(rows.iter().map(|r| r.chars().count()).max().unwrap(), 8);
    }

    #[test]
    fn the_cursor_maps_onto_the_wrapped_row_it_belongs_to() {
        let ta = typed("the quick brown fox"); // cursor at end, col 19
        let (_, cursor) = ta.display(10);
        // Row 0 is "the quick " (10 chars, cols 0..10); row 1 is "brown fox".
        // The cursor at col 19 lands at the end of row 1, col 9.
        assert_eq!(cursor, (1, 9));
    }

    #[test]
    fn the_cursor_at_a_wrap_boundary_lands_on_the_second_row() {
        let mut ta = typed("the quick brown fox");
        ta.home();
        for _ in 0..10 {
            ta.right();
        }
        // Col 10 is the 'b' of "brown", the first char of the wrapped row.
        let (_, cursor) = ta.display(10);
        assert_eq!(cursor, (1, 0));
    }

    #[test]
    fn a_trailing_empty_line_still_gets_a_row() {
        let ta = typed("hello\n");
        let (rows, cursor) = ta.display(20);
        assert_eq!(rows, vec!["hello", ""]);
        assert_eq!(cursor, (1, 0), "cursor on the empty line");
    }

    #[test]
    fn display_never_returns_zero_rows() {
        let ta = TextArea::new();
        let (rows, cursor) = ta.display(10);
        assert_eq!(rows.len(), 1);
        assert_eq!(cursor, (0, 0));
    }
}
