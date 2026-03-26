use mdbook::book::{Book, BookItem, Chapter};
use mdbook::preprocess::{Preprocessor, PreprocessorContext};
use mdbook_mdinclude::MdInclude;
use serde_json::json;
use std::fs;
use std::path::Path;
use tempfile::TempDir;

fn run_preprocessor(root: &Path, chapters: Vec<Chapter>) -> Book {
    let ctx: PreprocessorContext = serde_json::from_value(json!({
        "root": root.to_str().unwrap(),
        "config": {
            "book": {
                "src": "src"
            }
        },
        "renderer": "html",
        "mdbook_version": mdbook::MDBOOK_VERSION,
    }))
    .unwrap();
    let mut book = Book::default();
    book.sections = chapters.into_iter().map(BookItem::Chapter).collect();
    MdInclude.run(&ctx, book).unwrap()
}

fn get_chapter_content(book: &Book, index: usize) -> &str {
    match &book.sections[index] {
        BookItem::Chapter(ch) => &ch.content,
        _ => panic!("Expected chapter at index {index}"),
    }
}

#[test]
fn basic_include() {
    let tmp = TempDir::new().unwrap();
    let src = tmp.path().join("src");
    fs::create_dir_all(&src).unwrap();
    fs::write(src.join("included.md"), "Hello from included file!").unwrap();

    let ch = Chapter::new(
        "Test",
        "{{#mdinclude included.md}}".to_string(),
        "chapter.md",
        vec![],
    );

    let book = run_preprocessor(tmp.path(), vec![ch]);
    let content = get_chapter_content(&book, 0);
    assert!(
        content.contains("Hello from included file!"),
        "Expected included content, got: {content}"
    );
}

#[test]
fn relative_link_rewriting_dot_slash() {
    let tmp = TempDir::new().unwrap();
    let src = tmp.path().join("src");
    let content_dir = src.join("content");
    fs::create_dir_all(&content_dir).unwrap();
    fs::write(
        content_dir.join("included.md"),
        "![image](./images/photo.png)",
    )
    .unwrap();

    let ch = Chapter::new(
        "Test",
        "{{#mdinclude content/included.md}}".to_string(),
        "chapter.md",
        vec![],
    );

    let book = run_preprocessor(tmp.path(), vec![ch]);
    let content = get_chapter_content(&book, 0);
    assert!(
        content.contains("![image](content/images/photo.png)"),
        "Expected rewritten link, got: {content}"
    );
}

#[test]
fn relative_link_rewriting_parent_dir() {
    let tmp = TempDir::new().unwrap();
    let src = tmp.path().join("src");
    let content_dir = src.join("content");
    fs::create_dir_all(&content_dir).unwrap();
    fs::write(
        content_dir.join("included.md"),
        "![image](../images/photo.png)",
    )
    .unwrap();

    let ch = Chapter::new(
        "Test",
        "{{#mdinclude content/included.md}}".to_string(),
        "chapter.md",
        vec![],
    );

    let book = run_preprocessor(tmp.path(), vec![ch]);
    let content = get_chapter_content(&book, 0);
    assert!(
        content.contains("![image](images/photo.png)"),
        "Expected rewritten parent-dir link, got: {content}"
    );
}

#[test]
fn relative_link_rewriting_bare_path() {
    let tmp = TempDir::new().unwrap();
    let src = tmp.path().join("src");
    let content_dir = src.join("content");
    fs::create_dir_all(&content_dir).unwrap();
    fs::write(
        content_dir.join("included.md"),
        "![image](images/photo.png)",
    )
    .unwrap();

    let ch = Chapter::new(
        "Test",
        "{{#mdinclude content/included.md}}".to_string(),
        "chapter.md",
        vec![],
    );

    let book = run_preprocessor(tmp.path(), vec![ch]);
    let content = get_chapter_content(&book, 0);
    assert!(
        content.contains("![image](content/images/photo.png)"),
        "Expected rewritten bare path, got: {content}"
    );
}

#[test]
fn absolute_urls_not_rewritten() {
    let tmp = TempDir::new().unwrap();
    let src = tmp.path().join("src");
    let content_dir = src.join("content");
    fs::create_dir_all(&content_dir).unwrap();
    fs::write(
        content_dir.join("included.md"),
        "[link](https://example.com)\n[other](http://example.com)",
    )
    .unwrap();

    let ch = Chapter::new(
        "Test",
        "{{#mdinclude content/included.md}}".to_string(),
        "chapter.md",
        vec![],
    );

    let book = run_preprocessor(tmp.path(), vec![ch]);
    let content = get_chapter_content(&book, 0);
    assert!(
        content.contains("[link](https://example.com)"),
        "HTTPS link should not be rewritten, got: {content}"
    );
    assert!(
        content.contains("[other](http://example.com)"),
        "HTTP link should not be rewritten, got: {content}"
    );
}

#[test]
fn escaped_include_preserved() {
    let tmp = TempDir::new().unwrap();
    let src = tmp.path().join("src");
    fs::create_dir_all(&src).unwrap();

    let ch = Chapter::new(
        "Test",
        r"\{{#mdinclude file.md}}".to_string(),
        "chapter.md",
        vec![],
    );

    let book = run_preprocessor(tmp.path(), vec![ch]);
    let content = get_chapter_content(&book, 0);
    assert_eq!(content, "{{#mdinclude file.md}}");
}

