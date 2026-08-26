use notm_notmuch::{
    Database, MessageSummary, MessageTagMutation, QueryOptions, SortOrder, TagBatchReport,
    TagFailureStage, TagMutation,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

#[test]
fn applies_and_undoes_tag_operations_without_cli() -> anyhow::Result<()> {
    let fixture = notm_test_support::FixtureDatabase::create()?;
    let options = query_options(SortOrder::NewestFirst);
    let db = fixture.open_readwrite()?;
    let before = db.count_messages("subject:\"Unread inbox message\" and tag:inbox", &options)?;
    assert_eq!(before, 1);
    let removed = db.apply_tags_to_query(
        "subject:\"Unread inbox message\"",
        &TagMutation {
            add: vec![],
            remove: vec!["inbox".into()],
            sync_maildir_flags: true,
        },
    )?;
    assert_complete(&removed.batch);
    let after = db.count_messages("subject:\"Unread inbox message\" and tag:inbox", &options)?;
    assert_eq!(after, 0);
    let restored = db.apply_tags_to_query(
        "subject:\"Unread inbox message\"",
        &TagMutation {
            add: vec!["inbox".into()],
            remove: vec![],
            sync_maildir_flags: true,
        },
    )?;
    assert_complete(&restored.batch);
    let restored = db.count_messages("subject:\"Unread inbox message\" and tag:inbox", &options)?;
    assert_eq!(restored, 1);
    db.close()?;
    Ok(())
}

#[test]
fn per_message_tag_deltas_round_trip_mixed_thread_exactly() -> anyhow::Result<()> {
    let fixture = notm_test_support::FixtureDatabase::create()?;
    let db = fixture.open_readwrite()?;
    let options = query_options(SortOrder::OldestFirst);
    let matching = db.search_messages("subject:\"Three message thread\"", &options)?;
    let thread_id = matching
        .first()
        .map(|message| message.thread_id.clone())
        .expect("fixture thread");
    let query = format!("thread:{thread_id}");
    let before = tags_by_message_id(db.search_messages(&query, &options)?);
    assert_eq!(before.len(), 3, "fixture should contain a 3-message thread");

    let report = db.apply_tags_to_query(
        &query,
        &TagMutation {
            add: vec!["inbox".to_string()],
            remove: vec!["unread".to_string()],
            sync_maildir_flags: false,
        },
    )?;
    assert_complete(&report.batch);
    assert_eq!(report.batch.changed_messages, 2);
    assert_eq!(report.batch.changes.len(), 2);
    assert!(
        report
            .batch
            .changes
            .iter()
            .any(|change| { change.added == ["inbox"] && change.removed.is_empty() })
    );
    assert!(
        report
            .batch
            .changes
            .iter()
            .any(|change| { change.added.is_empty() && change.removed == ["unread"] })
    );

    let inverses = report
        .batch
        .changes
        .iter()
        .map(|change| change.inverse())
        .collect::<Vec<_>>();
    let undo = db.apply_tags_to_messages(&inverses, false)?;
    assert_complete(&undo);

    let restored = tags_by_message_id(db.search_messages(&query, &options)?);
    assert_eq!(restored, before);

    let noop = db.apply_tags_to_query(
        &query,
        &TagMutation {
            add: Vec::new(),
            remove: vec!["not-present".to_string()],
            sync_maildir_flags: false,
        },
    )?;
    assert_complete(&noop.batch);
    assert_eq!(noop.batch.changed_messages, 0);
    assert!(noop.batch.changes.is_empty());

    db.close()?;
    Ok(())
}

#[test]
fn applies_path_style_tags() -> anyhow::Result<()> {
    let fixture = notm_test_support::FixtureDatabase::create()?;
    let options = query_options(SortOrder::NewestFirst);
    let db = fixture.open_readwrite()?;
    let report = db.apply_tags_to_query(
        "subject:\"Unread inbox message\"",
        &TagMutation {
            add: vec!["tests/notm".into()],
            remove: vec![],
            sync_maildir_flags: true,
        },
    )?;
    assert_complete(&report.batch);
    let count = db.count_messages(
        "subject:\"Unread inbox message\" and tag:\"tests/notm\"",
        &options,
    )?;
    assert_eq!(count, 1);
    db.close()?;
    Ok(())
}

#[test]
fn exact_thread_ids_remain_targets_after_search_reorders() -> anyhow::Result<()> {
    let fixture = notm_test_support::FixtureDatabase::create()?;
    let options = query_options(SortOrder::NewestFirst);
    let db = fixture.open_readwrite()?;
    let before = db.search_threads("tag:inbox", &options)?;
    assert!(before.len() >= 4);
    let selected_thread_ids = before[1..=2]
        .iter()
        .map(|thread| thread.thread_id.clone())
        .collect::<Vec<_>>();
    let expected_message_ids = selected_thread_ids
        .iter()
        .map(|thread_id| db.thread_messages_bounded(thread_id, 4_096))
        .collect::<notm_notmuch::Result<Vec<_>>>()?
        .into_iter()
        .flatten()
        .map(|message| message.message_id)
        .collect::<BTreeSet<_>>();

    let newest_path = fixture.maildir.join("cur/reorder.fixture:2,");
    fs::write(
        &newest_path,
        "From: reorder@example.test\r\nTo: fixture@example.test\r\nSubject: Reorder interloper\r\nDate: Thu, 18 Jun 2037 20:00:00 -0600\r\nMessage-ID: <reorder-interloper@fixture.test>\r\n\r\nnewest\r\n",
    )?;
    let interloper_id = db.index_file_with_tags(&newest_path, &["inbox", "unread"])?;
    let reordered = db.search_threads("tag:inbox", &options)?;
    assert_eq!(reordered[0].subject, "Reorder interloper");
    assert_ne!(
        reordered[1..=2]
            .iter()
            .map(|thread| thread.thread_id.clone())
            .collect::<Vec<_>>(),
        selected_thread_ids,
        "the positional range must actually have changed before exercising exact IDs"
    );

    let report = db.apply_tags_to_threads(
        &selected_thread_ids,
        &TagMutation {
            add: vec!["notm/exact-id-test".into()],
            remove: Vec::new(),
            sync_maildir_flags: false,
        },
    )?;
    assert!(report.is_complete(), "unexpected report: {report:#?}");
    assert_eq!(report.thread_ids, selected_thread_ids);
    assert_eq!(report.matched_threads, selected_thread_ids.len());
    assert_eq!(report.changed_threads, selected_thread_ids.len());
    assert_eq!(report.batch.changed_messages, expected_message_ids.len());

    let tagged_ids = db
        .search_messages("tag:\"notm/exact-id-test\"", &options)?
        .into_iter()
        .map(|message| message.message_id)
        .collect::<BTreeSet<_>>();
    assert_eq!(tagged_ids, expected_message_ids);
    assert!(!tagged_ids.contains(&interloper_id));
    db.close()?;
    Ok(())
}

#[test]
fn successful_multi_file_maildir_sync_reports_current_filenames() -> anyhow::Result<()> {
    let fixture = notm_test_support::FixtureDatabase::create()?;
    let options = query_options(SortOrder::NewestFirst);
    let db = fixture.open_readwrite()?;
    let original = only_message(&db, "subject:\"Unread inbox message\"", &options)?;
    let original_path = PathBuf::from(&original.filenames[0]);
    let duplicate_path = fixture.maildir.join("new/multi-file.fixture");
    fs::copy(&original_path, &duplicate_path)?;
    let duplicate_id = db.index_file_with_tags(&duplicate_path, &[])?;
    assert_eq!(duplicate_id, original.message_id);

    let report = db.apply_tags_to_messages(
        &[MessageTagMutation {
            message_id: original.message_id.clone(),
            add: Vec::new(),
            remove: vec!["unread".into()],
        }],
        true,
    )?;
    assert_complete(&report);
    assert_eq!(report.changed_messages, 1);
    let change = &report.changes[0];
    let expected_original = filename_with_flags(&original_path, "S");
    let expected_duplicate = fixture.maildir.join("cur/multi-file.fixture:2,S");
    assert_eq!(
        change
            .filenames
            .iter()
            .map(PathBuf::from)
            .collect::<BTreeSet<_>>(),
        [expected_original.clone(), expected_duplicate.clone()]
            .into_iter()
            .collect()
    );
    assert_eq!(
        change
            .filename_changes
            .iter()
            .map(|mapping| {
                (
                    PathBuf::from(&mapping.previous_filename),
                    PathBuf::from(&mapping.current_filename),
                )
            })
            .collect::<BTreeSet<_>>(),
        [
            (original_path.clone(), expected_original.clone()),
            (duplicate_path.clone(), expected_duplicate.clone()),
        ]
        .into_iter()
        .collect()
    );
    assert!(!change.tags.iter().any(|tag| tag == "unread"));
    assert!(expected_original.is_file());
    assert!(expected_duplicate.is_file());
    assert!(!original_path.exists());
    assert!(!duplicate_path.exists());
    db.close()?;

    let reopened = fixture.open_readonly()?;
    let persisted = only_message(&reopened, "subject:\"Unread inbox message\"", &options)?;
    assert_eq!(persisted.filenames, change.filenames);
    assert!(!persisted.tags.iter().any(|tag| tag == "unread"));
    reopened.close()?;
    Ok(())
}

#[test]
fn partial_maildir_rename_failure_is_reported_per_file() -> anyhow::Result<()> {
    let fixture = notm_test_support::FixtureDatabase::create()?;
    let options = query_options(SortOrder::NewestFirst);
    let db = fixture.open_readwrite()?;
    let original = only_message(&db, "subject:\"Read inbox message\"", &options)?;
    let original_path = PathBuf::from(&original.filenames[0]);
    let duplicate_path = fixture.maildir.join("cur/blocked-copy.fixture:2,S");
    fs::copy(&original_path, &duplicate_path)?;
    let duplicate_id = db.index_file_with_tags(&duplicate_path, &[])?;
    assert_eq!(duplicate_id, original.message_id);
    let blocked_target = fixture.maildir.join("cur/blocked-copy.fixture:2,");
    fs::create_dir(&blocked_target)?;

    let report = db.apply_tags_to_messages(
        &[MessageTagMutation {
            message_id: original.message_id.clone(),
            add: vec!["unread".into()],
            remove: Vec::new(),
        }],
        true,
    )?;
    assert!(
        !report.is_complete(),
        "partial rename must not look successful"
    );
    assert_eq!(report.changed_messages, 1);
    assert_eq!(report.failures.len(), 1);
    let failure = &report.failures[0];
    assert_eq!(failure.message_id, original.message_id);
    assert_eq!(failure.stage, TagFailureStage::MaildirFlagSync);
    assert_eq!(failure.file_failures.len(), 1);
    assert_eq!(
        failure.file_failures[0].previous_filename,
        duplicate_path.to_string_lossy()
    );
    assert_eq!(
        failure.file_failures[0].expected_filename,
        blocked_target.to_string_lossy()
    );
    assert_eq!(
        failure.file_failures[0].current_filename.as_deref(),
        Some(duplicate_path.to_string_lossy().as_ref())
    );
    let successful_target = filename_with_flags(&original_path, "");
    let current = report.changes[0]
        .filenames
        .iter()
        .map(PathBuf::from)
        .collect::<BTreeSet<_>>();
    assert_eq!(
        current,
        [successful_target.clone(), duplicate_path.clone()]
            .into_iter()
            .collect()
    );
    assert_eq!(
        report.changes[0]
            .filename_changes
            .iter()
            .map(|mapping| {
                (
                    PathBuf::from(&mapping.previous_filename),
                    PathBuf::from(&mapping.current_filename),
                )
            })
            .collect::<BTreeSet<_>>(),
        [
            (original_path.clone(), successful_target.clone()),
            (duplicate_path.clone(), duplicate_path.clone()),
        ]
        .into_iter()
        .collect()
    );
    assert!(successful_target.is_file());
    assert!(duplicate_path.is_file());
    assert!(blocked_target.is_dir());

    fs::remove_dir(&blocked_target)?;
    let retry = db.apply_tags_to_messages(
        &[MessageTagMutation {
            message_id: original.message_id.clone(),
            add: vec!["unread".into()],
            remove: Vec::new(),
        }],
        true,
    )?;
    assert_complete(&retry);
    assert_eq!(retry.changed_messages, 1);
    assert!(retry.changes[0].added.is_empty());
    assert!(retry.changes[0].removed.is_empty());
    assert_eq!(
        retry.changes[0]
            .filenames
            .iter()
            .map(PathBuf::from)
            .collect::<BTreeSet<_>>(),
        [successful_target.clone(), blocked_target.clone()]
            .into_iter()
            .collect()
    );
    assert!(blocked_target.is_file());
    assert!(!duplicate_path.exists());
    db.close()?;
    Ok(())
}

#[test]
fn missing_message_produces_partial_batch_without_losing_known_change() -> anyhow::Result<()> {
    let fixture = notm_test_support::FixtureDatabase::create()?;
    let options = query_options(SortOrder::NewestFirst);
    let db = fixture.open_readwrite()?;
    let existing = only_message(&db, "subject:\"Unread inbox message\"", &options)?;
    let missing_id = "missing-message@fixture.test".to_string();
    let report = db.apply_tags_to_messages(
        &[
            MessageTagMutation {
                message_id: existing.message_id.clone(),
                add: vec!["notm/partial-batch".into()],
                remove: Vec::new(),
            },
            MessageTagMutation {
                message_id: missing_id.clone(),
                add: vec!["notm/partial-batch".into()],
                remove: Vec::new(),
            },
        ],
        false,
    )?;
    assert!(!report.is_complete());
    assert_eq!(report.requested_messages, 2);
    assert_eq!(report.changed_messages, 1);
    assert_eq!(report.changes[0].message_id, existing.message_id);
    assert_eq!(report.failures.len(), 1);
    assert_eq!(report.failures[0].message_id, missing_id);
    assert_eq!(report.failures[0].stage, TagFailureStage::Lookup);
    db.close()?;

    let reopened = fixture.open_readonly()?;
    assert_eq!(
        reopened.count_messages("tag:\"notm/partial-batch\"", &options)?,
        1
    );
    reopened.close()?;
    Ok(())
}

#[test]
fn removes_indexed_message_file_from_database() -> anyhow::Result<()> {
    let fixture = notm_test_support::FixtureDatabase::create()?;
    let options = query_options(SortOrder::NewestFirst);
    let db = fixture.open_readwrite()?;
    let messages = db.search_messages("subject:\"Draft like message\"", &options)?;
    let filename = db
        .open_message_file(messages.first().expect("fixture draft message"))?
        .path()
        .to_path_buf();

    db.remove_message_file(&filename)?;
    let count = db.count_messages("subject:\"Draft like message\"", &options)?;

    assert_eq!(count, 0);
    assert!(filename.exists());
    db.close()?;
    Ok(())
}

fn query_options(sort: SortOrder) -> QueryOptions {
    QueryOptions {
        limit: usize::MAX,
        offset: 0,
        sort,
        excluded_tags: Vec::new(),
    }
}

fn only_message(
    db: &Database,
    query: &str,
    options: &QueryOptions,
) -> anyhow::Result<MessageSummary> {
    let messages = db.search_messages(query, options)?;
    anyhow::ensure!(messages.len() == 1, "expected one message for `{query}`");
    Ok(messages.into_iter().next().expect("length checked"))
}

fn filename_with_flags(path: &Path, flags: &str) -> PathBuf {
    let path = path.to_string_lossy();
    let prefix = path
        .split_once(":2,")
        .map_or(path.as_ref(), |(prefix, _)| prefix);
    PathBuf::from(format!("{prefix}:2,{flags}"))
}

fn tags_by_message_id(messages: Vec<MessageSummary>) -> BTreeMap<String, Vec<String>> {
    messages
        .into_iter()
        .map(|message| (message.message_id, message.tags))
        .collect()
}

fn assert_complete(report: &TagBatchReport) {
    assert!(
        report.is_complete(),
        "unexpected partial report: {report:#?}"
    );
}
