//! `mc` — Mission Control CLI.
//!
//! Subcommands:
//!   status     Print the "needs-you" lane as a table (Phase 1)
//!   serve      Run the daemon (Phase 2)
//!   tui        Launch the TUI client (Phase 2)

mod daemon;
mod status;
mod tui;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "mc", version, about = "Mission Control — birds-eye view over coding-agent panes")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Print the "needs-you" lane — all panes sorted by attention
    Status,
    /// Start the Mission Control daemon
    Serve,
    /// Launch the TUI client
    Tui,
}

fn main() {
    let cli = Cli::parse();

    match cli.command {
        Commands::Status => status::run(),
        Commands::Serve => daemon::run(),
        Commands::Tui => {
            if let Err(e) = tui::run() {
                eprintln!("mc: tui error: {e}");
                std::process::exit(1);
            }
        }
    }
}