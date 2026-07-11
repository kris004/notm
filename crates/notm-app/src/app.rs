use std::{path::PathBuf, time::Duration};

use chrono::Utc;
use notm_mail::{ComposedMessage, ExternalCommandTransport, FakeSendTransport, SendTransport};
use notm_notmuch::{Database, DatabaseMode, OpenConfig, QueryOptions, SortOrder};
use notm_ui::{LaunchOptions, SavedSearch};
use uuid::Uuid;

use crate::{
    cli::{Cli, Command},
    config,
};

pub fn run(cli: Cli) -> anyhow::Result<()> {
    let app_config_path = cli.config.clone().unwrap_or_else(crate::paths::config_path);
    match cli.command {
        Command::Launch {
            automation,
            automation_socket,
            automation_token,
            fixture,
            message_id,
        } => {
            let cfg = config::load(cli.config)?;
            let fixture_guard = if fixture {
                Some(notm_test_support::FixtureDatabase::create()?)
            } else {
                None
            };
            let mut options = launch_options(&cfg, Some(app_config_path.clone()));
            if let Some(fixture) = &fixture_guard {
                options.database_path = Some(fixture.root.clone());
                options.config_path = Some(fixture.config_path.clone());
                options.default_query = "tag:inbox".to_string();
                options.send_command = None;
                options.fake_send_capture_dir = Some(fixture.root.join("captured-send"));
                options.draft_path = Some(fixture.root.join(".notm-fixture-draft.json"));
                options.drafts_dir = Some(fixture.root.join(".notm-fixture-drafts"));
                options.app_config_path = Some(fixture.root.join(".notm-fixture-config.toml"));
            }
            if let Some(message_id) = message_id {
                apply_message_id_target(&mut options, &message_id)?;
            }
            if automation {
                options.automation_enabled = true;
                options.automation_socket = automation_socket;
                options.automation_token = automation_token;
            }
            notm_ui::launch(options)
        }
        Command::PrintConfig => {
            let cfg = config::load(cli.config)?;
            println!("{}", serde_json::to_string_pretty(&cfg)?);
            Ok(())
        }
        Command::ProbeSend => {
            let cfg = config::load(cli.config)?;
            let transport = external_transport(&cfg)?;
            let rt = tokio::runtime::Runtime::new()?;
            let report = rt.block_on(transport.probe())?;
            println!("{}", serde_json::to_string_pretty(&report)?);
            anyhow::ensure!(report.ok, "send transport probe failed");
            Ok(())
        }
        Command::FixtureSmoke => fixture_smoke(),
        Command::LiveReadonlySmoke => {
            let cfg = config::load(cli.config)?;
            live_readonly_smoke(&cfg)
        }
        Command::LiveSelfSend => {
            let cfg = config::load(cli.config)?;
            live_self_send(&cfg)
        }
    }
}

