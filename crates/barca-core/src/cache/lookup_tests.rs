//! The cache reader's lookups through `cache` (moved up from barca-db's `db::cache_reader`
//! with #905 step 4: they call `cache::lookup_cached`).

use crate::cache::{CacheReader, lookup_cached};
use crate::db::{db_guard, init_db, open_conn, prepares};
use barca_model::step_result::OutputRef;

/// A reader compiles its lookup statements once, however many keys it looks up: 0.34.0
/// compiled the one-key lookup again for every key, a third of `barca status` over a
/// million keys.
#[tokio::test(flavor = "current_thread")]
async fn a_reader_compiles_each_lookup_once() {
    let dir = tempfile::tempdir().unwrap();
    let path = db_in(&dir);
    init_db(&path).await.unwrap();
    let cache = CacheReader::open(&path).await.unwrap();
    let before = prepares();
    for k in 0..100 {
        lookup_cached(&cache, &format!("p.py:a[k={k}]"), "h").await;
    }
    assert_eq!(prepares() - before, 1, "one-key lookups");
    let keys: Vec<(String, String)> = (0..5000)
        .map(|k| (format!("p.py:a[k={k}]"), "h".to_string()))
        .collect();
    let pairs: Vec<(&str, &str)> = keys.iter().map(|(n, h)| (n.as_str(), h.as_str())).collect();
    let found = cache.lookup_many(&pairs).await.unwrap();
    assert_eq!(found.len(), 5000);
    assert_eq!(
        prepares() - before,
        2,
        "and the lookup of many keys, in two chunks"
    );
}

fn db_in(dir: &tempfile::TempDir) -> String {
    dir.path().join("metadata.db").display().to_string()
}

#[tokio::test]
async fn lookups_together_answer_what_lookups_one_by_one_do() {
    let dir = tempfile::tempdir().unwrap();
    let path = db_in(&dir);
    init_db(&path).await.unwrap();
    for (node, hash, path_, format, status) in [
        // The newest successful row decides, even without a format: no hit.
        (
            "p.py:a[k=1]",
            "h1",
            Some("/x/a1.json"),
            Some("json"),
            "success",
        ),
        ("p.py:a[k=1]", "h1", Some("/x/a1b.json"), None, "success"),
        // An older row with an artifact under a newer one without.
        (
            "p.py:a[k=2]",
            "h2",
            Some("/x/a2.json"),
            Some("json"),
            "success",
        ),
        ("p.py:a[k=2]", "h2", None, None, "success"),
        // Failed only, and another run hash.
        (
            "p.py:a[k=3]",
            "h3",
            Some("/x/a3.json"),
            Some("json"),
            "failed",
        ),
        (
            "p.py:a[k=4]",
            "other",
            Some("/x/a4.json"),
            Some("json"),
            "success",
        ),
        (
            "p.py:a[k=5]",
            "h5",
            Some("/x/a5.pkl"),
            Some("pickle"),
            "success",
        ),
    ] {
        let _g = db_guard().await;
        let (_db, conn) = open_conn(&path).await.unwrap();
        conn.execute(
            "INSERT INTO materializations (node_id, run_hash, artifact_path, \
             artifact_format, artifact_size_bytes, status, output_hash) \
             VALUES (?1, ?2, ?3, ?4, 7, ?5, 'c')",
            libsql::params![node, hash, path_, format, status],
        )
        .await
        .unwrap();
    }
    let keys: Vec<(String, String)> = (1..=6)
        .map(|k| (format!("p.py:a[k={k}]"), format!("h{k}")))
        .collect();
    let pairs: Vec<(&str, &str)> = keys.iter().map(|(n, h)| (n.as_str(), h.as_str())).collect();
    let cache = CacheReader::open(&path).await.unwrap();
    let together = cache.lookup_many(&pairs).await.expect("answered together");
    let mut one_by_one = Vec::new();
    for (node, hash) in &pairs {
        one_by_one.push(lookup_cached(&cache, node, hash).await);
    }
    let paths = |found: &[Option<OutputRef>]| -> Vec<Option<String>> {
        found
            .iter()
            .map(|o| o.as_ref().map(|o| o.path.clone()))
            .collect()
    };
    assert_eq!(paths(&together), paths(&one_by_one));
    assert_eq!(
        paths(&together),
        [
            None,
            Some("/x/a2.json".into()),
            None,
            None,
            Some("/x/a5.pkl".into()),
            None
        ]
    );
}
