#[test]
fn upload_snapshot_cleanup_preserves_another_live_runs_directory() {
    let directory = tempfile::tempdir().unwrap();
    let a = super::UploadSnapshots {
        prefix: directory.path().join("upload-a-"),
    };
    let b = super::UploadSnapshots {
        prefix: directory.path().join("upload-b-"),
    };
    let owned = directory.path().join("upload-a-source.tmp");
    let helper_stage = directory.path().join(".upload-a-source.tmp.helper.tmp");
    std::fs::write(&owned, b"11").unwrap();
    std::fs::write(&helper_stage, b"11").unwrap();
    drop(a);
    assert!(!owned.exists() && !helper_stage.exists());
    let later = directory.path().join("upload-b-source.tmp");
    std::fs::write(&later, b"222").unwrap();
    drop(b);
    assert!(!later.exists());
}

use super::*;
use crate::cache::{CacheHit, resolve_cache_hit};
use crate::transfer::ArtifactLayout;

fn oref(path: &str) -> dispatch::OutputRef {
    dispatch::OutputRef {
        path: path.to_string(),
        format: "json".to_string(),
        size_bytes: 3,
        elapsed_seconds: None,
        content_hash: None,
    }
}

#[test]
fn cache_hit_without_store_is_used_as_recorded() {
    match resolve_cache_hit(oref("/w/.barca/artifacts/n/h.json"), None) {
        CacheHit::Local(o) => assert_eq!(o.path, "/w/.barca/artifacts/n/h.json"),
        _ => panic!("expected Local"),
    }
}

#[test]
fn cache_hit_in_store_points_at_local_mirror() {
    let layout = ArtifactLayout::new("/w/a", "s3://b/p/default/artifacts");
    match resolve_cache_hit(oref("s3://b/p/default/artifacts/n/h.json"), Some(&layout)) {
        CacheHit::Store { local, store } => {
            assert_eq!(local.path, "/w/a/n/h.json");
            assert_eq!(local.format, "json");
            assert_eq!(store, "s3://b/p/default/artifacts/n/h.json");
        }
        _ => panic!("expected Store"),
    }
}

#[test]
fn legacy_local_row_is_used_when_the_file_is_here() {
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("h.json");
    std::fs::write(&f, "[1]").unwrap();
    let layout = ArtifactLayout::new("/w/a", "s3://b/p");
    match resolve_cache_hit(oref(f.to_str().unwrap()), Some(&layout)) {
        CacheHit::Local(o) => assert_eq!(o.path, f.to_str().unwrap()),
        _ => panic!("expected Local"),
    }
}

#[test]
fn a_row_outside_the_store_is_a_hit_whatever_is_on_disk() {
    let layout = ArtifactLayout::new("/w/a", "s3://b/p");
    // e.g. recorded against a different store, or another machine's local path. It is still
    // the cached result: it is computed again only if something has to read it (#252).
    for path in ["s3://other/p/n/h.json", "/elsewhere/n/h.json"] {
        match resolve_cache_hit(oref(path), Some(&layout)) {
            CacheHit::Local(o) => assert_eq!(o.path, path),
            _ => panic!("expected Local"),
        }
    }
}