fn launch_options(cfg: &config::AppConfig, app_config_path: Option<PathBuf>) -> LaunchOptions {
    LaunchOptions {
        database_path: cfg.notmuch.database_path.clone(),
        config_path: cfg.notmuch.config_path.clone(),
        profile: cfg.notmuch.profile.clone(),
        default_query: cfg.notmuch.default_query.clone(),
        excluded_tags: cfg.notmuch.excluded_tags.clone(),
        page_size: cfg.ui.page_size,
        identity_name: cfg.identity.name.clone(),
        primary_email: cfg.identity.primary_email.clone(),
        other_email: cfg.identity.other_email.clone(),
        send_enabled: cfg.send.enabled,
        send_command: cfg.send.command.clone(),
        send_args: cfg.send.args.clone(),
        send_mode: config::transport_mode(&cfg.send.mode),
        send_working_dir: cfg.send.working_dir.clone(),
        send_env: cfg.send.env.clone(),
        send_timeout_seconds: cfg.send.timeout_seconds,
        fake_send_capture_dir: None,
        save_sent: cfg.send.save_sent,
        sent_maildir: cfg.send.sent_maildir.clone(),
        sent_tags: cfg.send.sent_tags.clone(),
        index_sent_after_send: cfg.send.index_sent_after_send,
        save_drafts_to_maildir: cfg.drafts.save_maildir,
        draft_maildir: cfg.drafts.maildir.clone(),
        draft_tags: cfg.drafts.tags.clone(),
        index_draft_after_save: cfg.drafts.index_after_save,
        sync_enabled: cfg.sync.enabled,
        manual_sync_label: cfg.sync.manual_action_label.clone(),
        notmuch_database_update_enabled: cfg.sync.notmuch_database_update_enabled,
        notmuch_database_update_on_startup: cfg.sync.notmuch_database_update_on_startup,
        notmuch_database_update_command: cfg.sync.notmuch_database_update_command.clone(),
        external_receive_enabled: cfg.sync.external_receive_enabled,
        external_receive_on_startup: cfg.sync.external_receive_on_startup,
        external_receive_command: cfg.sync.external_receive_command.clone(),
        screenshot_dir: cfg.automation.screenshot_dir.clone(),
        automation_enabled: cfg.automation.enabled,
        automation_socket: cfg.automation.socket_path.clone(),
        automation_token: cfg.automation.token.clone(),
        show_debug_panel: cfg.ui.show_debug_panel,
        start_maximized: cfg.ui.start_maximized,
        show_sidebar: cfg.ui.show_sidebar,
        show_message_list: cfg.ui.show_message_list,
        show_message_view: cfg.ui.show_message_view,
        remote_images: cfg.ui.remote_images,
        show_thread_numbers: cfg.ui.show_thread_numbers,
        show_thread_dates: cfg.ui.show_thread_dates,
        show_thread_tags: cfg.ui.show_thread_tags,
        show_thread_preview: cfg.ui.show_thread_preview,
        show_keybind_hints: cfg.ui.show_keybind_hints,
        layout: cfg.ui.layout.clone(),
        html_mode: cfg.ui.html_mode.clone(),
        trusted_image_senders: cfg.ui.trusted_image_senders.clone(),
        hidden_tag_searches: cfg.ui.hidden_tag_searches.clone(),
        sync_maildir_flags_after_tag_change: cfg.notmuch.sync_maildir_flags_after_tag_change,
        draft_path: None,
        drafts_dir: None,
        app_config_path,
        custom_saved_searches: cfg
            .ui
            .custom_saved_searches
            .iter()
            .map(|saved| SavedSearch {
                name: saved.name.clone(),
                query: saved.query.clone(),
            })
            .collect(),
        open_message_id: None,
        runtime_settings: Default::default(),
    }
}

fn normalize_message_id(raw: &str) -> anyhow::Result<String> {
    let trimmed = raw.trim();
    let without_angles = trimmed
        .strip_prefix('<')
        .and_then(|value| value.strip_suffix('>'))
        .unwrap_or(trimmed)
        .trim();
    anyhow::ensure!(
        !without_angles.is_empty(),
        "--message-id requires a non-empty message id"
    );
    Ok(without_angles.to_string())
}

fn apply_message_id_target(options: &mut LaunchOptions, raw: &str) -> anyhow::Result<()> {
    options.open_message_id = Some(normalize_message_id(raw)?);
    Ok(())
}

fn open_config(cfg: &config::AppConfig) -> OpenConfig {
    OpenConfig {
        database_path: cfg.notmuch.database_path.clone(),
        config_path: cfg.notmuch.config_path.clone(),
        profile: cfg.notmuch.profile.clone(),
    }
}

fn external_transport(cfg: &config::AppConfig) -> anyhow::Result<ExternalCommandTransport> {
    if !cfg.send.enabled {
        anyhow::bail!("send.enabled is false");
    }
    let Some(command) = &cfg.send.command else {
        anyhow::bail!("send.command is not configured and no helper was detected");
    };
    Ok(ExternalCommandTransport {
        command: command.clone(),
        args: cfg.send.args.clone(),
        mode: config::transport_mode(&cfg.send.mode),
        working_dir: cfg.send.working_dir.clone(),
        env: cfg.send.env.clone(),
        timeout: Duration::from_secs(cfg.send.timeout_seconds),
    })
}

fn fixture_smoke() -> anyhow::Result<()> {
    let fixture = notm_test_support::FixtureDatabase::create()?;
    let db = fixture.open_readonly()?;
    let options = QueryOptions {
        limit: 20,
        offset: 0,
        sort: SortOrder::NewestFirst,
        excluded_tags: vec!["trash".to_string(), "spam".to_string()],
    };
    let threads = db.search_threads("tag:inbox", &options)?;
    anyhow::ensure!(
        !threads.is_empty(),
        "fixture inbox search returned no threads"
    );
    let messages = db.search_messages("subject:HTML", &options)?;
    anyhow::ensure!(!messages.is_empty(), "fixture HTML message not indexed");
    let capture_dir = PathBuf::from("artifacts/captured-send");
    let fake = FakeSendTransport { capture_dir };
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(fake.probe())?;
    let report = rt.block_on(fake.send(ComposedMessage::new(
        "Fixture User <fixture@example.test>".to_string(),
        vec!["fixture@example.test".to_string()],
        "notm fake send contract".to_string(),
        "fake body".to_string(),
    )))?;
    anyhow::ensure!(report.accepted, "fake transport did not accept message");
    println!(
        "fixture_smoke ok: {} inbox threads, fake capture {:?}",
        threads.len(),
        report.captured_path
    );
    Ok(())
}

