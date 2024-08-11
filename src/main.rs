//! mdBook preprocessor for file inclusion with shift
//!
//! Based on the links preprocessor in the main mdBook project.

use anyhow::Context;
use clap::{App, Arg, SubCommand};
use log::{error, warn};
use mdbook::utils::take_anchored_lines;
use mdbook::utils::take_lines;
use mdbook::{
    book::{Book, BookItem},
    errors::{Error, Result},
    preprocess::{CmdPreprocessor, Preprocessor, PreprocessorContext},
};
use once_cell::sync::Lazy;
use regex::{CaptureMatches, Captures, Regex};
use std::{
    fs, io,
    ops::{Bound, Range, RangeBounds, RangeFrom, RangeFull, RangeTo},
    path::{Path, PathBuf},
    process,
};

const ESCAPE_CHAR: char = '\\';
const MAX_LINK_NESTED_DEPTH: usize = 10;

fn main() -> Result<(), Error> {
    env_logger::init();
    let app = App::new(MdInclude::NAME)
        .about("An mdbook preprocessor which includes files with shift")
        .subcommand(
            SubCommand::with_name("supports")
                .arg(Arg::with_name("renderer").required(true))
                .about("Check whether a renderer is supported by this preprocessor"),
        );
    let matches = app.get_matches();

    if let Some(sub_args) = matches.subcommand_matches("supports") {
        let renderer = sub_args.value_of("renderer").expect("Required argument");
        let supported = MdInclude::supports_renderer(renderer);

        // Signal whether the renderer is supported by exiting with 1 or 0.
        if supported {
            process::exit(0);
        } else {
            process::exit(1);
        }
    } else {
        let (ctx, book) = CmdPreprocessor::parse_input(io::stdin())?;
        let pre = MdInclude::new(&ctx);

        let processed_book = pre.run(&ctx, book)?;
        serde_json::to_writer(io::stdout(), &processed_book)?;
    }
    Ok(())
}

/// A pre-processor for `{{#mdinclude}}` that acts like `{{#include}}` but updates relative links.
#[derive(Default)]
pub struct MdInclude;

impl MdInclude {
    const NAME: &'static str = "mdinclude";

    fn new(ctx: &PreprocessorContext) -> Self {
        if ctx.mdbook_version != mdbook::MDBOOK_VERSION {
            // We should probably use the `semver` crate to check compatibility
            // here...
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

    /// Indicate whether a renderer is supported.  This preprocessor can emit MarkDown so should support almost any
    /// renderer.
    fn supports_renderer(renderer: &str) -> bool {
        renderer != "not-supported"
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

                    let content = replace_all(&ch.content, base, chapter_path, 0);
                    ch.content = content;
                }
            }
        });
        Ok(book)
    }
}

fn replace_all<P1, P2>(s: &str, path: P1, source: P2, depth: usize) -> String
where
    P1: AsRef<Path>,
    P2: AsRef<Path>,
{
    // When replacing one thing in a string by something with a different length,
    // the indices after that will not correspond,
    // we therefore have to store the difference to correct this
    let path = path.as_ref();
    let source = source.as_ref();
    let mut previous_end_index = 0;
    let mut replaced = String::new();

    for link in find_links(s) {
        replaced.push_str(&s[previous_end_index..link.start_index]);
        match link.render_with_path(path) {
            Ok(mut new_content) => {
                if let Some(relative_path) = link.link_type.clone().relative_path(path) {
                    new_content = update_relative_links(&new_content, &path, &relative_path);
                }
                if depth < MAX_LINK_NESTED_DEPTH {
                    if let Some(rel_path) = link.link_type.relative_path(path) {
                        replaced.push_str(&replace_all(&new_content, rel_path, source, depth + 1));
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

                // This should make sure we include the raw `{{# ... }}` snippet
                // in the page content if there are any errors.
                previous_end_index = link.start_index;
            }
        }
    }

    replaced.push_str(&s[previous_end_index..]);
    replaced
}

/// This function updates relative links in `content` based on the provided `relative_path`
/// and `path`. For example, if you use `{{#mdinclude ./my_folder/README.md}}`, then links
/// in `README.md` will be updated with `my_folder`.
fn update_relative_links(content: &str, path: &Path, relative_path: &Path) -> String {
    // Strip the `path` prefix from `relative_path` to get the relative folder
    let Ok(relative_folder) = relative_path.strip_prefix(path) else {
        return content.to_owned();
    };

    // Regex to match Markdown image and link syntax
    let re = Regex::new(
        r#"(?x)
        !\[(.*?)\]\((./[^)]+)\)|           # Markdown image ![alt text](path)
        \[(.*?)\]\((./[^)]+)\)           # Markdown link [text](path)
        "#,
    )
    .unwrap();

    // Replace all matches using the regex
    let updated_content = re.replace_all(content, |caps: &regex::Captures| {
        // Extract the relative link
        let relative_link = if let Some(link) = caps.get(2) {
            link.as_str()
        } else {
            caps.get(4).map_or("", |m| m.as_str())
        };

        // Create a PathBuf from the relative_folder and the relative link
        let mut new_path = PathBuf::from(relative_folder);
        new_path.push(Path::new(relative_link));

        // Normalize the path to remove redundant components (like `./`)
        let updated_link = new_path.display().to_string().replace("\\", "/"); // Ensure Unix-style path separators

        // Determine the replacement based on the match
        if let Some(alt_text) = caps.get(1) {
            // Handle Markdown image with alt text
            format!("![{}]({})", alt_text.as_str(), updated_link)
        } else if let Some(text) = caps.get(3) {
            // Handle Markdown link
            format!("[{}]({})", text.as_str(), updated_link)
        } else {
            // In case something unexpected happens, just return the original match
            caps.get(0).unwrap().as_str().to_string()
        }
    });

    updated_content.into_owned()
}

