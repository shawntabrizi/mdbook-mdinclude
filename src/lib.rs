//! An mdBook preprocessor for better markdown file inclusion.
//!
//! Provides `{{#mdinclude}}` which works like `{{#include}}` but automatically
//! rewrites relative links in the included content so they resolve correctly
//! from the including file's location.

use anyhow::Context;
use log::{error, warn};
use mdbook::utils::{take_anchored_lines, take_lines};
use mdbook::{
    book::{Book, BookItem},
    errors::{Error, Result},
    preprocess::{Preprocessor, PreprocessorContext},
};
use regex::{CaptureMatches, Captures, Regex};
use std::{
    fs,
    ops::{Bound, Range, RangeBounds},
    path::{Path, PathBuf},
    sync::LazyLock,
};

const ESCAPE_CHAR: char = '\\';
const MAX_LINK_NESTED_DEPTH: usize = 10;

/// Regex for finding `{{#mdinclude ...}}` directives and escaped variants.
static LINK_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?x)              # insignificant whitespace mode
        \\\{\{\#.*?\}\}     # match escaped link (non-greedy)
        |                   # or
        \{\{\s*             # link opening parens and whitespace
        \#([a-zA-Z0-9_]+)   # link type
        \s+                 # separating whitespace
        ([^}]+)             # link target path and space separated properties
        \}\}                # link closing parens",
    )
    .unwrap()
});

/// Regex for matching inline markdown links and images, with optional titles.
///
/// Matches `![alt](path)`, `![alt](<path>)`, `![alt](path "title")`,
/// `[text](path)`, `[text](<path>)`, and `[text](path "title")`.
/// Angle-bracket destinations allow spaces in the path.
static MARKDOWN_LINK_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"(?x)
        !\[(.*?)\]\((<[^>]+>|[^)\s]+)(\s+(?:"[^"]*"|'[^']*'))?\)   # image
        |                                                              # or
        \[(.*?)\]\((<[^>]+>|[^)\s]+)(\s+(?:"[^"]*"|'[^']*'))?\)       # link
        "#,
    )
    .unwrap()
});

/// Regex for matching reference-style link definitions.
///
/// Matches `[label]: url` and `[label]: url "title"` at the start of a line.
static REF_LINK_DEF_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"(?xm)
        ^\[(.*?)\]:\s+(\S+)(\s+(?:"[^"]*"|'[^']*'))?$   # [ref]: url "title"
        "#,
    )
    .unwrap()
});

/// Returns true if `link` is a relative path (not an absolute URL, absolute
/// path, fragment reference, or URI scheme like `mailto:`, `tel:`, `data:`, etc.).
///
/// A URI scheme is `ALPHA *(ALPHA / DIGIT / "+" / "-" / ".") ":"` per RFC 3986.
/// Colons that appear *after* the first `/` are part of a filename (legal on
/// Unix/macOS) and do not indicate a scheme.
fn is_relative_link(link: &str) -> bool {
    if link.starts_with('/') || link.starts_with('#') {
        return false;
    }
    if let Some(colon_pos) = link.find(':') {
        let slash_pos = link.find('/');
        // A colon before any slash could be a URI scheme.
        if slash_pos.is_none() || colon_pos < slash_pos.unwrap() {
            let before_colon = &link[..colon_pos];
            if !before_colon.is_empty()
                && before_colon.as_bytes()[0].is_ascii_alphabetic()
                && before_colon
                    .bytes()
                    .skip(1)
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'.' | b'-'))
            {
                return false;
            }
        }
    }
    true
}

/// A preprocessor for `{{#mdinclude}}` that acts like `{{#include}}` but updates relative links.
#[derive(Default)]
pub struct MdInclude;

impl MdInclude {
    pub const NAME: &'static str = "mdinclude";

    pub fn new(ctx: &PreprocessorContext) -> Self {
        if ctx.mdbook_version != mdbook::MDBOOK_VERSION {
            warn!(
                "The {} plugin was built against version {} of mdbook, \
                 but we're being called from version {}",
                Self::NAME,
                mdbook::MDBOOK_VERSION,
                ctx.mdbook_version
            );
        }
        Self
    }
}

impl Preprocessor for MdInclude {
    fn name(&self) -> &str {
        Self::NAME
    }

    fn run(&self, ctx: &PreprocessorContext, mut book: Book) -> Result<Book, Error> {
        let src_dir = ctx.root.join(&ctx.config.book.src);

        book.for_each_mut(|section: &mut BookItem| {
            if let BookItem::Chapter(ch) = section {
                if let Some(chapter_path) = &ch.path {
                    let base = chapter_path
                        .parent()
                        .map(|dir| src_dir.join(dir))
                        .expect("All book items have a parent");

                    ch.content = replace_all(&ch.content, base, chapter_path, 0);
                }
            }
        });
        Ok(book)
    }

    fn supports_renderer(&self, _renderer: &str) -> bool {
        true
    }
}

