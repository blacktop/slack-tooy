use std::borrow::Cow;
use std::ops::Range;

use linkify::{LinkFinder, LinkKind};
use pulldown_cmark::{CowStr, Event, LinkType, Parser, Tag};
use reqwest::Url;

const MAX_MESSAGE_BYTES: usize = 256 * 1024;
const MAX_URL_BYTES: usize = 8192;
const MAX_LINKS: usize = 128;

#[derive(Clone, Copy)]
enum LinkFormat {
    Markdown,
    Clipboard,
}

pub fn render_text(input: &str) -> Cow<'_, str> {
    rewrite_slack_links(input, LinkFormat::Markdown)
}

pub fn copy_text(input: &str) -> Cow<'_, str> {
    rewrite_slack_links(input, LinkFormat::Clipboard)
}

/// Keep destinations suitable for opening in a browser. URL parsing alone
/// accepts inputs such as `https:example.com` and strips some control bytes.
pub fn browser_url(input: &str) -> Option<Url> {
    if input.len() > MAX_URL_BYTES
        || input
            .chars()
            .any(|ch| ch.is_control() || ch.is_whitespace())
    {
        return None;
    }
    let (scheme, _) = input.split_once("://")?;
    if !scheme.eq_ignore_ascii_case("https") && !scheme.eq_ignore_ascii_case("http") {
        return None;
    }
    let url = Url::parse(input).ok()?;
    url.host_str()?;
    Some(url)
}

/// Find distinct web destinations in source order, without treating a link's
/// display label or a code sample as a second destination.
pub fn links(input: &str) -> Vec<Url> {
    if input.len() > MAX_MESSAGE_BYTES {
        return Vec::new();
    }
    let markdown = render_text(input);
    let mut finder = LinkFinder::new();
    finder.kinds(&[LinkKind::Url]);
    let mut urls = Vec::new();
    let mut cursor = 0;
    // Scan source slices instead of Text events: Markdown can split a bare URL
    // at punctuation that it interprets as emphasis. Reference definitions are
    // skipped here; their destinations are collected only where used as links.
    for (range, destination) in markdown_spans(&markdown, true) {
        if range.start < cursor {
            continue;
        }
        if let Some(before) = markdown.get(cursor..range.start) {
            find_bare_urls(&finder, before, &mut urls);
        }
        if let Some(destination) = destination {
            push_url(&mut urls, &destination);
        }
        cursor = range.end;
        if urls.len() >= MAX_LINKS {
            return urls;
        }
    }
    if let Some(rest) = markdown.get(cursor..) {
        find_bare_urls(&finder, rest, &mut urls);
    }
    urls
}

fn markdown_spans(input: &str, include_autolinks: bool) -> Vec<(Range<usize>, Option<CowStr<'_>>)> {
    let parser = Parser::new(input);
    let mut spans: Vec<_> = parser
        .reference_definitions()
        .iter()
        .map(|(_, definition)| (definition.span.clone(), None))
        .collect();
    for (event, range) in parser.into_offset_iter() {
        let destination = match event {
            Event::Start(Tag::Link {
                link_type: LinkType::Autolink,
                ..
            }) if !include_autolinks => continue,
            Event::Start(Tag::Link { dest_url, .. } | Tag::Image { dest_url, .. }) => {
                include_autolinks.then_some(dest_url)
            }
            Event::Code(_) | Event::Start(Tag::CodeBlock(_)) => None,
            _ => continue,
        };
        spans.push((range, destination));
    }
    spans.sort_unstable_by_key(|(range, _)| (range.start, std::cmp::Reverse(range.end)));
    spans
}

fn find_bare_urls(finder: &LinkFinder, input: &str, urls: &mut Vec<Url>) {
    for link in finder.links(&decode_slack_entities(input)) {
        if urls.len() >= MAX_LINKS {
            break;
        }
        push_url(urls, link.as_str());
    }
}

fn push_url(urls: &mut Vec<Url>, input: &str) {
    if urls.len() < MAX_LINKS
        && let Some(url) = browser_url(input)
        && !urls.contains(&url)
    {
        urls.push(url);
    }
}

fn rewrite_slack_links(input: &str, format: LinkFormat) -> Cow<'_, str> {
    if input.len() > MAX_MESSAGE_BYTES || !input.contains('<') {
        return Cow::Borrowed(input);
    }
    let mut output = String::with_capacity(input.len());
    // Outer code and Markdown links win over Slack tags inside them. Markdown
    // syntax inside a Slack label is literal and cannot protect that tag.
    let mut protected = markdown_spans(input, false)
        .into_iter()
        .map(|(range, _)| range)
        .peekable();
    let mut cursor = 0;
    for link in slack_links(input) {
        while protected
            .peek()
            .is_some_and(|range| range.end <= link.range.start)
        {
            protected.next();
        }
        if protected
            .peek()
            .is_some_and(|range| range.start <= link.range.start)
        {
            continue;
        }
        if let Some(before) = input.get(cursor..link.range.start) {
            output.push_str(before);
        }
        write_link(&link, format, &mut output);
        cursor = link.range.end;
        while protected
            .peek()
            .is_some_and(|range| range.start < link.range.end)
        {
            protected.next();
        }
    }
    if let Some(rest) = input.get(cursor..) {
        output.push_str(rest);
    }
    if output == input {
        Cow::Borrowed(input)
    } else {
        Cow::Owned(output)
    }
}

