use postgres::Client;

fn ownership_matches(
    database: &str,
    expected: &str,
    token: &str,
    marker: &str,
    owner: bool,
) -> bool {
    let hex_id = |value: &str| {
        value.len() == 32
            && value
                .bytes()
                .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
    };
    owner
        && database == expected
        && database.strip_prefix("test_outbox_").is_some_and(hex_id)
        && hex_id(token)
        && marker == format!("centaeris-test-owner:{token}")
}

pub(super) fn require_owned_database(db: &mut Client) {
    assert_eq!(
        std::env::var("CENTAERIS_ALLOW_POSTGRES_TEST_RESET").as_deref(),
        Ok("1")
    );
    let expected =
        std::env::var("CENTAERIS_TEST_POSTGRES_DATABASE").expect("gate-owned database required");
    let token = std::env::var("CENTAERIS_TEST_POSTGRES_OWNERSHIP_TOKEN")
        .expect("gate database ownership token required");
    let row = db.query_one("SELECT datname, COALESCE(shobj_description(oid, 'pg_database'), ''), pg_get_userbyid(datdba) = current_user FROM pg_database WHERE datname = current_database()", &[]).unwrap();
    assert!(ownership_matches(&row.get::<_, String>(0), &expected, &token, &row.get::<_, String>(1), row.get(2)),
        "destructive fixture requires the current gate-owned disposable database and matching ownership marker");
}

#[test]
fn disposable_database_requires_exact_name_marker_owner_and_unpredictable_identity() {
    let name = "test_outbox_0123456789abcdef0123456789abcdef";
    let token = "fedcba9876543210fedcba9876543210";
    let marker = format!("centaeris-test-owner:{token}");
    assert!(ownership_matches(name, name, token, &marker, true));
    assert!(!ownership_matches(
        "production",
        "production",
        token,
        &marker,
        true
    ));
    assert!(!ownership_matches(
        name,
        "test_outbox_ffffffffffffffffffffffffffffffff",
        token,
        &marker,
        true
    ));
    assert!(!ownership_matches(name, name, token, "", true));
    assert!(!ownership_matches(name, name, token, &marker, false));
    assert!(!ownership_matches(
        "test_retained_cleanup",
        "test_retained_cleanup",
        token,
        &marker,
        true
    ));
    assert!(!ownership_matches(
        name,
        name,
        "",
        "centaeris-test-owner:",
        true
    ));
}
