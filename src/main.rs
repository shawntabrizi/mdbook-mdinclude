use clap::{Parser, Subcommand};
use mdbook::preprocess::{CmdPreprocessor, Preprocessor};
use mdbook_mdinclude::MdInclude;
use std::{io, process};

#[derive(Parser)]
#[command(about = "An mdBook preprocessor for better markdown file inclusion.")]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Check whether a renderer is supported by this preprocessor
    Supports { renderer: String },
}

fn main() -> anyhow::Result<()> {
    env_logger::init();
    let cli = Cli::parse();

    match cli.command {
        Some(Commands::Supports { renderer }) => {
            let supported = MdInclude.supports_renderer(&renderer);
            process::exit(if supported { 0 } else { 1 });
        }
        None => {
            let (ctx, book) = CmdPreprocessor::parse_input(io::stdin())?;
            let pre = MdInclude::new(&ctx);
            let processed_book = pre.run(&ctx, book)?;
            serde_json::to_writer(io::stdout(), &processed_book)?;
        }
    }
    Ok(())
}