struct SlackLink<'a> {
    range: Range<usize>,
    url: Url,
    label: &'a str,
}

fn slack_links(input: &str) -> impl Iterator<Item = SlackLink<'_>> {
    let mut offset: usize = 0;
    input.split_inclusive('>').filter_map(move |part| {
        let part_start = offset;
        offset = offset.saturating_add(part.len());
        let (prefix, body) = part.strip_suffix('>')?.rsplit_once('<')?;
        let (destination, label) = body
            .split_once('|')
            .map_or((body, body), |(url, label)| (url, label));
        if destination.len() > MAX_URL_BYTES || label.len() > MAX_URL_BYTES {
            return None;
        }
        Some(SlackLink {
            range: part_start.checked_add(prefix.len())?..offset,
            url: browser_url(&decode_slack_entities(destination))?,
            label,
        })
    })
}

fn write_link(link: &SlackLink<'_>, format: LinkFormat, output: &mut String) {
    match format {
        LinkFormat::Clipboard => output.push_str(link.url.as_str()),
        LinkFormat::Markdown => {
            let label = decode_slack_entities(link.label);
            let label = if label.is_empty() || label.chars().any(char::is_control) {
                link.url.as_str()
            } else {
                &label
            };
            output.push('[');
            for ch in label.chars() {
                if ch.is_ascii_punctuation() {
                    output.push('\\');
                }
                output.push(ch);
            }
            output.push_str("](<");
            output.push_str(&link.url.as_str().replace('&', "&amp;"));
            output.push_str(">)");
        }
    }
}

fn decode_slack_entities(input: &str) -> String {
    input
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

#[cfg(test)]
#[expect(clippy::expect_used, reason = "tests")]
mod tests {
    use crate::message_links::{MAX_LINKS, browser_url, copy_text, links, render_text};

    #[test]
    fn finds_real_destinations_and_deduplicates_without_following_labels() {
        let input = "<https://example.com/a?x=1&amp;y=2|https://other.test/> \
                     [another](https://example.com/b) \
                     <https://example.com/a?x=1&amp;y=2> \
                     (https://example.com/wiki/Thing_(part)).";
        let urls: Vec<_> = links(input).into_iter().map(String::from).collect();
        assert_eq!(
            urls,
            [
                "https://example.com/a?x=1&y=2",
                "https://example.com/b",
                "https://example.com/wiki/Thing_(part)"
            ]
        );
    }

    #[test]
    fn code_and_non_web_slack_markup_remain_literal() {
        let input = "`<https://example.com/|sample>`\n\n```text\n<https://example.com/|sample>\n```\n<@U123> <#C123|general>";
        assert_eq!(render_text(input), input);
        assert_eq!(copy_text(input), input);
        assert_eq!(links(input), Vec::<reqwest::Url>::new());
    }

    #[test]
    fn existing_markdown_destinations_are_preserved() {
        let input = "[label](<https://example.com/a?x=1&amp;amp;y=2>)";
        assert_eq!(render_text(input), input);
        assert_eq!(copy_text(input), input);
        assert_eq!(
            links(input)[0].as_str(),
            "https://example.com/a?x=1&amp;y=2"
        );

        let reference = "[label][post]\n\n[post]: <https://example.com/post>";
        assert_eq!(render_text(reference), reference);
        assert_eq!(copy_text(reference), reference);
        assert_eq!(links(reference)[0].as_str(), "https://example.com/post");
    }

    #[test]
    fn reference_definitions_do_not_expose_hidden_links() {
        let input = "[shown][ref]\n\n[ref]: https://example.com/real \"https://other.test/hidden\"\n[unused]: https://other.test/invisible";
        let urls: Vec<_> = links(input).into_iter().map(String::from).collect();
        assert_eq!(urls, ["https://example.com/real"]);
    }

    #[test]
    fn browser_targets_reject_other_schemes_and_control_bytes() {
        for input in [
            "file:///tmp/test",
            "javascript:alert(1)",
            "https:example.com",
            "https://example.com/\nfile",
            "--help",
        ] {
            assert!(browser_url(input).is_none(), "accepted {input:?}");
        }
    }

    #[test]
    fn link_count_is_bounded() {
        use std::fmt::Write;

        let mut input = String::new();
        for index in 0..MAX_LINKS + 10 {
            write!(input, "<https://example.com/{index}> ").expect("write to string");
        }
        assert_eq!(links(&input).len(), MAX_LINKS);
    }
}
