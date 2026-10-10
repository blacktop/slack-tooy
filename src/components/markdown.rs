use pulldown_cmark::{CodeBlockKind, Event, Options, Parser, Tag, TagEnd};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

use crate::components::messages::{split_segments, split_to_width};
use crate::theme;

#[derive(Debug, Clone, Copy, Default)]
struct InlineState {
    emphasis: usize,
    strong: usize,
    strike: usize,
    quote: usize,
    link: usize,
}

impl InlineState {
    fn style(self) -> Style {
        let mut style = Style::default().fg(if self.quote > 0 {
            theme::TEXT_MUTED
        } else {
            theme::TEXT
        });
        if self.emphasis > 0 {
            style = style.italic();
        }
        if self.strong > 0 {
            style = style.bold();
        }
        if self.strike > 0 {
            style = style.crossed_out();
        }
        if self.link > 0 {
            style = style.fg(theme::BLUE).underlined();
        }
        style
    }
}

#[derive(Debug, Clone)]
struct StyledSegment {
    text: String,
    style: Style,
}

#[derive(Debug, Clone, Copy)]
struct ListState {
    next: Option<u64>,
}

pub(crate) fn render_markdown(input: &str, width: usize) -> Vec<Line<'static>> {
    let mut renderer = Renderer::new(width);
    renderer.render(&crate::message_links::render_text(input));
    renderer.finish()
}

struct Renderer {
    width: usize,
    lines: Vec<Line<'static>>,
    segments: Vec<StyledSegment>,
    inline: InlineState,
    list_stack: Vec<ListState>,
    in_code_block: bool,
}

impl Renderer {
    fn new(width: usize) -> Self {
        Self {
            width,
            lines: Vec::new(),
            segments: Vec::new(),
            inline: InlineState::default(),
            list_stack: Vec::new(),
            in_code_block: false,
        }
    }

    fn render(&mut self, input: &str) {
        let mut options = Options::empty();
        options.insert(Options::ENABLE_STRIKETHROUGH);
        options.insert(Options::ENABLE_TASKLISTS);

        for event in Parser::new_ext(input, options) {
            self.handle_event(event);
        }
    }

    fn handle_event(&mut self, event: Event<'_>) {
        if self.in_code_block {
            self.handle_code_block_event(event);
            return;
        }

        match event {
            Event::Start(tag) => self.handle_start(tag),
            Event::End(tag) => self.handle_end(tag),
            Event::Text(text) | Event::InlineMath(text) | Event::DisplayMath(text) => {
                self.push_text(text.as_ref(), self.inline.style());
            }
            Event::Code(code) => self.push_text(code.as_ref(), inline_code_style()),
            Event::SoftBreak | Event::HardBreak => self.flush_segments(true),
            Event::Rule => {
                self.flush_segments(false);
                self.lines.push(rule_line(self.width));
            }
            Event::TaskListMarker(checked) => {
                self.push_text(if checked { "[x] " } else { "[ ] " }, self.inline.style());
            }
            Event::Html(html) | Event::InlineHtml(html) | Event::FootnoteReference(html) => {
                self.push_text(html.as_ref(), self.inline.style());
            }
        }
    }

    fn handle_code_block_event(&mut self, event: Event<'_>) {
        match event {
            Event::End(TagEnd::CodeBlock) => {
                self.in_code_block = false;
            }
            Event::Text(text) => {
                self.push_code_text(text.as_ref());
            }
            Event::SoftBreak | Event::HardBreak => self.push_code_line(""),
            Event::Code(code)
            | Event::InlineMath(code)
            | Event::DisplayMath(code)
            | Event::Html(code)
            | Event::InlineHtml(code)
            | Event::FootnoteReference(code) => {
                self.push_code_text(code.as_ref());
            }
            Event::Start(_) | Event::End(_) | Event::Rule | Event::TaskListMarker(_) => {}
        }
    }

