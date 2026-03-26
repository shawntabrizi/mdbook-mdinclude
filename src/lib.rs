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
    ops::{Bound, RangeBounds},
    path::{Path, PathBuf},
    sync::LazyLock,
};

const ESCAPE_CHAR: char = '\\';
const MAX_LINK_NESTED_DEPTH: usize = 10;

/// Regex for finding `{{#mdinclude ...}}` directives and escaped variants.
static LINK_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?x)              # insignificant whitespace mode
        \\\{\{\#.*\}\}      # match escaped link
        |                   # or
        \{\{\s*             # link opening parens and whitespace
        \#([a-zA-Z0-9_]+)   # link type
        \s+                 # separating whitespace
        ([^}]+)             # link target path and space separated properties
        \}\}                # link closing parens",
    )
    .unwrap()
});

/// Regex for matching all markdown links and images.
///
/// Matches `![alt](path)` and `[text](path)`. Filtering for relative-only
/// paths is done in the replacement logic since the regex crate does not
/// support lookahead.
static MARKDOWN_LINK_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"(?x)
        !\[(.*?)\]\(([^)\s]+)\)   # image ![alt](path)
        |                          # or
        \[(.*?)\]\(([^)\s]+)\)     # link [text](path)
        "#,
    )
    .unwrap()
});

/// Returns true if `link` is a relative path (not an absolute URL, absolute
/// path, or fragment reference).
fn is_relative_link(link: &str) -> bool {
    !link.starts_with('/') && !link.starts_with('#') && !link.contains("://")
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
                let rel_path = link.link_type.relative_path(path);

                if let Some(ref rp) = rel_path {
                    new_content = update_relative_links(&new_content, path, rp);
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

/// Updates relative links in `content` to account for the included file's location.
///
/// For example, if a file at `content/README.md` is included into a chapter at
/// the book root, a link like `![img](./images/photo.png)` becomes
/// `![img](content/images/photo.png)`.
fn update_relative_links(content: &str, path: &Path, relative_path: &Path) -> String {
    let Ok(relative_folder) = relative_path.strip_prefix(path) else {
        return content.to_owned();
    };

    MARKDOWN_LINK_RE
        .replace_all(content, |caps: &regex::Captures| {
            let (is_image, alt_or_text, link) =
                if let (Some(alt), Some(link)) = (caps.get(1), caps.get(2)) {
                    (true, alt.as_str(), link.as_str())
                } else if let (Some(text), Some(link)) = (caps.get(3), caps.get(4)) {
                    (false, text.as_str(), link.as_str())
                } else {
                    return caps.get(0).unwrap().as_str().to_string();
                };

            if !is_relative_link(link) {
                return caps.get(0).unwrap().as_str().to_string();
            }

            let new_path = normalize_path(&relative_folder.join(link));
            let updated_link = new_path.display().to_string().replace('\\', "/");

            if is_image {
                format!("![{alt_or_text}]({updated_link})")
            } else {
                format!("[{alt_or_text}]({updated_link})")
            }
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
                components.pop();
            }
            other => components.push(other),
        }
    }
    components.iter().collect()
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
}
