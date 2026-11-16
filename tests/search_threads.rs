use notm_notmuch::{QueryOptions, SortOrder};

#[test]
fn refresh_positions_match_sorted_query_and_support_cancellation() -> anyhow::Result<()> {
    let fixture = notm_test_support::FixtureDatabase::create()?;
    let db = fixture.open_readonly()?;
    let options = QueryOptions::default();
    let threads = db.search_threads("tag:inbox", &options)?;
    assert!(threads.len() >= 3);
    let ids = [
        threads[1].thread_id.clone(),
        threads[2].thread_id.clone(),
        "absent-thread".to_string(),
    ]
    .into_iter()
    .collect();
    let positions = db.thread_positions(
        "tag:inbox",
        &QueryOptions {
            offset: 100,
            limit: 1,
            ..options.clone()
        },
        &ids,
        || false,
    )?;
    assert_eq!(positions.len(), 2);
    assert_eq!(positions[&threads[1].thread_id], 1);
    assert_eq!(positions[&threads[2].thread_id], 2);
    assert!(matches!(
        db.thread_positions("tag:inbox", &options, &ids, || true),
        Err(notm_notmuch::Error::Io(error)) if error.kind() == std::io::ErrorKind::Interrupted
    ));
    Ok(())
}

#[test]
fn searches_fixture_threads() -> anyhow::Result<()> {
    let fixture = notm_test_support::FixtureDatabase::create()?;
    let db = fixture.open_readonly()?;
    let options = QueryOptions {
        limit: 50,
        offset: 0,
        sort: SortOrder::NewestFirst,
        excluded_tags: vec!["trash".into(), "spam".into()],
    };
    let threads = db.search_threads("tag:inbox", &options)?;
    assert!(threads.iter().any(|t| t.subject.contains("Unread inbox")));
    assert!(
        threads
            .iter()
            .all(|t| !t.tags.contains(&"spam".to_string()))
    );
    Ok(())
}

#[test]
fn searches_fixture_thread_by_message_id() -> anyhow::Result<()> {
    let fixture = notm_test_support::FixtureDatabase::create()?;
    let db = fixture.open_readonly()?;
    let options = QueryOptions {
        limit: 50,
        offset: 0,
        sort: SortOrder::NewestFirst,
        excluded_tags: vec!["trash".into(), "spam".into()],
    };

    let threads = db.search_threads("id:html-message@fixture.test", &options)?;
    assert_eq!(threads.len(), 1);
    let messages = db.thread_messages_bounded(&threads[0].thread_id, 4_096)?;
    assert!(
        messages
            .iter()
            .any(|message| message.message_id == "html-message@fixture.test")
    );
    Ok(())
}
