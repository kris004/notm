use notm_notmuch::{QueryOptions, SortOrder, TagMutation};

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
