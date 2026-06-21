use std::path::PathBuf;

use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(name = "notm", version, about = "Native GTK4 Notmuch client")]
pub struct Cli {
    #[arg(long, global = true)]
    pub config: Option<PathBuf>,
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    Launch {
        #[arg(long)]
        automation: bool,
        #[arg(long)]
        automation_socket: Option<PathBuf>,
        #[arg(long)]
        automation_token: Option<String>,
        #[arg(long)]
        fixture: bool,
    },
    PrintConfig,
    ProbeSend,
    FixtureSmoke,
    LiveReadonlySmoke,
    LiveSelfSend,
}

pub fn parse() -> Cli {
    Cli::parse()
}