#[tokio::test]
async fn persist_run_records_failure_type_and_its_own_attempts() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("m.db").to_string_lossy().into_owned();
    db::init_db(&db_path).await.unwrap();
    let failures = vec![dispatch::StepFailure {
        node_id: "f:a".to_string(),
        error: dispatch::StepError {
            error_type: "UploadError".to_string(),
            message: "upload to s3://b/f__a/h1.json failed: ConnectionError: reset".to_string(),
            traceback: String::new(),
            phase: None,
            attempts: 4,
            started_unix_ns: None,
            elapsed_seconds: None,
        },
    }];
    let run_hashes = HashMap::from([("f:a".to_string(), "h1".to_string())]);
    // The step itself ran once; the upload made 4 attempts.
    let all_attempts = HashMap::from([("f:a".to_string(), 1u32)]);
    let ledger = RunLedger {
        run_id: "r1",
        status: "failed",
        command: "get",
        files: "f.py".to_string(),
        target: None,
        job: "f:a",
        steps_total: 1,
        steps_executed: 1,
        steps_cached: 0,
        elapsed: 0.1,
        all_outputs: &HashMap::new(),
        all_failures: &failures,
        all_sinks: &HashMap::new(),
        sink_publications: &[],
        all_attempts: &all_attempts,
        all_timings: &HashMap::new(),
        batches: &HashMap::new(),
        cached_node_ids: &std::collections::HashSet::new(),
        run_hashes: &run_hashes,
        components: &HashMap::new(),
        output_hashes: &HashMap::new(),
        store_paths: &HashMap::new(),
        cost_snapshot: &[],
        touched: &[],
    };
    persist_run(&db_path, &ledger).await.unwrap();

    let (_db, conn) = db::open_conn(&db_path).await.unwrap();
    let mut rows = conn
            .query(
                "SELECT status, error_type, attempts, artifact_path IS NULL, run_hash FROM materializations",
                (),
            )
            .await
            .unwrap();
    let row = rows.next().await.unwrap().unwrap();
    assert_eq!(row.get::<String>(0).unwrap(), "failed");
    assert_eq!(row.get::<String>(1).unwrap(), "UploadError");
    assert_eq!(row.get::<i64>(2).unwrap(), 4);
    assert_eq!(row.get::<i64>(3).unwrap(), 1);
    assert_eq!(row.get::<String>(4).unwrap(), "h1");
}

#[tokio::test]
async fn persist_run_records_store_locations() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("m.db").to_string_lossy().into_owned();
    db::init_db(&db_path).await.unwrap();

    let all_outputs = HashMap::from([
        ("f:a".to_string(), oref("/w/a/f__a/h1.json")),
        ("f:b".to_string(), oref("/w/a/f__b/h2.json")),
    ]);
    let run_hashes = HashMap::from([
        ("f:a".to_string(), "h1".to_string()),
        ("f:b".to_string(), "h2".to_string()),
    ]);
    // Only a was uploaded through a store; b keeps its recorded path.
    let store_paths = HashMap::from([("f:a".to_string(), "s3://b/p/f__a/h1.json".to_string())]);
    let ledger = RunLedger {
        run_id: "r1",
        status: "success",
        command: "get",
        files: "f.py".to_string(),
        target: None,
        job: "f:a",
        steps_total: 2,
        steps_executed: 2,
        steps_cached: 0,
        elapsed: 0.1,
        all_outputs: &all_outputs,
        all_failures: &[],
        all_sinks: &HashMap::new(),
        sink_publications: &[],
        all_attempts: &HashMap::new(),
        all_timings: &HashMap::new(),
        batches: &HashMap::new(),
        cached_node_ids: &std::collections::HashSet::new(),
        run_hashes: &run_hashes,
        components: &HashMap::new(),
        output_hashes: &HashMap::new(),
        store_paths: &store_paths,
        cost_snapshot: &[],
        touched: &[],
    };
    persist_run(&db_path, &ledger).await.unwrap();

    let (_db, conn) = db::open_conn(&db_path).await.unwrap();
    let mut rows = conn
        .query(
            "SELECT node_id, artifact_path FROM materializations ORDER BY node_id",
            (),
        )
        .await
        .unwrap();
    let mut got = Vec::new();
    while let Some(row) = rows.next().await.unwrap() {
        got.push((row.get::<String>(0).unwrap(), row.get::<String>(1).unwrap()));
    }
    assert_eq!(
        got,
        vec![
            ("f:a".to_string(), "s3://b/p/f__a/h1.json".to_string()),
            ("f:b".to_string(), "/w/a/f__b/h2.json".to_string()),
        ]
    );
}
