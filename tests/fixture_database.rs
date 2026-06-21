#[test]
fn creates_fixture_database_with_native_libnotmuch() -> anyhow::Result<()> {
    let fixture = notm_test_support::FixtureDatabase::create()?;
    assert!(notm_test_support::fixture_maildir::fixture_root_exists(
        &fixture.root
    ));
    let db = fixture.open_readonly()?;
    assert!(!db.path().is_empty());
    let rev = db.revision();
    assert!(!rev.uuid.is_empty());
    Ok(())
}