fn live_readonly_smoke(cfg: &config::AppConfig) -> anyhow::Result<()> {
    let db = Database::open(&open_config(cfg), DatabaseMode::ReadOnly)?;
    let options = QueryOptions {
        limit: cfg.ui.page_size.min(25),
        offset: 0,
        sort: SortOrder::NewestFirst,
        excluded_tags: cfg.notmuch.excluded_tags.clone(),
    };
    let threads = db.search_threads(&cfg.notmuch.default_query, &options)?;
    let revision = db.revision();
    println!(
        "live_readonly_smoke ok: path={} revision={} uuid={} query={} threads={}",
        db.path(),
        revision.revision,
        revision.uuid,
        cfg.notmuch.default_query,
        threads.len()
    );
    Ok(())
}

fn live_self_send(cfg: &config::AppConfig) -> anyhow::Result<()> {
    if !cfg.automation.allow_live_send_test {
        anyhow::bail!("live self-send disabled by automation.allow_live_send_test=false");
    }
    let Some(email) = &cfg.identity.primary_email else {
        anyhow::bail!("primary email is not configured/detectable; skipping live self-send");
    };
    let transport = external_transport(cfg)?;
    let rt = tokio::runtime::Runtime::new()?;
    let probe = rt.block_on(transport.probe())?;
    if !probe.ok {
        anyhow::bail!("send transport probe failed: {:?}", probe.details);
    }
    let stamp = Utc::now().format("%Y%m%dT%H%M%SZ");
    let short_uuid = Uuid::new_v4().to_string()[..8].to_string();
    let subject = format!("notm live self-test {stamp} {short_uuid}");
    let from = match &cfg.identity.name {
        Some(name) => format!("{} <{}>", name, email),
        None => email.clone(),
    };
    let message = ComposedMessage::new(
        from,
        vec![email.clone()],
        subject.clone(),
        format!(
            "notm single automated self-test\nTimestamp: {}\nApp version: {}\nThis command sends exactly one message and does not force sync.\n",
            Utc::now().to_rfc3339(),
            env!("CARGO_PKG_VERSION")
        ),
    );
    let report = rt.block_on(transport.send(message))?;
    println!("send_report: {}", serde_json::to_string_pretty(&report)?);
    if !report.accepted {
        anyhow::bail!("send transport exited unsuccessfully; not retrying");
    }
    let db_config = open_config(cfg);
    let options = QueryOptions {
        limit: 10,
        offset: 0,
        sort: SortOrder::NewestFirst,
        excluded_tags: Vec::new(),
    };
    let query = format!("subject:\"{}\"", subject.replace('"', ""));
    for _ in 0..30 {
        let db = Database::open(&db_config, DatabaseMode::ReadOnly)?;
        let messages = db.search_messages(&query, &options)?;
        if !messages.is_empty() {
            println!("self-send appeared in Notmuch without forced sync: subject={subject}");
            return Ok(());
        }
        std::thread::sleep(Duration::from_secs(2));
    }
    println!(
        "send transport accepted the self-test, but it did not appear in Notmuch without forced sync: subject={subject}"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use notm_ui::LaunchOptions;

    #[test]
    fn normalize_message_id_accepts_rfc_angle_brackets() {
        assert_eq!(
            super::normalize_message_id(" <abc@example.test> ").unwrap(),
            "abc@example.test"
        );
    }

    #[test]
    fn message_id_target_preserves_startup_query() {
        let mut options = LaunchOptions {
            default_query: "tag:inbox".to_string(),
            ..LaunchOptions::default()
        };

        super::apply_message_id_target(&mut options, "<abc@example.test>").unwrap();

        assert_eq!(options.default_query, "tag:inbox");
        assert_eq!(options.open_message_id.as_deref(), Some("abc@example.test"));
    }

    #[test]
    fn launch_options_passes_layout_preference() {
        let mut cfg = crate::config::AppConfig::default();
        cfg.ui.layout = "stacked".to_string();

        let options = super::launch_options(&cfg, None);

        assert_eq!(options.layout, "stacked");
    }
}
