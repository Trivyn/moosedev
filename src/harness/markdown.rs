//! Safe, presentation-only Markdown rendering for the harness transcript.
use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
};
use unicode_width::UnicodeWidthChar;

#[derive(Clone, Copy)]
struct ListState {
    next: Option<u64>,
}

struct Renderer {
    base: Style,
    styles: Vec<Style>,
    lines: Vec<Line<'static>>,
    current: Vec<Span<'static>>,
    lists: Vec<ListState>,
    item_depth: usize,
    quote_depth: usize,
    in_code_block: bool,
    links: Vec<String>,
}

impl Renderer {
    fn new(base: Style) -> Self {
        Self {
            base,
            styles: Vec::new(),
            lines: Vec::new(),
            current: Vec::new(),
            lists: Vec::new(),
            item_depth: 0,
            quote_depth: 0,
            in_code_block: false,
            links: Vec::new(),
        }
    }

    fn style(&self) -> Style {
        self.styles
            .iter()
            .fold(self.base, |style, overlay| style.patch(*overlay))
    }

    fn ensure_prefix(&mut self) {
        if !self.current.is_empty() {
            return;
        }
        let prefix = Style::default().fg(Color::DarkGray);
        for _ in 0..self.quote_depth {
            self.push_styled("│ ", prefix);
        }
        if self.in_code_block {
            self.push_styled("│ ", prefix);
        }
    }

    fn push_styled(&mut self, value: &str, style: Style) {
        if value.is_empty() {
            return;
        }
        if let Some(last) = self.current.last_mut().filter(|span| span.style == style) {
            last.content.to_mut().push_str(value);
        } else {
            self.current.push(Span::styled(value.to_owned(), style));
        }
    }

    fn push(&mut self, value: &str) {
        let style = self.style();
        self.push_value(value, style);
    }

    fn push_value(&mut self, value: &str, style: Style) {
        let value: String = value
            .chars()
            .filter(|c| !c.is_control() || *c == '\n' || *c == '\t')
            .collect();
        for (index, part) in value.split('\n').enumerate() {
            if index > 0 {
                self.finish_line(true);
            }
            if !part.is_empty() {
                self.ensure_prefix();
                self.push_styled(part, style);
            }
        }
    }

    fn finish_line(&mut self, force: bool) {
        if force {
            self.ensure_prefix();
        }
        if force || !self.current.is_empty() {
            self.lines
                .push(Line::from(std::mem::take(&mut self.current)));
        }
    }

    fn blank_line(&mut self) {
        self.finish_line(false);
        if self.lines.last().is_some_and(|line| !line.spans.is_empty()) {
            self.lines.push(Line::default());
        }
    }

    fn start_item(&mut self) {
        self.finish_line(false);
        self.ensure_prefix();
        let depth = self.lists.len().saturating_sub(1);
        self.push_styled(&"  ".repeat(depth), Style::default());
        let marker = match self.lists.last_mut() {
            Some(ListState { next: Some(next) }) => {
                let marker = format!("{next}. ");
                *next = next.saturating_add(1);
                marker
            }
            _ => "• ".to_owned(),
        };
        self.push_styled(
            &marker,
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        );
        self.item_depth += 1;
    }

    fn start_code_block(&mut self, kind: CodeBlockKind<'_>) {
        self.finish_line(false);
        let language = match kind {
            CodeBlockKind::Fenced(info) => info.split_whitespace().next().unwrap_or("").to_owned(),
            CodeBlockKind::Indented => String::new(),
        };
        let title = if language.is_empty() {
            "┌─ code".to_owned()
        } else {
            format!("┌─ {language}")
        };
        self.push_styled(
            &title,
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        );
        self.finish_line(true);
        self.in_code_block = true;
        self.styles.push(Style::default().fg(Color::LightGreen));
    }

    fn end_code_block(&mut self) {
        self.finish_line(false);
        self.styles.pop();
        self.in_code_block = false;
        self.push_styled("└─", Style::default().fg(Color::DarkGray));
        self.finish_line(true);
        self.blank_line();
    }

    fn finish(mut self) -> Text<'static> {
        self.finish_line(false);
        while self.lines.last().is_some_and(|line| line.spans.is_empty()) {
            self.lines.pop();
        }
        Text::from(self.lines)
    }
}