fn replace_all<P1, P2>(s: &str, path: P1, source: P2, depth: usize) -> String
where
    P1: AsRef<Path>,
    P2: AsRef<Path>,
{
    let path = path.as_ref();
    let source = source.as_ref();
    let mut previous_end_index = 0;
    let mut replaced = String::new();

    for link in find_links(s) {
        replaced.push_str(&s[previous_end_index..link.start_index]);
        match link.render_with_path(path) {
            Ok(mut new_content) => {
                new_content = strip_frontmatter(&new_content);

                let rel_path = link.link_type.relative_path(path);

                if let Some(ref rp) = rel_path {
                    new_content = update_relative_links(&new_content, path, rp);
                }

                // Build the full content preceding this link (already-replaced
                // text + the slice of the original between the last replacement
                // and this link) so we can find the nearest parent heading.
                let context_before =
                    format!("{}{}", &replaced, &s[previous_end_index..link.start_index]);
                if let Some(parent_level) = find_parent_heading_level(&context_before) {
                    new_content = adjust_heading_levels(&new_content, parent_level);
                }

                if depth < MAX_LINK_NESTED_DEPTH {
                    if let Some(rp) = rel_path {
                        replaced.push_str(&replace_all(&new_content, rp, source, depth + 1));
                    } else {
                        replaced.push_str(&new_content);
                    }
                } else {
                    error!(
                        "Stack depth exceeded in {}. Check for cyclic includes",
                        source.display()
                    );
                }
                previous_end_index = link.end_index;
            }
            Err(e) => {
                error!("Error updating \"{}\", {}", link.link_text, e);
                for cause in e.chain().skip(1) {
                    warn!("Caused By: {}", cause);
                }
                // Leave the raw `{{# ... }}` snippet in the output on error.
                previous_end_index = link.start_index;
            }
        }
    }

    replaced.push_str(&s[previous_end_index..]);
    replaced
}

/// Strip YAML frontmatter from the beginning of content.
///
/// Frontmatter is a block delimited by `---` on its own line at the very start
/// of the content. For example:
///
/// ```text
/// ---
/// title: My Page
/// ---
///
/// Actual content here.
/// ```
///
/// Returns the content after the closing `---` (with leading whitespace trimmed).
/// If there is no frontmatter, the content is returned unchanged.
fn strip_frontmatter(content: &str) -> String {
    let trimmed = content.trim_start();
    if !trimmed.starts_with("---") {
        return content.to_owned();
    }

    // Find the closing `---` after the opening one.
    let after_opening = &trimmed[3..];
    // The opening `---` must be followed by a newline.
    let rest = if let Some(r) = after_opening.strip_prefix('\n') {
        r
    } else if let Some(r) = after_opening.strip_prefix("\r\n") {
        r
    } else {
        return content.to_owned();
    };

    // Search for closing `---` on its own line.
    for (i, line) in rest.lines().enumerate() {
        if line.trim() == "---" {
            // Find the byte offset past the closing `---\n`.
            let consumed: usize = rest.lines().take(i + 1).map(|l| l.len() + 1).sum();
            let remaining = &rest[consumed.min(rest.len())..];
            // Trim one leading newline if present, but preserve the rest.
            return if let Some(r) = remaining.strip_prefix('\n') {
                r.to_owned()
            } else if let Some(r) = remaining.strip_prefix("\r\n") {
                r.to_owned()
            } else {
                remaining.to_owned()
            };
        }
    }

    // No closing `---` found — not valid frontmatter, return unchanged.
    content.to_owned()
}

/// Detect a fenced code block delimiter on a line.
///
/// Returns `Some((char, count))` if the line starts with 3+ backticks or tildes.
/// The `char` is `b'`'` or `b'~'` and `count` is how many fence characters.
fn detect_fence(line: &str) -> Option<(u8, usize)> {
    let trimmed = line.trim_start();
    let first = *trimmed.as_bytes().first()?;
    if first != b'`' && first != b'~' {
        return None;
    }
    let count = trimmed.bytes().take_while(|&b| b == first).count();
    if count >= 3 {
        Some((first, count))
    } else {
        None
    }
}

/// Check whether `line` is a valid closing fence for a block opened with
/// `open_char` repeated `open_count` times. A closing fence must use the
/// same character, be at least as long, and have no content after it.
fn is_closing_fence(line: &str, open_char: u8, open_count: usize) -> bool {
    if let Some((ch, count)) = detect_fence(line) {
        if ch == open_char && count >= open_count {
            let trimmed = line.trim_start();
            return trimmed[count..].trim().is_empty();
        }
    }
    false
}

/// Update fence tracking state for a line. Returns `true` if `line` is
/// inside a code block (including fence lines themselves).
fn update_fence_state(fence: &mut Option<(u8, usize)>, line: &str) -> bool {
    if let Some((open_char, open_count)) = *fence {
        if is_closing_fence(line, open_char, open_count) {
            *fence = None;
        }
        true // closing fence line is still "inside" the block
    } else if let Some(f) = detect_fence(line) {
        *fence = Some(f);
        true // opening fence line is "inside" the block
    } else {
        false
    }
}

