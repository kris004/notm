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
        #[arg(long = "test-harness", alias = "automation")]
        automation: bool,
        #[arg(
            long = "test-harness-socket",
            alias = "automation-socket",
            value_name = "SOCKET"
        )]
        automation_socket: Option<PathBuf>,
        #[arg(
            long = "test-harness-token",
            alias = "automation-token",
            value_name = "TOKEN"
        )]
        automation_token: Option<String>,
        #[arg(long)]
        fixture: bool,
        #[arg(long, value_name = "MESSAGE_ID")]
        message_id: Option<String>,
    },
    #[command(about = "Print effective configuration as JSON with secrets redacted by default")]
    PrintConfig {
        #[arg(
            long,
            help = "Show unredacted secret values (unsafe for logs or shared output)"
        )]
        show_secrets: bool,
    },
    ProbeSend,
    FixtureSmoke,
    LiveReadonlySmoke,
    LiveSelfSend,
}

pub fn parse() -> Cli {
    Cli::parse()
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::{Cli, Command};

    #[test]
    fn launch_accepts_message_id_target() {
        let cli = Cli::try_parse_from(["notm", "launch", "--message-id", "abc@example.test"])
            .expect("launch --message-id should parse");

        match cli.command {
            Command::Launch { message_id, .. } => {
                assert_eq!(message_id.as_deref(), Some("abc@example.test"));
            }
            other => panic!("expected launch command, got {other:?}"),
        }
    }
}
