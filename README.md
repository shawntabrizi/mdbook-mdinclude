# mdbook-mdinclude

A preprocessor to better handle including markdown files.

## Features

The only feature at this point is to update relative links from included markdown files.

This can be important if your markdown file includes some images that you want to continue to work after you have included the markdown file.

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