/// Find byte ranges in `content` that are inside fenced code blocks or inline
/// code spans. Matches inside these ranges should not be rewritten.
fn find_code_ranges(content: &str) -> Vec<Range<usize>> {
    let mut ranges = Vec::new();

    // Pass 1: fenced code blocks (tracking fence char + count).
    let mut fence: Option<(u8, usize)> = None;
    let mut block_start = 0;
    let mut offset = 0;
    for line in content.split('\n') {
        if let Some((open_char, open_count)) = fence {
            if is_closing_fence(line, open_char, open_count) {
                ranges.push(block_start..offset + line.len());
                fence = None;
            }
        } else if let Some(f) = detect_fence(line) {
            block_start = offset;
            fence = Some(f);
        }
        offset += line.len() + 1; // +1 for \n
    }
    if fence.is_some() {
        ranges.push(block_start..content.len());
    }

    // Pass 2: inline code spans (outside fenced blocks).
    let bytes = content.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'`' && !ranges.iter().any(|r| r.contains(&i)) {
            let start = i;
            let mut count = 0;
            while i < bytes.len() && bytes[i] == b'`' {
                count += 1;
                i += 1;
            }
            let closing = "`".repeat(count);
            if let Some(pos) = content[i..].find(&closing) {
                let end = i + pos + count;
                ranges.push(start..end);
                i = end;
            }
        } else {
            i += 1;
        }
    }

    ranges
}

/// Updates relative links in `content` to account for the included file's location.
///
/// Handles inline links/images (with optional titles) and reference-style link
/// definitions. Links inside fenced code blocks and inline code spans are left
/// unchanged.
fn update_relative_links(content: &str, path: &Path, relative_path: &Path) -> String {
    let Ok(relative_folder) = relative_path.strip_prefix(path) else {
        return content.to_owned();
    };

    // Pass 1: rewrite inline links and images.
    let code_ranges = find_code_ranges(content);
    let content = MARKDOWN_LINK_RE
        .replace_all(content, |caps: &regex::Captures| {
            let m = caps.get(0).unwrap();
            if code_ranges.iter().any(|r| r.contains(&m.start())) {
                return m.as_str().to_string();
            }

            let (is_image, alt_or_text, raw_url, title) =
                if let (Some(alt), Some(url)) = (caps.get(1), caps.get(2)) {
                    let title = caps.get(3).map_or("", |m| m.as_str());
                    (true, alt.as_str(), url.as_str(), title)
                } else if let (Some(text), Some(url)) = (caps.get(4), caps.get(5)) {
                    let title = caps.get(6).map_or("", |m| m.as_str());
                    (false, text.as_str(), url.as_str(), title)
                } else {
                    return m.as_str().to_string();
                };

            // Strip angle brackets if present, remember for reconstruction.
            let (link, is_angle) = if raw_url.starts_with('<') && raw_url.ends_with('>') {
                (&raw_url[1..raw_url.len() - 1], true)
            } else {
                (raw_url, false)
            };

            if !is_relative_link(link) {
                return m.as_str().to_string();
            }

            let new_path = normalize_path(&relative_folder.join(link));
            let updated_link = new_path.display().to_string().replace('\\', "/");

            let dest = if is_angle {
                format!("<{updated_link}>")
            } else {
                updated_link
            };

            if is_image {
                format!("![{alt_or_text}]({dest}{title})")
            } else {
                format!("[{alt_or_text}]({dest}{title})")
            }
        })
        .into_owned();

    // Pass 2: rewrite reference-style link definitions ([label]: url "title").
    let code_ranges = find_code_ranges(&content);
    REF_LINK_DEF_RE
        .replace_all(&content, |caps: &regex::Captures| {
            let m = caps.get(0).unwrap();
            if code_ranges.iter().any(|r| r.contains(&m.start())) {
                return m.as_str().to_string();
            }

            let label = caps.get(1).unwrap().as_str();
            let link = caps.get(2).unwrap().as_str();
            let title = caps.get(3).map_or("", |m| m.as_str());

            if !is_relative_link(link) {
                return m.as_str().to_string();
            }

            let new_path = normalize_path(&relative_folder.join(link));
            let updated_link = new_path.display().to_string().replace('\\', "/");

            format!("[{label}]: {updated_link}{title}")
        })
        .into_owned()
}

/// Normalize a path by resolving `.` and `..` components without touching the filesystem.
fn normalize_path(path: &Path) -> PathBuf {
    use std::path::Component;
    let mut components = Vec::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if matches!(components.last(), Some(Component::Normal(_))) {
                    components.pop();
                } else {
                    components.push(component);
                }
            }
            other => components.push(other),
        }
    }
    components.iter().collect()
}

/// Returns the heading level (1-6) of a line, or `None` if it's not a heading.
/// Only recognizes ATX-style headings (`# ...` through `###### ...`).
fn heading_level(line: &str) -> Option<usize> {
    let trimmed = line.trim_start();
    if !trimmed.starts_with('#') {
        return None;
    }
    let level = trimmed.bytes().take_while(|&b| b == b'#').count();
    if level <= 6 && trimmed.len() > level && trimmed.as_bytes()[level] == b' ' {
        Some(level)
    } else {
        None
    }
}

