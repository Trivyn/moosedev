//! Mouse text selection over the TUI's pre-wrapped content rows.
//!
//! Positions are content coordinates (post-wrap row index, display cell), so
//! a selection stays attached to its text while the pane scrolls or streams.
use ratatui::{
    style::Modifier,
    text::{Line, Span},
};
use std::collections::BTreeSet;
use unicode_width::UnicodeWidthChar;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct Pos {
    pub line: usize,
    pub col: usize,
}

/// Both ends are inclusive cells: the cell under the press and the cell under
/// the pointer are part of the selection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Selection {
    pub anchor: Pos,
    pub head: Pos,
}

impl Selection {
    fn ordered(&self) -> (Pos, Pos) {
        if self.anchor <= self.head {
            (self.anchor, self.head)
        } else {
            (self.head, self.anchor)
        }
    }

    /// The inclusive cell range selected on `line`, if any.
    pub(super) fn columns(&self, line: usize) -> Option<(usize, usize)> {
        let (start, end) = self.ordered();
        (start.line..=end.line).contains(&line).then_some((
            if line == start.line { start.col } else { 0 },
            if line == end.line {
                end.col
            } else {
                usize::MAX
            },
        ))
    }
}

/// Whether the cells `[col, col + width)` of one character meet the inclusive
/// range. A zero-width character follows the character before it.
fn selected(col: usize, width: usize, from: usize, to: usize, previous: bool) -> bool {
    if width == 0 {
        previous
    } else {
        col <= to && col + width > from
    }
}

fn slice(row: &str, from: usize, to: usize) -> String {
    let mut out = String::new();
    let (mut col, mut previous) = (0, false);
    for c in row.chars() {
        let width = c.width().unwrap_or(0);
        previous = selected(col, width, from, to, previous);
        if previous {
            out.push(c);
        }
        col += width;
    }
    out
}

/// The selected text. Rows listed in `soft_breaks` continue the row before
/// them (one logical line hard-wrapped to the pane), so they rejoin it
/// without a newline.
pub(super) fn text(rows: &[String], soft_breaks: &BTreeSet<usize>, selection: Selection) -> String {
    let mut out = String::new();
    let (start, end) = selection.ordered();
    for (index, row) in rows
        .iter()
        .enumerate()
        .skip(start.line)
        .take(end.line.saturating_sub(start.line) + 1)
    {
        let Some((from, to)) = selection.columns(index) else {
            continue;
        };
        if index > start.line && !soft_breaks.contains(&index) {
            out.push('\n');
        }
        out.push_str(&slice(row, from, to));
    }
    out
}

/// `line` with the inclusive cell range shown reversed.
pub(super) fn highlight(line: Line<'static>, from: usize, to: usize) -> Line<'static> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    let (mut col, mut previous) = (0, false);
    for span in line.spans {
        for c in span.content.chars() {
            let width = c.width().unwrap_or(0);
            previous = selected(col, width, from, to, previous);
            let style = if previous {
                span.style.add_modifier(Modifier::REVERSED)
            } else {
                span.style
            };
            match spans.last_mut().filter(|last| last.style == style) {
                Some(last) => last.content.to_mut().push(c),
                None => spans.push(Span::styled(c.to_string(), style)),
            }
            col += width;
        }
    }
    Line {
        spans,
        style: line.style,
        alignment: line.alignment,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::style::{Color, Style};

    fn select(anchor: (usize, usize), head: (usize, usize)) -> Selection {
        Selection {
            anchor: Pos {
                line: anchor.0,
                col: anchor.1,
            },
            head: Pos {
                line: head.0,
                col: head.1,
            },
        }
    }

    fn rows(rows: &[&str]) -> Vec<String> {
        rows.iter().map(|row| row.to_string()).collect()
    }

    #[test]
    fn selection_copies_inclusive_cells_in_either_drag_direction() {
        let rows = rows(&["hello world", "second row", "third"]);
        let none = BTreeSet::new();
        assert_eq!(text(&rows, &none, select((0, 6), (0, 10))), "world");
        assert_eq!(text(&rows, &none, select((0, 10), (0, 6))), "world");
        assert_eq!(
            text(&rows, &none, select((0, 6), (2, 1))),
            "world\nsecond row\nth"
        );
        assert_eq!(
            text(&rows, &none, select((2, 1), (0, 6))),
            "world\nsecond row\nth"
        );
        // Past the end of a row, and past the last row, selects what exists.
        assert_eq!(text(&rows, &none, select((2, 3), (9, 40))), "rd");
    }

    #[test]
    fn hard_wrapped_rows_rejoin_without_a_newline() {
        let rows = rows(&["cargo test --fea", "tures harness", "", "next"]);
        let soft_breaks = BTreeSet::from([1]);
        assert_eq!(
            text(&rows, &soft_breaks, select((0, 0), (3, 3))),
            "cargo test --features harness\n\nnext"
        );
    }

    #[test]
    fn wide_and_zero_width_characters_select_whole() {
        // 日 and 本 are two cells each; e + U+0301 is one cell.
        let rows = rows(&["a日本e\u{301}z"]);
        let none = BTreeSet::new();
        // Cell 2 is the second half of 日.
        assert_eq!(text(&rows, &none, select((0, 2), (0, 3))), "日本");
        assert_eq!(text(&rows, &none, select((0, 5), (0, 5))), "e\u{301}");
        assert_eq!(text(&rows, &none, select((0, 6), (0, 6))), "z");
    }

    #[test]
    fn highlight_reverses_only_the_selected_cells_and_keeps_styles() {
        let red = Style::default().fg(Color::Red);
        let line = Line::from(vec![Span::styled("abc", red), Span::raw("def")]);
        let shown = highlight(line, 2, 3);
        let parts: Vec<_> = shown
            .spans
            .iter()
            .map(|span| (span.content.to_string(), span.style))
            .collect();
        assert_eq!(
            parts,
            vec![
                ("ab".to_string(), red),
                ("c".to_string(), red.add_modifier(Modifier::REVERSED)),
                (
                    "d".to_string(),
                    Style::default().add_modifier(Modifier::REVERSED)
                ),
                ("ef".to_string(), Style::default()),
            ]
        );
    }
}