/// Render untrusted model Markdown without emitting terminal control sequences.
pub(crate) fn render(input: &str, base: Style) -> Text<'static> {
    let input: String = input
        .chars()
        .filter(|c| !c.is_control() || *c == '\n' || *c == '\t')
        .collect();
    let options = Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS | Options::ENABLE_GFM;
    let mut renderer = Renderer::new(base);

    for event in Parser::new_ext(&input, options) {
        match event {
            Event::Start(Tag::Heading { level, .. }) => {
                renderer.finish_line(false);
                let marker = match level {
                    HeadingLevel::H1 => "# ",
                    HeadingLevel::H2 => "## ",
                    HeadingLevel::H3 => "### ",
                    HeadingLevel::H4 => "#### ",
                    HeadingLevel::H5 => "##### ",
                    HeadingLevel::H6 => "###### ",
                };
                renderer.push_styled(marker, Style::default().fg(Color::DarkGray));
                renderer.styles.push(
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                );
            }
            Event::End(TagEnd::Heading(_)) => {
                renderer.styles.pop();
                renderer.finish_line(false);
                renderer.blank_line();
            }
            Event::Start(Tag::Emphasis) => renderer
                .styles
                .push(Style::default().add_modifier(Modifier::ITALIC)),
            Event::End(TagEnd::Emphasis) => {
                renderer.styles.pop();
            }
            Event::Start(Tag::Strong) => renderer
                .styles
                .push(Style::default().add_modifier(Modifier::BOLD)),
            Event::End(TagEnd::Strong) => {
                renderer.styles.pop();
            }
            Event::Start(Tag::Strikethrough) => renderer
                .styles
                .push(Style::default().add_modifier(Modifier::CROSSED_OUT)),
            Event::End(TagEnd::Strikethrough) => {
                renderer.styles.pop();
            }
            Event::Start(Tag::BlockQuote(_)) => {
                renderer.finish_line(false);
                renderer.quote_depth += 1;
                renderer.styles.push(
                    Style::default()
                        .fg(Color::Gray)
                        .add_modifier(Modifier::ITALIC),
                );
            }
            Event::End(TagEnd::BlockQuote(_)) => {
                renderer.finish_line(false);
                renderer.styles.pop();
                renderer.quote_depth = renderer.quote_depth.saturating_sub(1);
                renderer.blank_line();
            }
            Event::Start(Tag::CodeBlock(kind)) => renderer.start_code_block(kind),
            Event::End(TagEnd::CodeBlock) => renderer.end_code_block(),
            Event::Start(Tag::List(next)) => renderer.lists.push(ListState { next }),
            Event::End(TagEnd::List(_)) => {
                renderer.finish_line(false);
                renderer.lists.pop();
                if renderer.lists.is_empty() {
                    renderer.blank_line();
                }
            }
            Event::Start(Tag::Item) => renderer.start_item(),
            Event::End(TagEnd::Item) => {
                renderer.finish_line(false);
                renderer.item_depth = renderer.item_depth.saturating_sub(1);
            }
            Event::Start(Tag::Link { dest_url, .. }) => {
                renderer.links.push(dest_url.into_string());
                renderer.styles.push(
                    Style::default()
                        .fg(Color::LightBlue)
                        .add_modifier(Modifier::UNDERLINED),
                );
            }
            Event::End(TagEnd::Link) => {
                renderer.styles.pop();
                if let Some(target) = renderer.links.pop().filter(|target| !target.is_empty()) {
                    renderer.push_value(
                        &format!(" ({target})"),
                        Style::default().fg(Color::DarkGray),
                    );
                }
            }
            Event::Start(Tag::Image { dest_url, .. }) => {
                renderer.push_value("[image: ", Style::default().fg(Color::LightBlue));
                renderer.links.push(dest_url.into_string());
                renderer.styles.push(Style::default().fg(Color::LightBlue));
            }
            Event::End(TagEnd::Image) => {
                renderer.styles.pop();
                renderer.push_value("]", Style::default().fg(Color::LightBlue));
                if let Some(target) = renderer.links.pop().filter(|target| !target.is_empty()) {
                    renderer.push_value(
                        &format!(" ({target})"),
                        Style::default().fg(Color::DarkGray),
                    );
                }
            }
            Event::End(TagEnd::Paragraph) => {
                renderer.finish_line(false);
                if renderer.item_depth == 0 {
                    renderer.blank_line();
                }
            }
            Event::Text(text) => renderer.push(&text),
            Event::Code(code) => renderer.push_value(
                &code,
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
            Event::Html(html) | Event::InlineHtml(html) => renderer.push_value(
                &html,
                Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::DIM),
            ),
            Event::SoftBreak => renderer.push(" "),
            Event::HardBreak => renderer.finish_line(true),
            Event::Rule => {
                renderer.finish_line(false);
                renderer.push_styled(
                    "────────────────────────",
                    Style::default().fg(Color::DarkGray),
                );
                renderer.finish_line(true);
                renderer.blank_line();
            }
            Event::TaskListMarker(checked) => renderer.push_value(
                if checked { "[x] " } else { "[ ] " },
                Style::default().fg(if checked {
                    Color::Green
                } else {
                    Color::DarkGray
                }),
            ),
            Event::FootnoteReference(label) => {
                renderer.push_value(&format!("[{label}]"), Style::default().fg(Color::DarkGray))
            }
            Event::InlineMath(math) => {
                renderer.push_value(&format!("${math}$"), Style::default().fg(Color::Magenta))
            }
            Event::DisplayMath(math) => {
                renderer.push_value(&format!("$${math}$$"), Style::default().fg(Color::Magenta))
            }
            Event::Start(_) | Event::End(_) => {}
        }
    }
    renderer.finish()
}