/// Find the heading level of the nearest heading before `position` in `content`,
/// skipping headings inside fenced code blocks.
fn find_parent_heading_level(content: &str) -> Option<usize> {
    let mut last_heading_level = None;
    let mut fence: Option<(u8, usize)> = None;

    for line in content.lines() {
        if !update_fence_state(&mut fence, line) {
            if let Some(level) = heading_level(line) {
                last_heading_level = Some(level);
            }
        }
    }

    last_heading_level
}

/// Adjust heading levels in `content` so they nest under `parent_level`.
///
/// For example, if `parent_level` is 2 (`##`) and the included content has
/// `## Foo` and `### Bar`, they become `### Foo` and `#### Bar`.
fn adjust_heading_levels(content: &str, parent_level: usize) -> String {
    // Find the minimum heading level, skipping code blocks.
    let mut min_level: Option<usize> = None;
    let mut fence: Option<(u8, usize)> = None;
    for line in content.lines() {
        if !update_fence_state(&mut fence, line) {
            if let Some(level) = heading_level(line) {
                min_level = Some(min_level.map_or(level, |m: usize| m.min(level)));
            }
        }
    }

    let Some(min_level) = min_level else {
        return content.to_owned();
    };

    // Offset so that the shallowest included heading becomes parent_level + 1.
    let offset = (parent_level + 1) as isize - min_level as isize;
    if offset == 0 {
        return content.to_owned();
    }

    let mut out = String::with_capacity(content.len() + 32);
    let mut fence: Option<(u8, usize)> = None;
    let mut first = true;
    for line in content.lines() {
        if !first {
            out.push('\n');
        }
        first = false;

        if !update_fence_state(&mut fence, line) {
            if let Some(level) = heading_level(line) {
                let trimmed = line.trim_start();
                let new_level = ((level as isize + offset).max(1) as usize).min(6);
                out.push_str(&"#".repeat(new_level));
                out.push_str(&trimmed[level..]);
                continue;
            }
        }
        out.push_str(line);
    }
    if content.ends_with('\n') {
        out.push('\n');
    }
    out
}

#[derive(PartialEq, Debug, Clone)]
enum LinkType {
    Escaped,
    Include(PathBuf, RangeOrAnchor),
}

impl LinkType {
    fn relative_path(&self, base: &Path) -> Option<PathBuf> {
        match self {
            LinkType::Escaped => None,
            LinkType::Include(p, _) => Some(
                base.join(p)
                    .parent()
                    .expect("Included file should not be /")
                    .to_path_buf(),
            ),
        }
    }
}

#[derive(PartialEq, Debug, Clone)]
enum RangeOrAnchor {
    Range(LineRange),
    Anchor(String),
}

/// A range of lines specified with an include directive.
#[derive(PartialEq, Debug, Clone)]
struct LineRange {
    start: Option<usize>,
    end: Option<usize>,
}

impl RangeBounds<usize> for LineRange {
    fn start_bound(&self) -> Bound<&usize> {
        match &self.start {
            Some(s) => Bound::Included(s),
            None => Bound::Unbounded,
        }
    }

    fn end_bound(&self) -> Bound<&usize> {
        match &self.end {
            Some(e) => Bound::Excluded(e),
            None => Bound::Unbounded,
        }
    }
}

fn parse_range_or_anchor(parts: Option<&str>) -> RangeOrAnchor {
    let mut parts = parts.unwrap_or("").splitn(3, ':').fuse();

    let next_element = parts.next();
    let start = if let Some(value) = next_element.and_then(|s| s.parse::<usize>().ok()) {
        // Subtract 1 since line numbers usually begin with 1.
        Some(value.saturating_sub(1))
    } else if let Some("") = next_element {
        None
    } else if let Some(anchor) = next_element {
        return RangeOrAnchor::Anchor(String::from(anchor));
    } else {
        None
    };

    let end = parts.next();
    // If `end` is empty string or unparseable, treat as open-ended range.
    // If `end` isn't specified at all, include only the single line from `start`.
    let end = end.map(|s| s.parse::<usize>());

    match (start, end) {
        (Some(s), Some(Ok(e))) => RangeOrAnchor::Range(LineRange {
            start: Some(s),
            end: Some(e),
        }),
        (Some(s), Some(Err(_))) => RangeOrAnchor::Range(LineRange {
            start: Some(s),
            end: None,
        }),
        (Some(s), None) => RangeOrAnchor::Range(LineRange {
            start: Some(s),
            end: Some(s + 1),
        }),
        (None, Some(Ok(e))) => RangeOrAnchor::Range(LineRange {
            start: None,
            end: Some(e),
        }),
        (None, None) | (None, Some(Err(_))) => RangeOrAnchor::Range(LineRange {
            start: None,
            end: None,
        }),
    }
}