#[test]
fn nested_includes() {
    let tmp = TempDir::new().unwrap();
    let src = tmp.path().join("src");
    let sub = src.join("sub");
    fs::create_dir_all(&sub).unwrap();

    fs::write(sub.join("inner.md"), "inner content").unwrap();
    fs::write(
        src.join("outer.md"),
        "before {{#mdinclude sub/inner.md}} after",
    )
    .unwrap();

    let ch = Chapter::new(
        "Test",
        "{{#mdinclude outer.md}}".to_string(),
        "chapter.md",
        vec![],
    );

    let book = run_preprocessor(tmp.path(), vec![ch]);
    let content = get_chapter_content(&book, 0);
    assert!(
        content.contains("before")
            && content.contains("inner content")
            && content.contains("after"),
        "Expected nested include content, got: {content}"
    );
}

#[test]
fn line_range_include() {
    let tmp = TempDir::new().unwrap();
    let src = tmp.path().join("src");
    fs::create_dir_all(&src).unwrap();
    fs::write(src.join("lines.md"), "line1\nline2\nline3\nline4\n").unwrap();

    // Include only lines 2-3 (1-indexed, end-exclusive: 2:3)
    let ch = Chapter::new(
        "Test",
        "{{#mdinclude lines.md:2:3}}".to_string(),
        "chapter.md",
        vec![],
    );

    let book = run_preprocessor(tmp.path(), vec![ch]);
    let content = get_chapter_content(&book, 0);
    assert!(
        content.contains("line2") && content.contains("line3"),
        "Expected lines 2-3, got: {content}"
    );
    assert!(
        !content.contains("line1") && !content.contains("line4"),
        "Should not contain lines 1 or 4, got: {content}"
    );
}

#[test]
fn mixed_links_in_included_file() {
    let tmp = TempDir::new().unwrap();
    let src = tmp.path().join("src");
    let content_dir = src.join("docs");
    fs::create_dir_all(&content_dir).unwrap();
    fs::write(
        content_dir.join("mixed.md"),
        "\
![relative](./img/photo.png)
[absolute](https://example.com)
[fragment](#section)
[bare](other/file.md)
![parent](../root-img.png)",
    )
    .unwrap();

    let ch = Chapter::new(
        "Test",
        "{{#mdinclude docs/mixed.md}}".to_string(),
        "chapter.md",
        vec![],
    );

    let book = run_preprocessor(tmp.path(), vec![ch]);
    let content = get_chapter_content(&book, 0);

    // Relative links should be rewritten
    assert!(
        content.contains("![relative](docs/img/photo.png)"),
        "got: {content}"
    );
    assert!(
        content.contains("[bare](docs/other/file.md)"),
        "got: {content}"
    );
    assert!(
        content.contains("![parent](root-img.png)"),
        "got: {content}"
    );

    // Absolute and fragment links should be untouched
    assert!(
        content.contains("[absolute](https://example.com)"),
        "got: {content}"
    );
    assert!(content.contains("[fragment](#section)"), "got: {content}");
}

#[test]
fn heading_level_adjustment() {
    let tmp = TempDir::new().unwrap();
    let src = tmp.path().join("src");
    fs::create_dir_all(&src).unwrap();
    fs::write(
        src.join("install.md"),
        "Here are the instructions:\n\n## MacOS\n\nTo install on mac...\n\n## Linux\n\nTo install on linux...\n",
    )
    .unwrap();

    let ch = Chapter::new(
        "Test",
        "# My Project\n\nMy project is cool.\n\n## Installation\n\n{{#mdinclude install.md}}"
            .to_string(),
        "chapter.md",
        vec![],
    );

    let book = run_preprocessor(tmp.path(), vec![ch]);
    let content = get_chapter_content(&book, 0);
    // ## MacOS should become ### MacOS (nested under ## Installation)
    assert!(
        content.contains("### MacOS"),
        "Expected ### MacOS, got: {content}"
    );
    assert!(
        content.contains("### Linux"),
        "Expected ### Linux, got: {content}"
    );
    // Should NOT contain the original ## level (check at line boundary)
    assert!(
        !content.contains("\n## MacOS"),
        "Should not have ## MacOS as a heading, got: {content}"
    );
}

#[test]
fn heading_adjustment_preserves_hierarchy() {
    let tmp = TempDir::new().unwrap();
    let src = tmp.path().join("src");
    fs::create_dir_all(&src).unwrap();
    fs::write(src.join("nested.md"), "## Top\n\n### Sub\n\n#### Deep\n").unwrap();

    let ch = Chapter::new(
        "Test",
        "### Context\n\n{{#mdinclude nested.md}}".to_string(),
        "chapter.md",
        vec![],
    );

    let book = run_preprocessor(tmp.path(), vec![ch]);
    let content = get_chapter_content(&book, 0);
    // h2 -> h4, h3 -> h5, h4 -> h6 (parent is h3, offset = 4-2 = +2)
    assert!(content.contains("#### Top"), "got: {content}");
    assert!(content.contains("##### Sub"), "got: {content}");
    assert!(content.contains("###### Deep"), "got: {content}");
}

#[test]
fn no_heading_adjustment_without_parent() {
    let tmp = TempDir::new().unwrap();
    let src = tmp.path().join("src");
    fs::create_dir_all(&src).unwrap();
    fs::write(src.join("intro.md"), "# Welcome\n\nHello!\n").unwrap();

    let ch = Chapter::new(
        "Test",
        "{{#mdinclude intro.md}}".to_string(),
        "chapter.md",
        vec![],
    );

    let book = run_preprocessor(tmp.path(), vec![ch]);
    let content = get_chapter_content(&book, 0);
    // No parent heading, so # Welcome should stay as-is
    assert!(
        content.contains("# Welcome"),
        "Should keep original heading level, got: {content}"
    );
}
