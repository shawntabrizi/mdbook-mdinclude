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

{{#mdinclude ./content/include_me.md}}
```

In `include_me.md` you have the following:

```md
Check out this cool image:

![my image](./images/image.png)
```

So the final rendered `README.md` page will be:

```md
Here is the content of `include_me.md`:

Check out this cool image:

![my image](content/images/image.png)
```

The link is updated to the correct path relative to the including file.

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
