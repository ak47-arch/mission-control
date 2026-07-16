//! `mc` — Mission Control CLI.
//!
//! Subcommands:
//!   status     Print the "needs-you" lane as a table (Phase 1)
//!   serve      Run the daemon (Phase 2)
//!   tui        Launch the TUI client (Phase 2)

mod daemon;
mod diagnose;
mod status;
mod tui;
mod web;

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
    /// Diagnose session-to-pane mapping and orphaned sessions
    Diagnose,
    /// Start the Mission Control daemon
    Serve,
    /// Launch the TUI client
    Tui,
    /// Start the web dashboard (HTTP + SSE)
    Web {
        /// Port to listen on (default: 9876)
        #[arg(short, long, default_value = "9876")]
        port: u16,
    },
}

fn main() {
    let cli = Cli::parse();

    match cli.command {
        Commands::Status => status::run(),
        Commands::Diagnose => diagnose::run(),
        Commands::Serve => daemon::run(),
        Commands::Tui => {
            if let Err(e) = tui::run() {
                eprintln!("mc: tui error: {e}");
                std::process::exit(1);
            }
        }
        Commands::Web { port } => {
            let rt = tokio::runtime::Runtime::new().unwrap();
            if let Err(e) = rt.block_on(web::run(port)) {
                eprintln!("mc: web error: {e}");
                std::process::exit(1);
            }
        }
    }
}