/// Hard-wrap styled text with the same display-width semantics as the old transcript renderer.
pub(crate) fn wrap(text: Text<'static>, width: usize) -> Text<'static> {
    let width = width.max(1);
    let text_style = text.style;
    let mut output = Vec::new();
    for line in text.lines {
        if line.spans.is_empty() {
            output.push(Line::default());
            continue;
        }
        let mut current: Vec<Span<'static>> = Vec::new();
        let mut col = 0usize;
        for span in line.spans {
            let style = text_style.patch(line.style).patch(span.style);
            let content = span.content.replace('\t', "    ");
            for c in content.chars() {
                let size = c.width().unwrap_or(0);
                if col > 0 && col + size > width {
                    output.push(Line::from(std::mem::take(&mut current)));
                    col = 0;
                }
                if let Some(last) = current.last_mut().filter(|span| span.style == style) {
                    last.content.to_mut().push(c);
                } else {
                    current.push(Span::styled(c.to_string(), style));
                }
                col += size;
            }
        }
        output.push(Line::from(current));
    }
    Text::from(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain(text: &Text<'_>) -> String {
        text.lines
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn markdown_structure_becomes_safe_styled_terminal_text() {
        let text = render(
            "# Heading\n\n- **bold** and *italic* with `code`\n\n> quote\n\n[site](https://example.com)\n\n```rust\nfn main() {}\n```\n\u{1b}[31m &#x1b;[32m",
            Style::default().fg(Color::LightCyan),
        );
        let plain = plain(&text);
        assert!(plain.contains("# Heading"));
        assert!(plain.contains("• bold and italic with code"));
        assert!(plain.contains("│ quote"));
        assert!(plain.contains("site (https://example.com)"));
        assert!(plain.contains("┌─ rust\n│ fn main() {}\n└─"), "{plain:?}");
        assert!(!plain.contains('\u{1b}'));
    }

    #[test]
    fn streaming_incomplete_fence_and_wide_text_wrap_without_panicking() {
        let text = render(
            "```rust\nlet animal = \"🫎日本\";",
            Style::default().fg(Color::LightCyan),
        );
        let wrapped = wrap(text, 8);
        assert!(wrapped.height() > 2);
        assert!(plain(&wrapped).replace('\n', "").contains("animal"));
    }

    #[test]
    fn task_lists_strikes_and_raw_html_remain_readable() {
        let text = render(
            "- [x] done\n- [ ] ~~later~~\n\n<span>raw</span>",
            Style::default().fg(Color::LightCyan),
        );
        let plain = plain(&text);
        assert!(plain.contains("• [x] done"));
        assert!(plain.contains("• [ ] later"));
        assert!(plain.contains("<span>raw</span>"));
    }
}
