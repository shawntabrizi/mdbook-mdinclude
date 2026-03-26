# mdbook-mdinclude

An mdBook preprocessor for better markdown file inclusion.

## Features

This `mdinclude` plugin will perform additional preprocessor steps for markdown files compared to the default `include` plugin.

### Update Relative Links

When you include a markdown file from a subdirectory, any relative links (images, links) in that file are automatically rewritten so they resolve correctly from the including file's location.

This handles `./` paths, `../` paths, and bare relative paths like `images/photo.png`. Absolute URLs (`https://...`), absolute paths (`/...`), and fragment links (`#...`) are left untouched.

For example, imagine you have the following folder structure:

```text
my_project/
├─ README.md
├─ content/
│  ├─ include_me.md
│  ├─ images/
│  │  ├─ image.png
```

In `README.md`, you write the following:

```md
Here is the content of `include_me.md`:

{{#mdinclude content/include_me.md}}
```

In `include_me.md` you have the following:

```md
Check out this cool image:

![my image](images/image.png)
```

So the final rendered `README.md` page will be:

```md
Here is the content of `include_me.md`:

Check out this cool image:

![my image](content/images/image.png)
```

The link is updated to the correct path relative to the including file.

### Update Header Level

When including an external markdown file, heading levels are automatically adjusted to nest under the heading where the include is placed. This preserves the document hierarchy without requiring you to manually edit the included file.

For example, imagine you have the following folder structure:

```text
my_project/
├─ README.md
├─ install.md
```

In `README.md` you have:

```md
# My Cool Project

My project is really cool.

## Installation Instructions

{{#mdinclude install.md}}
```

In `install.md` you have:

```md
Here are the instructions to install this project:

## MacOS

To install this on mac...
```

The final output will be:

```md
# My Cool Project

My project is really cool.

## Installation Instructions

Here are the instructions to install this project:

### MacOS

To install this on mac...
```

The `## MacOS` heading becomes `### MacOS` because it is nested under the `##` heading where the include was placed. All heading levels in the included file are shifted by the same amount, preserving the relative hierarchy.

If there is no heading before the include directive, headings are left unchanged.

### Strip Frontmatter

Many markdown files (especially README files from other projects) start with YAML frontmatter:

```md
---
title: My Crate
description: Something cool
---

# My Crate

Actual content here.
```

When included with `{{#mdinclude}}`, the frontmatter block is automatically stripped. Only the content after the closing `---` is included. If there is no frontmatter, the file is included as-is.

## Supported Syntax and Known Limitations

Link rewriting covers:

- Inline links: `[text](url)` and `![alt](url)`
- Titled links: `[text](url "title")` and `![alt](url 'title')`
- Reference-style definitions: `[label]: url` and `[label]: url "title"`
- Code awareness: links inside fenced code blocks and inline code spans are intentionally skipped

Not currently supported:

- Autolinks (`<url>`) and raw URLs in text
- HTML `<a>` / `<img>` tags
- Nested brackets in link text (e.g., `[[text]](url)`)

If you need full Markdown-aware rewriting, consider pairing this tool with a Markdown AST-based preprocessor.

## Installation & Setup

This preprocessor can be installed with Cargo:

```console
cargo install mdbook-mdinclude
```

Add the following line to your `book.toml` file:

```toml
[preprocessor.mdinclude]
```

Now you can use the `mdinclude` links in your book.
