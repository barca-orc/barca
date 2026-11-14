//! The error envelope of a metadata database barca cannot use (#513). The database side (it is
//! `MetadataUnusable` naming the file, and left as it was) is barca-db's
//! `db::schema::ddl` tests; these moved up with `envelope_of` in #905 step 4.

use super::*;
use crate::db;
use crate::db::schema::ddl::VERSION;

async fn raw(path: &str) -> (libsql::Database, libsql::Connection) {
    let db = libsql::Builder::new_local(path).build().await.unwrap();
    let conn = db.connect().unwrap();
    (db, conn)
}

/// A database this release cannot use has the `metadata_unusable` reason, exit code 3 and a
/// remediation naming `barca state restore` in its envelope.
#[tokio::test]
async fn a_database_barca_cannot_use_has_the_metadata_unusable_envelope() {
    let dir = tempfile::tempdir().unwrap();
    let junk = dir.path().join("junk.db").display().to_string();
    std::fs::write(&junk, "not sqlite ".repeat(100)).unwrap();
    let future = dir.path().join("future.db").display().to_string();
    db::init_db(&future).await.unwrap();
    {
        let (_db, conn) = raw(&future).await;
        conn.execute("PRAGMA user_version=7", ()).await.unwrap();
        db::checkpoint(&conn).await.unwrap();
    }
    let malformed = dir.path().join("malformed.db").display().to_string();
    {
        let (_db, conn) = raw(&malformed).await;
        conn.execute("CREATE TABLE runs (run_id TEXT)", ())
            .await
            .unwrap();
        conn.execute("CREATE TABLE materializations (node_id TEXT)", ())
            .await
            .unwrap();
        conn.execute(&format!("PRAGMA user_version={VERSION}"), ())
            .await
            .unwrap();
        db::checkpoint(&conn).await.unwrap();
    }
    for path in [&junk, &future, &malformed] {
        let error = db::init_db(path).await.unwrap_err();
        let envelope = envelope_of(error, &Default::default()).to_json();
        assert_eq!(envelope["reason"], "metadata_unusable", "{envelope}");
        assert_eq!(envelope["code"], 3);
        assert!(
            envelope["remediation"]
                .as_str()
                .unwrap()
                .contains("barca state restore")
        );
    }
}

#[test]
fn an_error_envelope_without_a_reason_has_no_reason_key() {
    let envelope = envelope_of(BarcaError::Db("locked".into()), &Default::default()).to_json();
    assert!(envelope.get("reason").is_none(), "{envelope}");
}