#[derive(PartialEq, Debug, Clone)]
enum LinkType {
    Escaped,
    Include(PathBuf, RangeOrAnchor),
}

#[derive(PartialEq, Debug, Clone)]
enum RangeOrAnchor {
    Range(LineRange),
    Anchor(String),
}

// A range of lines specified with some include directive.
#[allow(clippy::enum_variant_names)] // The prefix can't be removed, and is meant to mirror the contained type
#[derive(PartialEq, Debug, Clone)]
enum LineRange {
    Range(Range<usize>),
    RangeFrom(RangeFrom<usize>),
    RangeTo(RangeTo<usize>),
    RangeFull(RangeFull),
}

impl RangeBounds<usize> for LineRange {
    fn start_bound(&self) -> Bound<&usize> {
        match self {
            LineRange::Range(r) => r.start_bound(),
            LineRange::RangeFrom(r) => r.start_bound(),
            LineRange::RangeTo(r) => r.start_bound(),
            LineRange::RangeFull(r) => r.start_bound(),
        }
    }

    fn end_bound(&self) -> Bound<&usize> {
        match self {
            LineRange::Range(r) => r.end_bound(),
            LineRange::RangeFrom(r) => r.end_bound(),
            LineRange::RangeTo(r) => r.end_bound(),
            LineRange::RangeFull(r) => r.end_bound(),
        }
    }
}

impl From<Range<usize>> for LineRange {
    fn from(r: Range<usize>) -> LineRange {
        LineRange::Range(r)
    }
}

impl From<RangeFrom<usize>> for LineRange {
    fn from(r: RangeFrom<usize>) -> LineRange {
        LineRange::RangeFrom(r)
    }
}

impl From<RangeTo<usize>> for LineRange {
    fn from(r: RangeTo<usize>) -> LineRange {
        LineRange::RangeTo(r)
    }
}

impl From<RangeFull> for LineRange {
    fn from(r: RangeFull) -> LineRange {
        LineRange::RangeFull(r)
    }
}

impl LinkType {
    fn relative_path<P: AsRef<Path>>(self, base: P) -> Option<PathBuf> {
        let base = base.as_ref();
        match self {
            LinkType::Escaped => None,
            LinkType::Include(p, _) => Some(return_relative_path(base, &p)),
        }
    }
}
fn return_relative_path<P: AsRef<Path>>(base: P, relative: P) -> PathBuf {
    base.as_ref()
        .join(relative)
        .parent()
        .expect("Included file should not be /")
        .to_path_buf()
}