fn parse_md_include_path(path: &str) -> LinkType {
    let mut parts = path.splitn(2, ':');
    let path = parts.next().unwrap().into();
    let range_or_anchor = parse_range_or_anchor(parts.next());
    LinkType::Include(path, range_or_anchor)
}

#[derive(PartialEq, Debug, Clone)]
struct Link<'a> {
    start_index: usize,
    end_index: usize,
    link_type: LinkType,
    link_text: &'a str,
}

impl<'a> Link<'a> {
    fn from_capture(cap: Captures<'a>) -> Option<Link<'a>> {
        let link_type = match (cap.get(0), cap.get(1), cap.get(2)) {
            (_, Some(typ), Some(rest)) => {
                let mut path_props = rest.as_str().split_whitespace();
                let file_arg = path_props.next();

                match (typ.as_str(), file_arg) {
                    ("mdinclude", Some(pth)) => Some(parse_md_include_path(pth)),
                    _ => None,
                }
            }
            (Some(mat), None, None) if mat.as_str().starts_with(ESCAPE_CHAR) => {
                Some(LinkType::Escaped)
            }
            _ => None,
        };

        link_type.and_then(|lnk_type| {
            cap.get(0).map(|mat| Link {
                start_index: mat.start(),
                end_index: mat.end(),
                link_type: lnk_type,
                link_text: mat.as_str(),
            })
        })
    }

    fn render_with_path(&self, base: &Path) -> Result<String> {
        match &self.link_type {
            LinkType::Escaped => Ok(self.link_text[1..].to_owned()),
            LinkType::Include(pat, range_or_anchor) => {
                let target = base.join(pat);
                fs::read_to_string(&target)
                    .map(|s| match range_or_anchor {
                        RangeOrAnchor::Range(range) => take_lines(&s, range.clone()),
                        RangeOrAnchor::Anchor(anchor) => take_anchored_lines(&s, anchor),
                    })
                    .with_context(|| {
                        format!(
                            "Could not read file for link {} ({})",
                            self.link_text,
                            target.display(),
                        )
                    })
            }
        }
    }
}

struct LinkIter<'a>(CaptureMatches<'a, 'a>);

impl<'a> Iterator for LinkIter<'a> {
    type Item = Link<'a>;
    fn next(&mut self) -> Option<Link<'a>> {
        for cap in &mut self.0 {
            if let Some(inc) = Link::from_capture(cap) {
                return Some(inc);
            }
        }
        None
    }
}