    fn handle_start(&mut self, tag: Tag<'_>) {
        match tag {
            Tag::Heading { .. } | Tag::Strong => {
                self.inline.strong = self.inline.strong.saturating_add(1);
            }
            Tag::BlockQuote(_) => {
                self.flush_segments(false);
                self.inline.quote = self.inline.quote.saturating_add(1);
            }
            Tag::CodeBlock(kind) => {
                self.flush_segments(false);
                let language = code_block_language(kind);
                if let Some(ref lang) = language {
                    self.lines.push(code_label_line(lang, self.width));
                }
                self.in_code_block = true;
            }
            Tag::List(next) => {
                self.flush_segments(false);
                self.list_stack.push(ListState { next });
            }
            Tag::Item => {
                self.flush_segments(false);
                let marker = self.next_list_marker();
                self.push_text(&marker, self.inline.style());
            }
            Tag::Emphasis => {
                self.inline.emphasis = self.inline.emphasis.saturating_add(1);
            }
            Tag::Strikethrough => {
                self.inline.strike = self.inline.strike.saturating_add(1);
            }
            Tag::Link { .. } => {
                self.inline.link = self.inline.link.saturating_add(1);
            }
            Tag::Image { dest_url, .. } => {
                self.push_text("[image: ", self.inline.style());
                self.push_text(dest_url.as_ref(), link_style());
                self.push_text("]", self.inline.style());
            }
            Tag::Paragraph
            | Tag::HtmlBlock
            | Tag::FootnoteDefinition(_)
            | Tag::DefinitionList
            | Tag::DefinitionListTitle
            | Tag::DefinitionListDefinition
            | Tag::Table(_)
            | Tag::TableHead
            | Tag::TableRow
            | Tag::TableCell
            | Tag::Superscript
            | Tag::Subscript
            | Tag::MetadataBlock(_) => {}
        }
    }

    fn handle_end(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Paragraph | TagEnd::Item => {
                self.flush_segments(false);
            }
            TagEnd::Heading(_) => {
                self.flush_segments(false);
                self.inline.strong = self.inline.strong.saturating_sub(1);
            }
            TagEnd::BlockQuote(_) => {
                self.flush_segments(false);
                self.inline.quote = self.inline.quote.saturating_sub(1);
            }
            TagEnd::List(_) => {
                self.flush_segments(false);
                self.list_stack.pop();
            }
            TagEnd::Emphasis => {
                self.inline.emphasis = self.inline.emphasis.saturating_sub(1);
            }
            TagEnd::Strong => {
                self.inline.strong = self.inline.strong.saturating_sub(1);
            }
            TagEnd::Strikethrough => {
                self.inline.strike = self.inline.strike.saturating_sub(1);
            }
            TagEnd::Link => {
                self.inline.link = self.inline.link.saturating_sub(1);
            }
            TagEnd::CodeBlock
            | TagEnd::HtmlBlock
            | TagEnd::FootnoteDefinition
            | TagEnd::DefinitionList
            | TagEnd::DefinitionListTitle
            | TagEnd::DefinitionListDefinition
            | TagEnd::Table
            | TagEnd::TableHead
            | TagEnd::TableRow
            | TagEnd::TableCell
            | TagEnd::Superscript
            | TagEnd::Subscript
            | TagEnd::Image
            | TagEnd::MetadataBlock(_) => {}
        }
    }

    fn next_list_marker(&mut self) -> String {
        let depth = self.list_stack.len().saturating_sub(1);
        let indent = "  ".repeat(depth);
        let marker = match self
            .list_stack
            .last_mut()
            .and_then(|state| state.next.as_mut())
        {
            Some(next) => {
                let marker = format!("{next}. ");
                *next = next.saturating_add(1);
                marker
            }
            None => "- ".to_string(),
        };
        format!("{indent}{marker}")
    }

    fn push_text(&mut self, text: &str, style: Style) {
        if !text.is_empty() {
            self.segments.push(StyledSegment {
                text: text.to_string(),
                style,
            });
        }
    }

    fn push_code_text(&mut self, text: &str) {
        for line in text.lines() {
            self.push_code_line(line);
        }
    }

    fn push_code_line(&mut self, line: &str) {
        for wrapped in wrap_code_line(line, self.width) {
            self.lines.push(wrapped);
        }
    }

    fn flush_segments(&mut self, force: bool) {
        if self.segments.is_empty() {
            if force {
                self.lines.push(Line::from(""));
            }
            return;
        }

        let quote_depth = self.inline.quote;
        let wrap_width = self.width.saturating_sub(quote_prefix_width(quote_depth));
        for line in wrap_styled_segments(&self.segments, wrap_width) {
            self.lines.push(apply_quote_prefix(line, quote_depth));
        }
        self.segments.clear();
    }

    fn finish(mut self) -> Vec<Line<'static>> {
        self.flush_segments(false);
        if self.lines.is_empty() {
            self.lines.push(Line::from(""));
        }
        self.lines
    }
}