fn parse_range_or_anchor(parts: Option<&str>) -> RangeOrAnchor {
    let mut parts = parts.unwrap_or("").splitn(3, ':').fuse();

    let next_element = parts.next();
    let start = if let Some(value) = next_element.and_then(|s| s.parse::<usize>().ok()) {
        // subtract 1 since line numbers usually begin with 1
        Some(value.saturating_sub(1))
    } else if let Some("") = next_element {
        None
    } else if let Some(anchor) = next_element {
        return RangeOrAnchor::Anchor(String::from(anchor));
    } else {
        None
    };

    let end = parts.next();
    // If `end` is empty string or any other value that can't be parsed as a usize, treat this
    // include as a range with only a start bound. However, if end isn't specified, include only
    // the single line specified by `start`.
    let end = end.map(|s| s.parse::<usize>());

    match (start, end) {
        (Some(start), Some(Ok(end))) => RangeOrAnchor::Range(LineRange::from(start..end)),
        (Some(start), Some(Err(_))) => RangeOrAnchor::Range(LineRange::from(start..)),
        (Some(start), None) => RangeOrAnchor::Range(LineRange::from(start..start + 1)),
        (None, Some(Ok(end))) => RangeOrAnchor::Range(LineRange::from(..end)),
        (None, None) | (None, Some(Err(_))) => RangeOrAnchor::Range(LineRange::from(RangeFull)),
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

    fn render_with_path<P: AsRef<Path>>(&self, base: P) -> Result<String> {
        let base = base.as_ref();
        match self.link_type {
            // omit the escape char
            LinkType::Escaped => Ok(self.link_text[1..].to_owned()),
            LinkType::Include(ref pat, ref range_or_anchor) => {
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
    // lazily compute following regex
    // r"\\\{\{#.*\}\}|\{\{#([a-zA-Z0-9]+)\s*([^}]+)\}\}")?;
    static RE: Lazy<Regex> = Lazy::new(|| {
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

    LinkIter(RE.captures_iter(contents))
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
        assert!(find_links(s).collect::<Vec<_>>() == vec![]);
    }

    #[test]
    fn test_find_links_partial_link() {
        let s = "Some random text with {{#playground...";
        assert!(find_links(s).collect::<Vec<_>>() == vec![]);
        let s = "Some random text with {{#include...";
        assert!(find_links(s).collect::<Vec<_>>() == vec![]);
        let s = "Some random text with \\{{#include...";
        assert!(find_links(s).collect::<Vec<_>>() == vec![]);
    }

    #[test]
    fn test_find_links_empty_link() {
        let s = "Some random text with {{#playground}} and {{#playground   }} {{}} {{#}}...";
        assert!(find_links(s).collect::<Vec<_>>() == vec![]);
    }

    #[test]
    fn test_find_links_unknown_link_type() {
        let s = "Some random text with {{#playgroundz ar.rs}} and {{#incn}} {{baz}} {{#bar}}...";
        assert!(find_links(s).collect::<Vec<_>>() == vec![]);
    }

    #[test]
    fn test_find_links_escaped_link() {
        let s = "Some random text with escaped playground \\{{#playground file.rs editable}} ...";

        let res = find_links(s).collect::<Vec<_>>();
        println!("\nOUTPUT: {:?}\n", res);

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
    fn update_relative_links_works() {
        let inputs_and_outputs = [
            (
                "My image here: ![my image](./.hidden/subfolder/image/image.png), and it is really cool!",
                "My image here: ![my image](with/subfolder/./.hidden/subfolder/image/image.png), and it is really cool!"
            ),
            (
                "My image here: [my link](./.hidden/subfolder/tests/test.rs), and it is really cool!",
                "My image here: [my link](with/subfolder/./.hidden/subfolder/tests/test.rs), and it is really cool!"
            ),
        ];
        let path = Path::new("/long/concrete/path/to/project/");
        let relative_path = Path::new("/long/concrete/path/to/project/with/subfolder/");

        for (input, output) in inputs_and_outputs.into_iter() {
            let final_content = update_relative_links(input, path, relative_path);

            assert_eq!(final_content, output)
        }
    }

    #[test]
    fn update_relative_links_skips_random_links() {
        let content =
            "My image here: `./.hidden/subfolder/image/image.png`, and it is really cool!";
        let path = Path::new("/long/concrete/path/to/project/");
        let relative_path = Path::new("/long/concrete/path/to/project/with/subfolder/");

        let final_content = update_relative_links(content, path, relative_path);

        // Unchanged
        assert_eq!(final_content, content)
    }
}
