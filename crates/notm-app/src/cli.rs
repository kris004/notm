use std::path::PathBuf;

use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(name = "notm", version, about = "Native GTK4 Notmuch client")]
pub struct Cli {
    #[arg(
        long,
        global = true,
        value_name = "PATH",
        help = "Read notm configuration from PATH"
    )]
    pub config: Option<PathBuf>,
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    #[command(about = "Launch the GTK mail client")]
    Launch {
        #[arg(
            long = "test-harness",
            alias = "automation",
            help = "Enable the local developer test harness"
        )]
        automation: bool,
        #[arg(
            long = "test-harness-socket",
            alias = "automation-socket",
            value_name = "SOCKET",
            help = "Listen for test-harness requests on this Unix socket"
        )]
        automation_socket: Option<PathBuf>,
        #[arg(
            long = "test-harness-token",
            alias = "automation-token",
            value_name = "TOKEN",
            help = "Require this token on test-harness requests"
        )]
        automation_token: Option<String>,
        #[arg(long, help = "Use a disposable synthetic mailbox")]
        fixture: bool,
        #[arg(
            long,
            value_name = "MESSAGE_ID",
            help = "Open a specific Notmuch message ID after launch"
        )]
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
    #[command(about = "Validate the configured send helper without sending mail")]
    ProbeSend,
    #[command(about = "Run a disposable database and fake-send smoke test")]
    FixtureSmoke,
    #[command(about = "Run a read-only smoke test against the configured database")]
    LiveReadonlySmoke,
    #[command(about = "Send one real self-test message through the configured helper")]
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

    #[test]
    fn top_level_help_describes_every_subcommand() {
        let help = Cli::try_parse_from(["notm", "--help"])
            .expect_err("--help should stop parsing")
            .to_string();

        for description in [
            "Launch the GTK mail client",
            "Print effective configuration",
            "Validate the configured send helper",
            "Run a disposable database",
            "Run a read-only smoke test",
            "Send one real self-test message",
        ] {
            assert!(help.contains(description), "help omitted {description:?}");
        }
    }
}
