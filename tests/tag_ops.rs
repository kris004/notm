use notm_notmuch::{MessageSummary, QueryOptions, SortOrder, TagMutation};
use std::{collections::BTreeMap, path::Path};

#[test]
fn applies_and_undoes_tag_operations_without_cli() -> anyhow::Result<()> {
    let fixture = notm_test_support::FixtureDatabase::create()?;
    let options = QueryOptions {
        limit: 10,
        offset: 0,
        sort: SortOrder::NewestFirst,
        excluded_tags: vec![],
    };
    let db = fixture.open_readwrite()?;
    let before = db.count_messages("subject:\"Unread inbox message\" and tag:inbox", &options)?;
    assert_eq!(before, 1);
    db.apply_tags_to_query(
        "subject:\"Unread inbox message\"",
        &TagMutation {
            add: vec![],
            remove: vec!["inbox".into()],
            sync_maildir_flags: true,
        },
    )?;
    let after = db.count_messages("subject:\"Unread inbox message\" and tag:inbox", &options)?;
    assert_eq!(after, 0);
    db.apply_tags_to_query(
        "subject:\"Unread inbox message\"",
        &TagMutation {
            add: vec!["inbox".into()],
            remove: vec![],
            sync_maildir_flags: true,
        },
    )?;
    let restored = db.count_messages("subject:\"Unread inbox message\" and tag:inbox", &options)?;
    assert_eq!(restored, 1);
    Ok(())
}

#[test]
fn per_message_tag_deltas_round_trip_mixed_thread_exactly() -> anyhow::Result<()> {
    let fixture = notm_test_support::FixtureDatabase::create()?;
    let db = fixture.open_readwrite()?;
    let options = QueryOptions {
        limit: usize::MAX,
        offset: 0,
        sort: SortOrder::OldestFirst,
        excluded_tags: Vec::new(),
    };
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

    assert_eq!(report.changed_messages, 2);
    assert_eq!(report.changes.len(), 2);
    assert!(
        report
            .changes
            .iter()
            .any(|change| { change.added == ["inbox"] && change.removed.is_empty() })
    );
    assert!(
        report
            .changes
            .iter()
            .any(|change| { change.added.is_empty() && change.removed == ["unread"] })
    );

    let inverses = report
        .changes
        .iter()
        .map(|change| change.inverse())
        .collect::<Vec<_>>();
    db.apply_tags_to_messages(&inverses, false)?;

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
    assert_eq!(noop.changed_messages, 0);
    assert!(noop.changes.is_empty());

    Ok(())
}

fn tags_by_message_id(messages: Vec<MessageSummary>) -> BTreeMap<String, Vec<String>> {
    messages
        .into_iter()
        .map(|message| (message.message_id, message.tags))
        .collect()
}

#[test]
fn applies_path_style_tags() -> anyhow::Result<()> {
    let fixture = notm_test_support::FixtureDatabase::create()?;
    let options = QueryOptions {
        limit: 10,
        offset: 0,
        sort: SortOrder::NewestFirst,
        excluded_tags: vec![],
    };
    let db = fixture.open_readwrite()?;
    db.apply_tags_to_query(
        "subject:\"Unread inbox message\"",
        &TagMutation {
            add: vec!["tests/notm".into()],
            remove: vec![],
            sync_maildir_flags: true,
        },
    )?;
    let count = db.count_messages(
        "subject:\"Unread inbox message\" and tag:\"tests/notm\"",
        &options,
    )?;
    assert_eq!(count, 1);
    Ok(())
}

#[test]
fn applies_tags_to_thread_range_without_expanding_ids() -> anyhow::Result<()> {
    let fixture = notm_test_support::FixtureDatabase::create()?;
    let options = QueryOptions {
        limit: 20,
        offset: 0,
        sort: SortOrder::NewestFirst,
        excluded_tags: vec![],
    };
    let db = fixture.open_readwrite()?;
    let threads = db.search_threads("tag:inbox", &options)?;
    assert!(threads.len() >= 4);
    let selected = &threads[1..=2];
    let expected_messages = selected
        .iter()
        .map(|thread| {
            db.thread_messages(&thread.thread_id)
                .map(|messages| messages.len())
        })
        .sum::<notm_notmuch::Result<usize>>()?;

    let report = db.apply_tags_to_thread_range(
        "tag:inbox",
        &QueryOptions {
            limit: usize::MAX,
            offset: 0,
            sort: SortOrder::NewestFirst,
            excluded_tags: vec![],
        },
        1,
        2,
        &TagMutation {
            add: vec!["notm/range-test".into()],
            remove: vec![],
            sync_maildir_flags: true,
        },
    )?;

    assert_eq!(report.changed_threads, selected.len());
    assert_eq!(report.changed_messages, expected_messages);
    assert_eq!(
        db.count_messages("tag:\"notm/range-test\"", &options)? as usize,
        expected_messages
    );
    assert_eq!(
        db.count_messages(
            &format!(
                "thread:{} and tag:\"notm/range-test\"",
                threads[0].thread_id
            ),
            &options,
        )?,
        0
    );
    Ok(())
}

#[test]
fn removes_indexed_message_file_from_database() -> anyhow::Result<()> {
    let fixture = notm_test_support::FixtureDatabase::create()?;
    let options = QueryOptions {
        limit: 10,
        offset: 0,
        sort: SortOrder::NewestFirst,
        excluded_tags: vec![],
    };
    let db = fixture.open_readwrite()?;
    let messages = db.search_messages("subject:\"Draft like message\"", &options)?;
    let filename = messages
        .first()
        .and_then(|message| message.filenames.first())
        .expect("fixture draft filename")
        .clone();

    db.remove_message_file(Path::new(&filename))?;
    let count = db.count_messages("subject:\"Draft like message\"", &options)?;

    assert_eq!(count, 0);
    assert!(Path::new(&filename).exists());
    Ok(())
}
