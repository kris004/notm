use notm_notmuch::{QueryOptions, SortOrder};

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
    let messages = db.thread_messages(&threads[0].thread_id)?;
    assert!(
        messages
            .iter()
            .any(|message| message.message_id == "html-message@fixture.test")
    );
    Ok(())
}