fn code_block_language(kind: CodeBlockKind<'_>) -> Option<String> {
    match kind {
        CodeBlockKind::Indented => None,
        CodeBlockKind::Fenced(language) => language.split_whitespace().next().map(str::to_string),
    }
}

fn inline_code_style() -> Style {
    Style::default().fg(theme::YELLOW).bg(theme::SURFACE_RAISED)
}

fn code_block_style() -> Style {
    Style::default().fg(theme::TEXT).bg(theme::SURFACE_RAISED)
}

fn code_label_line(language: &str, width: usize) -> Line<'static> {
    let text = format!(" code {language} ");
    Line::from(Span::styled(pad_to_width(&text, width), code_block_style()))
}

fn link_style() -> Style {
    Style::default().fg(theme::BLUE).underlined()
}

fn rule_line(width: usize) -> Line<'static> {
    let width = width.max(1);
    Line::from(Span::styled("-".repeat(width), theme::muted()))
}

fn quote_prefix_width(depth: usize) -> usize {
    if depth == 0 { 0 } else { 2 }
}

fn apply_quote_prefix(mut line: Line<'static>, depth: usize) -> Line<'static> {
    if depth == 0 {
        return line;
    }
    let mut spans = vec![Span::styled("> ", theme::muted())];
    spans.append(&mut line.spans);
    Line::from(spans)
}

fn wrap_code_line(line: &str, width: usize) -> Vec<Line<'static>> {
    let style = code_block_style();
    if width == 0 {
        return vec![Line::from(Span::styled(line.to_string(), style))];
    }

    if line.is_empty() {
        return vec![code_line("", width, style)];
    }

    let inner_width = width.saturating_sub(2).max(1);
    let mut lines = Vec::new();
    let mut remainder = line;
    while !remainder.is_empty() {
        let (chunk, rest, _) = split_to_width(remainder, inner_width);
        lines.push(code_line(chunk, width, style));
        remainder = rest;
    }
    lines
}

fn code_line(text: &str, width: usize, style: Style) -> Line<'static> {
    let text = format!(" {text}");
    Line::from(Span::styled(pad_to_width(&text, width), style))
}

fn wrap_styled_segments(segments: &[StyledSegment], width: usize) -> Vec<Line<'static>> {
    if width == 0 {
        let spans = segments
            .iter()
            .map(|segment| Span::styled(segment.text.clone(), segment.style))
            .collect::<Vec<_>>();
        return vec![Line::from(spans)];
    }

    let mut lines = Vec::new();
    let mut current = Vec::new();
    let mut current_width = 0;
    for segment in segments {
        push_wrapped_segment(segment, width, &mut lines, &mut current, &mut current_width);
    }
    if !current.is_empty() || lines.is_empty() {
        lines.push(Line::from(current));
    }
    lines
}

fn push_wrapped_segment(
    segment: &StyledSegment,
    width: usize,
    lines: &mut Vec<Line<'static>>,
    current: &mut Vec<Span<'static>>,
    current_width: &mut usize,
) {
    for part in split_segments(&segment.text) {
        let mut remainder = part;
        while !remainder.is_empty() {
            let part_width = UnicodeWidthStr::width(remainder);
            if *current_width + part_width <= width {
                push_span(current, remainder, segment.style);
                *current_width += part_width;
                break;
            }
            if *current_width > 0 {
                lines.push(Line::from(std::mem::take(current)));
                *current_width = 0;
                continue;
            }

            let (prefix, rest, prefix_width) = split_to_width(remainder, width);
            push_span(current, prefix, segment.style);
            lines.push(Line::from(std::mem::take(current)));
            *current_width = 0;
            if rest.is_empty() && prefix_width < width {
                break;
            }
            remainder = rest;
        }
    }
}