fn find_links(contents: &str) -> LinkIter<'_> {
    LinkIter(LINK_RE.captures_iter(contents))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_replace_all_escaped() {
        let start = r"
        Some text over here.
        ```hbs
        \{{#mdinclude 0:file.rs}} << an escaped link!
        ```";
        let end = r"
        Some text over here.
        ```hbs
        {{#mdinclude 0:file.rs}} << an escaped link!
        ```";
        assert_eq!(replace_all(start, "", "", 0), end);
    }

    #[test]
    fn test_find_links_no_link() {
        let s = "Some random text without link...";
        assert!(find_links(s).collect::<Vec<_>>().is_empty());
    }

    #[test]
    fn test_find_links_partial_link() {
        let s = "Some random text with {{#playground...";
        assert!(find_links(s).collect::<Vec<_>>().is_empty());
        let s = "Some random text with {{#include...";
        assert!(find_links(s).collect::<Vec<_>>().is_empty());
        let s = "Some random text with \\{{#include...";
        assert!(find_links(s).collect::<Vec<_>>().is_empty());
    }

    #[test]
    fn test_find_links_empty_link() {
        let s = "Some random text with {{#playground}} and {{#playground   }} {{}} {{#}}...";
        assert!(find_links(s).collect::<Vec<_>>().is_empty());
    }

    #[test]
    fn test_find_links_unknown_link_type() {
        let s = "Some random text with {{#playgroundz ar.rs}} and {{#incn}} {{baz}} {{#bar}}...";
        assert!(find_links(s).collect::<Vec<_>>().is_empty());
    }

    #[test]
    fn test_find_links_escaped_link() {
        let s = "Some random text with escaped playground \\{{#playground file.rs editable}} ...";
        let res = find_links(s).collect::<Vec<_>>();
        assert_eq!(
            res,
            vec![Link {
                start_index: 41,
                end_index: 74,
                link_type: LinkType::Escaped,
                link_text: "\\{{#playground file.rs editable}}",
            }]
        );
    }

    #[test]
    fn update_relative_links_with_dot_slash() {
        let path = Path::new("/long/concrete/path/to/project/");
        let relative_path = Path::new("/long/concrete/path/to/project/with/subfolder/");

        let input = "![my image](./.hidden/subfolder/image/image.png)";
        let expected = "![my image](with/subfolder/.hidden/subfolder/image/image.png)";
        assert_eq!(update_relative_links(input, path, relative_path), expected);

        let input = "[my link](./.hidden/subfolder/tests/test.rs)";
        let expected = "[my link](with/subfolder/.hidden/subfolder/tests/test.rs)";
        assert_eq!(update_relative_links(input, path, relative_path), expected);
    }

    #[test]
    fn update_relative_links_with_parent_dir() {
        let path = Path::new("/project/");
        let relative_path = Path::new("/project/sub/");

        let input = "![image](../other/image.png)";
        let expected = "![image](other/image.png)";
        assert_eq!(update_relative_links(input, path, relative_path), expected);
    }

    #[test]
    fn update_relative_links_with_bare_relative() {
        let path = Path::new("/project/");
        let relative_path = Path::new("/project/content/");

        let input = "![image](images/photo.png)";
        let expected = "![image](content/images/photo.png)";
        assert_eq!(update_relative_links(input, path, relative_path), expected);
    }

    #[test]
    fn update_relative_links_skips_absolute_urls() {
        let path = Path::new("/project/");
        let relative_path = Path::new("/project/content/");

        let input = "[link](https://example.com/page)";
        assert_eq!(update_relative_links(input, path, relative_path), input);

        let input = "[link](http://example.com/page)";
        assert_eq!(update_relative_links(input, path, relative_path), input);
    }

    #[test]
    fn update_relative_links_skips_absolute_paths() {
        let path = Path::new("/project/");
        let relative_path = Path::new("/project/content/");

        let input = "[link](/absolute/path)";
        assert_eq!(update_relative_links(input, path, relative_path), input);
    }

    #[test]
    fn update_relative_links_skips_fragments() {
        let path = Path::new("/project/");
        let relative_path = Path::new("/project/content/");

        let input = "[link](#section)";
        assert_eq!(update_relative_links(input, path, relative_path), input);
    }

    #[test]
    fn update_relative_links_skips_non_links() {
        let content =
            "My image here: `./.hidden/subfolder/image/image.png`, and it is really cool!";
        let path = Path::new("/long/concrete/path/to/project/");
        let relative_path = Path::new("/long/concrete/path/to/project/with/subfolder/");

        assert_eq!(update_relative_links(content, path, relative_path), content);
    }

    #[test]
    fn test_normalize_path() {
        assert_eq!(normalize_path(Path::new("a/./b/c")), PathBuf::from("a/b/c"));
        assert_eq!(normalize_path(Path::new("a/b/../c")), PathBuf::from("a/c"));
        assert_eq!(normalize_path(Path::new("./a/b")), PathBuf::from("a/b"));
        assert_eq!(
            normalize_path(Path::new("a/b/./c/../d")),
            PathBuf::from("a/b/d")
        );
    }

    #[test]
    fn test_heading_level() {
        assert_eq!(heading_level("# Title"), Some(1));
        assert_eq!(heading_level("## Section"), Some(2));
        assert_eq!(heading_level("###### Deep"), Some(6));
        assert_eq!(heading_level("####### Too deep"), None);
        assert_eq!(heading_level("#NoSpace"), None);
        assert_eq!(heading_level("Not a heading"), None);
        assert_eq!(heading_level("  ## Indented"), Some(2));
    }

    #[test]
    fn test_find_parent_heading_level() {
        assert_eq!(find_parent_heading_level("# Title\n\nSome text\n"), Some(1));
        assert_eq!(find_parent_heading_level("# Title\n## Section\n"), Some(2));
        assert_eq!(find_parent_heading_level("No headings here\n"), None);
        // Headings in code blocks should be ignored
        assert_eq!(
            find_parent_heading_level("# Real\n```\n## Fake\n```\n"),
            Some(1)
        );
    }

    #[test]
    fn test_adjust_heading_levels_basic() {
        let content = "## Section\n\nSome text\n\n### Sub\n";
        let result = adjust_heading_levels(content, 2);
        assert_eq!(result, "### Section\n\nSome text\n\n#### Sub\n");
    }

    #[test]
    fn test_adjust_heading_levels_no_headings() {
        let content = "Just some text\nNo headings here\n";
        assert_eq!(adjust_heading_levels(content, 2), content);
    }

    #[test]
    fn test_adjust_heading_levels_already_correct() {
        // Parent is h1, content starts at h2 — already correct, no adjustment
        let content = "## Already correct\n### Sub\n";
        assert_eq!(adjust_heading_levels(content, 1), content);
    }

    #[test]
    fn test_adjust_heading_levels_skips_code_blocks() {
        let content = "## Real heading\n\n```\n## Fake heading\n```\n";
        let result = adjust_heading_levels(content, 2);
        // Real heading should be adjusted, fake should not
        assert!(result.contains("### Real heading"));
        assert!(result.contains("## Fake heading"));
    }

    #[test]
    fn test_adjust_heading_levels_clamps_to_h6() {
        let content = "###### Deep\n";
        let result = adjust_heading_levels(content, 5);
        // Would want h7 but should clamp to h6
        assert!(result.starts_with("######"));
    }

    #[test]
    fn test_adjust_heading_levels_negative_offset() {
        // Parent is h1, included content has h4 and h5
        // Should become h2 and h3
        let content = "#### Deep\n##### Deeper\n";
        let result = adjust_heading_levels(content, 1);
        assert_eq!(result, "## Deep\n### Deeper\n");
    }

    #[test]
    fn test_strip_frontmatter_basic() {
        let content = "---\ntitle: Test\n---\n\nActual content\n";
        assert_eq!(strip_frontmatter(content), "Actual content\n");
    }

    #[test]
    fn test_strip_frontmatter_multiline() {
        let content = "---\ntitle: Test\ndescription: Stuff\ntags:\n  - a\n  - b\n---\n\n# Hello\n";
        assert_eq!(strip_frontmatter(content), "# Hello\n");
    }

    #[test]
    fn test_strip_frontmatter_no_frontmatter() {
        let content = "# Just a heading\n\nSome text\n";
        assert_eq!(strip_frontmatter(content), content);
    }

    #[test]
    fn test_strip_frontmatter_unclosed() {
        // Opening --- but no closing --- is not valid frontmatter
        let content = "---\ntitle: Test\nno closing\n";
        assert_eq!(strip_frontmatter(content), content);
    }

    #[test]
    fn test_strip_frontmatter_not_at_start() {
        // --- not at start of file is not frontmatter
        let content = "Some text\n---\ntitle: Test\n---\n";
        assert_eq!(strip_frontmatter(content), content);
    }

    #[test]
    fn test_strip_frontmatter_empty_frontmatter() {
        let content = "---\n---\n\nContent\n";
        assert_eq!(strip_frontmatter(content), "Content\n");
    }

    // --- Regression tests for audit findings ---

    #[test]
    fn normalize_path_preserves_leading_parent_dir() {
        // P1: leading .. must not be dropped
        assert_eq!(
            normalize_path(Path::new("../shared/img.png")),
            PathBuf::from("../shared/img.png")
        );
        assert_eq!(
            normalize_path(Path::new("../../other/file.md")),
            PathBuf::from("../../other/file.md")
        );
        // .. after a normal component still resolves
        assert_eq!(normalize_path(Path::new("a/../b")), PathBuf::from("b"));
    }

    #[test]
    fn update_relative_links_skips_fenced_code_blocks() {
        // P1: links inside fenced code blocks should not be rewritten
        let path = Path::new("/project/");
        let relative_path = Path::new("/project/content/");

        let input = "```md\n![image](images/photo.png)\n```\n";
        assert_eq!(update_relative_links(input, path, relative_path), input);
    }

    #[test]
    fn update_relative_links_skips_inline_code() {
        // P1: links inside inline code should not be rewritten
        let path = Path::new("/project/");
        let relative_path = Path::new("/project/content/");

        let input = "Use `[link](images/photo.png)` syntax.";
        assert_eq!(update_relative_links(input, path, relative_path), input);
    }

    #[test]
    fn update_relative_links_rewrites_outside_code() {
        // Links outside code should still be rewritten
        let path = Path::new("/project/");
        let relative_path = Path::new("/project/content/");

        let input = "```\n[skip](a.md)\n```\n\n[rewrite](b.md)\n";
        let result = update_relative_links(input, path, relative_path);
        assert!(result.contains("[skip](a.md)"), "got: {result}");
        assert!(result.contains("[rewrite](content/b.md)"), "got: {result}");
    }

    #[test]
    fn adjust_heading_levels_indented_heading() {
        // P2: indented headings should not be corrupted
        let content = "  ## Indented Title\n";
        let result = adjust_heading_levels(content, 2);
        assert_eq!(result, "### Indented Title\n");
    }

    #[test]
    fn is_relative_link_rejects_non_http_schemes() {
        // URI schemes should not be treated as relative
        assert!(!is_relative_link("mailto:user@example.com"));
        assert!(!is_relative_link("tel:+1234567890"));
        assert!(!is_relative_link("data:text/plain;base64,abc"));
        assert!(!is_relative_link("file:///path/to/file"));
        assert!(!is_relative_link("https://example.com"));
        assert!(!is_relative_link("http://example.com"));
        assert!(!is_relative_link("ftp://example.com"));
        // Relative paths should still pass
        assert!(is_relative_link("images/photo.png"));
        assert!(is_relative_link("./images/photo.png"));
        assert!(is_relative_link("../images/photo.png"));
        // Colons in filenames (valid on Unix/macOS) should be treated as relative
        assert!(is_relative_link("images/foo:bar.png"));
        assert!(is_relative_link("./foo:bar.png"));
    }

    // --- Regression tests for second audit ---

    #[test]
    fn update_relative_links_titled_link() {
        // P2: titled links should be rewritten with title preserved
        let path = Path::new("/project/");
        let relative_path = Path::new("/project/content/");

        let input = r#"[click here](images/photo.png "A nice photo")"#;
        let expected = r#"[click here](content/images/photo.png "A nice photo")"#;
        assert_eq!(update_relative_links(input, path, relative_path), expected);
    }

    #[test]
    fn update_relative_links_titled_image() {
        let path = Path::new("/project/");
        let relative_path = Path::new("/project/content/");

        let input = r#"![alt](images/photo.png "A nice photo")"#;
        let expected = r#"![alt](content/images/photo.png "A nice photo")"#;
        assert_eq!(update_relative_links(input, path, relative_path), expected);
    }

    #[test]
    fn update_relative_links_single_quoted_title() {
        let path = Path::new("/project/");
        let relative_path = Path::new("/project/content/");

        let input = "[link](images/photo.png 'A nice photo')";
        let expected = "[link](content/images/photo.png 'A nice photo')";
        assert_eq!(update_relative_links(input, path, relative_path), expected);
    }

    #[test]
    fn update_relative_links_reference_style() {
        // P2: reference-style link definitions should be rewritten
        let path = Path::new("/project/");
        let relative_path = Path::new("/project/content/");

        let input = "[logo]: images/logo.png\n";
        let expected = "[logo]: content/images/logo.png\n";
        assert_eq!(update_relative_links(input, path, relative_path), expected);
    }

    #[test]
    fn update_relative_links_reference_style_with_title() {
        let path = Path::new("/project/");
        let relative_path = Path::new("/project/content/");

        let input = "[logo]: images/logo.png \"Our logo\"\n";
        let expected = "[logo]: content/images/logo.png \"Our logo\"\n";
        assert_eq!(update_relative_links(input, path, relative_path), expected);
    }

    #[test]
    fn update_relative_links_reference_style_absolute_unchanged() {
        let path = Path::new("/project/");
        let relative_path = Path::new("/project/content/");

        let input = "[site]: https://example.com\n";
        assert_eq!(update_relative_links(input, path, relative_path), input);
    }

    #[test]
    fn escaped_includes_not_merged_on_same_line() {
        // P3: multiple escaped includes on one line should be separate matches
        let s = r"\{{#mdinclude a.md}} and \{{#mdinclude b.md}}";
        let links: Vec<_> = find_links(s).collect();
        assert_eq!(links.len(), 2, "Expected 2 escaped links, got: {links:?}");
        assert_eq!(links[0].link_text, r"\{{#mdinclude a.md}}");
        assert_eq!(links[1].link_text, r"\{{#mdinclude b.md}}");
    }

    // --- Regression tests for third audit ---

    #[test]
    fn mixed_fence_backtick_containing_tildes() {
        // A backtick fence should not be closed by tildes
        let path = Path::new("/project/");
        let relative_path = Path::new("/project/content/");

        let input = "```md\n~~~\n[link](a.md)\n~~~\n```\n[outside](b.md)\n";
        let result = update_relative_links(input, path, relative_path);
        assert!(
            result.contains("[link](a.md)"),
            "Link inside fence should be unchanged: {result}"
        );
        assert!(
            result.contains("[outside](content/b.md)"),
            "Link outside fence should be rewritten: {result}"
        );
    }

    #[test]
    fn four_backtick_fence_containing_triple_backticks() {
        // A 4-backtick fence should not be closed by 3 backticks
        let path = Path::new("/project/");
        let relative_path = Path::new("/project/content/");

        let input = "````\n```\n[link](a.md)\n```\n````\n[outside](b.md)\n";
        let result = update_relative_links(input, path, relative_path);
        assert!(
            result.contains("[link](a.md)"),
            "Link inside fence should be unchanged: {result}"
        );
        assert!(
            result.contains("[outside](content/b.md)"),
            "Link outside fence should be rewritten: {result}"
        );
    }

    #[test]
    fn heading_adjustment_respects_mixed_fences() {
        // Headings inside a 4-backtick fence should not be adjusted
        let content = "````\n```\n## Fake\n```\n````\n## Real\n";
        let result = adjust_heading_levels(content, 1);
        assert!(
            result.contains("## Fake"),
            "Heading inside fence should be unchanged: {result}"
        );
        assert!(
            result.contains("## Real"),
            "Heading outside should be adjusted: {result}"
        );
    }

    #[test]
    fn angle_bracket_link_rewritten() {
        // P3: angle-bracket link destinations should be rewritten
        let path = Path::new("/project/");
        let relative_path = Path::new("/project/content/");

        let input = "[link](<path with spaces/file.md>)";
        let expected = "[link](<content/path with spaces/file.md>)";
        assert_eq!(update_relative_links(input, path, relative_path), expected);
    }

    #[test]
    fn angle_bracket_image_rewritten() {
        let path = Path::new("/project/");
        let relative_path = Path::new("/project/content/");

        let input = "![alt](<images/my photo.png>)";
        let expected = "![alt](<content/images/my photo.png>)";
        assert_eq!(update_relative_links(input, path, relative_path), expected);
    }

    #[test]
    fn angle_bracket_absolute_unchanged() {
        let path = Path::new("/project/");
        let relative_path = Path::new("/project/content/");

        let input = "[link](<https://example.com/page>)";
        assert_eq!(update_relative_links(input, path, relative_path), input);
    }
}
