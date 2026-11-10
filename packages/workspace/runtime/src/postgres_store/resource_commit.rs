use postgres::GenericClient;

pub(super) fn require_durable_resource_commit(
    transaction: &mut impl GenericClient,
) -> Result<(), String> {
    let fsync: String = transaction
        .query_one("SHOW fsync", &[])
        .map_err(|error| format!("read resource commit durability failed: {error}"))?
        .get(0);
    require_fsync(&fsync)?;
    // SET LOCAL changes only this critical transaction, including when the
    // deployment role or connection intentionally uses asynchronous commits.
    transaction
        .execute("SET LOCAL synchronous_commit=on", &[])
        .map(|_| ())
        .map_err(|error| format!("require durable resource commit failed: {error}"))
}

fn require_fsync(setting: &str) -> Result<(), String> {
    if setting != "on" {
        return Err("critical resource commit requires PostgreSQL fsync=on".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use postgres::{Client, NoTls};

    #[test]
    fn critical_resource_commit_rejects_disabled_or_unknown_fsync() {
        assert!(require_fsync("on").is_ok());
        for setting in ["off", "", "unknown"] {
            assert!(require_fsync(setting).is_err());
        }
    }

    #[test]
    #[ignore = "requires the owned disposable PostgreSQL fixture"]
    fn critical_resource_transaction_overrides_asynchronous_session_without_changing_default() {
        assert_eq!(
            std::env::var("CENTAERIS_ALLOW_POSTGRES_TEST_RESET").as_deref(),
            Ok("1")
        );
        let url = std::env::var("CENTAERIS_TEST_POSTGRES_URL").unwrap();
        let mut connection = Client::connect(&url, NoTls).unwrap();
        connection
            .batch_execute("SET synchronous_commit=off")
            .unwrap();
        let mut transaction = connection.transaction().unwrap();
        require_durable_resource_commit(&mut transaction).unwrap();
        assert_eq!(
            transaction
                .query_one("SHOW synchronous_commit", &[])
                .unwrap()
                .get::<_, String>(0),
            "on",
            "a resource liability acknowledgement must not inherit asynchronous session commits"
        );
        transaction.commit().unwrap();
        assert_eq!(
            connection
                .query_one("SHOW synchronous_commit", &[])
                .unwrap()
                .get::<_, String>(0),
            "off",
            "ordinary adapter policy remains unchanged outside the critical transaction"
        );
    }
}