fn push_span(spans: &mut Vec<Span<'static>>, text: &str, style: Style) {
    if !text.is_empty() {
        spans.push(Span::styled(text.to_string(), style));
    }
}

fn pad_to_width(text: &str, width: usize) -> String {
    let text_width = UnicodeWidthStr::width(text);
    if text_width >= width {
        text.to_string()
    } else {
        format!("{text}{}", " ".repeat(width - text_width))
    }
}

#[cfg(test)]
mod tests {
    use ratatui::style::Modifier;

    use crate::components::markdown::render_markdown;
    use crate::theme;

    fn text(line: &ratatui::text::Line<'_>) -> String {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>()
    }

    #[test]
    fn renders_slack_links_once_and_preserves_literal_labels() {
        let url = "https://x.com/mdowd/status/2108691951705055448";
        let input = format!("<{url}|{url}> :thinking_face:");
        let lines = render_markdown(&input, 120);
        assert_eq!(text(&lines[0]), format!("{url} :thinking_face:"));
        assert_eq!(crate::message_links::copy_text(&input), text(&lines[0]));
        let urls = crate::message_links::links(&input);
        assert_eq!(urls.len(), 1);
        assert_eq!(urls[0].as_str(), url);
        assert!(
            lines[0]
                .spans
                .iter()
                .any(|span| { span.style.add_modifier.contains(Modifier::UNDERLINED) })
        );

        let labeled = render_markdown("<https://example.com/|a [label] *literal* &amp; more>", 120);
        assert_eq!(text(&labeled[0]), "a [label] *literal* & more");
    }

    #[test]
    fn slack_link_labels_with_backticks_remain_literal() {
        let input = "<https://example.com/a|click `here`>";
        let lines = render_markdown(input, 120);
        assert_eq!(text(&lines[0]), "click `here`");
        assert_eq!(
            crate::message_links::copy_text(input),
            "https://example.com/a"
        );
        assert_eq!(
            crate::message_links::links(input)[0].as_str(),
            "https://example.com/a"
        );
    }

    #[test]
    fn renders_fenced_code_block_without_fence_markers() {
        let lines = render_markdown("before\n```rust\nfn main() {}\n```\nafter", 20);
        let rendered = lines.iter().map(text).collect::<Vec<_>>();

        assert_eq!(rendered[0], "before");
        assert_eq!(rendered[1], " code rust          ");
        assert_eq!(rendered[2], " fn main() {}       ");
        assert_eq!(rendered[3], "after");
    }

    #[test]
    fn inline_code_uses_code_surface_style() {
        let lines = render_markdown("run `cargo test` now", 80);

        let code_text = lines[0]
            .spans
            .iter()
            .filter(|span| span.style.bg == Some(theme::SURFACE_RAISED))
            .map(|span| span.content.as_ref())
            .collect::<String>();

        assert_eq!(code_text, "cargo test");
    }

    #[test]
    fn preserves_soft_breaks_as_message_lines() {
        let lines = render_markdown("one\ntwo", 80);

        assert_eq!(
            lines.iter().map(text).collect::<Vec<_>>(),
            vec!["one", "two"]
        );
    }

    #[test]
    fn renders_lists_and_emphasis_markers_as_text_style() {
        let lines = render_markdown("- **ship** it\n- _test_ it", 80);

        assert_eq!(text(&lines[0]), "- ship it");
        assert_eq!(text(&lines[1]), "- test it");
        assert!(
            lines[0]
                .spans
                .iter()
                .any(|span| span.style.add_modifier.contains(Modifier::BOLD))
        );
    }

    #[test]
    fn wraps_code_blocks_with_background_padding() {
        let lines = render_markdown("```text\nabcdef\n```", 5);

        assert_eq!(text(&lines[0]), " code text ");
        assert_eq!(text(&lines[1]), " abc ");
        assert_eq!(text(&lines[2]), " def ");
        assert!(lines[1].spans[0].style.bg.is_some());
    }
}
