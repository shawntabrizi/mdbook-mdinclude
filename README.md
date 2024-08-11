# mdbook-mdinclude

An mdBook preprocessor for better markdown file inclusion.

## Features

This `mdinclude` plugin will perform additional preprocessor steps for markdown files compared to the default `include` plugin.

### Update Relative Links

This can be important if your markdown file includes some images/links that you want to continue to work after you have included the markdown file. This is a problem when using the default `{{#include }}` links.

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

![my image](./content/images/image.png)
```

So the link is updated to the correct path to the file.

### Update Header Level

> [!NOTE]
> This feature is not yet implemented, but is planned.

When including an external markdown, we update the heading level to match the relative heading level where the markdown is included.

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

{{#mdinclude ./install.md}}
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

So as you can see, it introduces an additional heading level to the `MacOS` header.

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
