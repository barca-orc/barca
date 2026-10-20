//! The size limit on a real `.barca/` in a temporary directory: measuring, planning and
//! deleting together (the order itself is tested on [`plan::plan`] alone).

use super::*;

/// A project root with an initialized metadata DB.
struct Fixture {
    _dir: tempfile::TempDir,
    project: Project,
    db: String,
}

impl Fixture {
    async fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap().join("proj");
        std::fs::create_dir_all(root.join(".barca/artifacts")).unwrap();
        let db = root.join(".barca/metadata.db").display().to_string();
        crate::db::init_db(&db).await.unwrap();
        Self {
            project: Project::at(root),
            _dir: dir,
            db,
        }
    }

    fn root(&self) -> &Path {
        &self.project.root
    }

    /// An artifact file of `bytes` bytes and its successful row, written at `created`.
    async fn result(&self, node: &str, hash: &str, bytes: usize, created: &str) -> String {
        let rel = format!(".barca/artifacts/{}/{hash}.json", node.replace(':', "--"));
        let path = self.root().join(&rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, vec![b'x'; bytes]).unwrap();
        self.row(node, hash, &rel, bytes, created).await;
        rel
    }

    async fn row(&self, node: &str, hash: &str, recorded: &str, bytes: usize, created: &str) {
        let _g = crate::db::db_guard().await;
        let (_h, conn) = crate::db::open_conn(&self.db).await.unwrap();
        conn.execute(
            "INSERT INTO materializations (node_id, run_hash, artifact_path, artifact_format, \
             artifact_size_bytes, status, created_at) VALUES (?1, ?2, ?3, 'json', ?4, 'success', ?5)",
            turso::params![node, hash, recorded, bytes as i64, created],
        )
        .await
        .unwrap();
    }

    async fn sql(&self, sql: &str) {
        let _g = crate::db::db_guard().await;
        let (_h, conn) = crate::db::open_conn(&self.db).await.unwrap();
        conn.execute(sql, ()).await.unwrap();
    }

    async fn evicted_at(&self, recorded: &str) -> Option<String> {
        let _g = crate::db::db_guard().await;
        let (_h, conn) = crate::db::open_conn(&self.db).await.unwrap();
        let mut rows = conn
            .query(
                "SELECT evicted_at FROM materializations WHERE artifact_path = ?1",
                [recorded],
            )
            .await
            .unwrap();
        rows.next().await.unwrap().unwrap().get::<String>(0).ok()
    }

    fn exists(&self, rel: &str) -> bool {
        self.root().join(rel).exists()
    }

    async fn total(&self) -> u64 {
        survey(&self.project, &envs(&self.project))
            .await
            .unwrap()
            .total
    }

    /// A limit whose low watermark is exactly `target` bytes.
    fn limit_for(target: u64) -> MaxSize {
        MaxSize::Bytes(target * 100 / LOW_WATERMARK_PERCENT)
    }

    async fn gc(&self, max_size: MaxSize, dry_run: bool) -> GcResult {
        let opts = GcOptions {
            max_size: Some(max_size),
            dry_run,
            env: None,
        };
        gc_in(&self.project, MaxSize::Off, &opts).await.unwrap()
    }
}

fn paths(result: &GcResult) -> Vec<(String, String)> {
    result
        .evicted
        .iter()
        .map(|e| (e.tier.clone(), e.path.clone()))
        .collect()
}

/// Leftovers, a superseded result, a store-less current result read long ago, and one read
/// recently: the four kinds a plan orders.
async fn project() -> (Fixture, Vec<String>) {
    let f = Fixture::new().await;
    let aside = f
        .root()
        .join(".barca/artifacts/p.py--a/h0.json.moved-aside");
    std::fs::create_dir_all(&aside).unwrap();
    std::fs::write(aside.join("junk"), vec![b'x'; 1000]).unwrap();
    let old = f.result("p.py:a", "h1", 1000, "2026-10-01 00:00:00").await;
    let current = f.result("p.py:a", "h2", 1000, "2026-10-02 00:00:00").await;
    let cold = f.result("p.py:b", "hb", 1000, "2026-10-01 00:00:00").await;
    let hot = f.result("p.py:c", "hc", 1000, "2026-09-01 00:00:00").await;
    f.sql(
        "UPDATE materializations SET last_read_at = '2026-10-19 00:00:00' WHERE node_id = 'p.py:c'",
    )
    .await;
    (f, vec![old, current, cold, hot])
}

#[tokio::test]
async fn gc_evicts_tier_by_tier_and_stops_at_the_low_watermark() {
    let (f, files) = project().await;
    let [old, current, cold, hot] = files.try_into().unwrap();
    let total = f.total().await;
    // Room for everything but the leftover, the superseded result and one current result.
    let result = f.gc(Fixture::limit_for(total - 3000), false).await;
    assert_eq!(
        paths(&result),
        [
            (
                "leftovers".to_string(),
                ".barca/artifacts/p.py--a/h0.json.moved-aside".to_string()
            ),
            ("superseded".to_string(), old.clone()),
            // The least recently used of the current results: `b` (never read since it was
            // written on 10-01) before `a` (written 10-02) and `c` (read 10-19).
            ("current".to_string(), cold.clone()),
        ]
    );
    assert_eq!(result.after_bytes, total - 3000);
    assert!(!f.exists(&old) && !f.exists(&cold));
    assert!(f.exists(&current) && f.exists(&hot));
    assert!(f.evicted_at(&old).await.is_some());
    assert!(f.evicted_at(&cold).await.is_some());
    assert!(f.evicted_at(&current).await.is_none());
}

#[tokio::test]
async fn a_dry_run_lists_exactly_what_gc_then_deletes() {
    let (f, _) = project().await;
    let total = f.total().await;
    let limit = Fixture::limit_for(total - 2500);
    let predicted = f.gc(limit, true).await;
    assert!(predicted.dry_run);
    assert_eq!(predicted.evicted_files, 3, "{predicted:?}");
    // Nothing was deleted by the dry run.
    assert_eq!(f.total().await, total);
    let done = f.gc(limit, false).await;
    assert_eq!(paths(&predicted), paths(&done));
    assert_eq!(predicted.after_bytes, done.after_bytes);
}

#[tokio::test]
async fn with_the_limit_off_gc_evicts_every_leftover_and_superseded_result_only() {
    let (f, files) = project().await;
    let result = f.gc(MaxSize::Off, false).await;
    assert_eq!(result.limit_bytes, None);
    assert_eq!(
        result
            .evicted
            .iter()
            .map(|e| e.tier.as_str())
            .collect::<Vec<_>>(),
        ["leftovers", "superseded"]
    );
    assert!(f.exists(&files[1]) && f.exists(&files[2]) && f.exists(&files[3]));
}

#[tokio::test]
async fn the_estimate_after_a_walk_is_the_walked_size() {
    let (f, files) = project().await;
    let all = envs(&f.project);
    let walked = survey(&f.project, &all).await.unwrap().total;
    let state = read_state(&f.project).unwrap();
    assert_eq!(estimate(&f.project, &all, &state).await.unwrap(), walked);
    // An eviction recorded in the DB is reflected without another walk.
    let gone = f.root().join(&files[0]);
    let bytes = std::fs::metadata(&gone).unwrap().len();
    std::fs::remove_file(&gone).unwrap();
    mark_evicted(&f.db, &[files[0].as_str()]).await;
    let now = walk(&f.project, true);
    assert_eq!(
        estimate(&f.project, &all, &state).await.unwrap(),
        now.other + now.artifacts
    );
    assert!(now.other + now.artifacts < walked + 64 * 1024 - bytes);
}

#[tokio::test]
async fn the_check_after_a_command_keeps_what_the_command_produced_and_read() {
    let (f, files) = project().await;
    let own = Own {
        recorded: files.iter().cloned().collect(),
        ..Default::default()
    };
    let report = check_in(&f.project, MaxSize::Bytes(1), &own, None)
        .await
        .unwrap()
        .unwrap();
    // Only the leftover could go; the command's own results all stay.
    assert_eq!(report.evicted_files, 1);
    assert!(files.iter().all(|rel| f.exists(rel)));
}

#[tokio::test]
async fn a_result_whose_upload_is_pending_is_never_evicted() {
    // The local copy of a result whose upload failed (#390) is the only copy: neither a
    // superseded nor a current one is evicted while its upload is pending.
    let (f, files) = project().await;
    let [old, _current, cold, _hot] = files.try_into().unwrap();
    let pending = |rel: &str, node: &str| crate::pending_uploads::PendingUpload {
        store_path: rel.to_string(),
        node_id: node.to_string(),
        run_hash: None,
        run_id: None,
        local_path: f.root().join(rel).display().to_string(),
        error: Some("upload failed".into()),
    };
    crate::pending_uploads::record(&f.db, &[pending(&old, "p.py:a"), pending(&cold, "p.py:b")])
        .await
        .unwrap();
    let result = f.gc(MaxSize::Bytes(1), false).await;
    let evicted: Vec<String> = paths(&result).into_iter().map(|(_, p)| p).collect();
    assert!(!evicted.contains(&old) && !evicted.contains(&cold), "{evicted:?}");
    assert!(f.exists(&old) && f.exists(&cold));
    assert!(!evicted.is_empty(), "the rest is still evicted");
}

#[tokio::test]
async fn metadata_backups_are_counted_and_never_evicted() {
    // `barca state restore` (#471) needs its copies: `.barca/backups/` and a named
    // environment's `backups/` count toward the limit but are never deleted.
    let (f, _files) = project().await;
    let before = f.total().await;
    let backups = [
        ".barca/backups/2026-10-20T00-00-00Z.db",
        ".barca/backups/2026-10-20T00-00-00Z.json",
        ".barca/backups/2026-10-20T00-01-00Z.db.unsent",
        ".barca/envs/dev/backups/2026-10-20T00-00-00Z.db",
    ];
    for rel in backups {
        let path = f.root().join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, vec![b'x'; 500]).unwrap();
    }
    assert_eq!(f.total().await, before + 2000);
    let result = f.gc(MaxSize::Bytes(1), false).await;
    assert!(!result.evicted.is_empty());
    for rel in backups {
        assert!(f.exists(rel), "{rel} was evicted");
        assert!(paths(&result).iter().all(|(_, p)| !p.contains("backups")));
    }
}

#[tokio::test]
async fn runs_in_flight_in_this_process_hold_their_results() {
    let (f, files) = project().await;
    let pin = pin_at("other-run", f.root());
    // Before its plan is known, only leftovers and superseded results may go.
    let result = f.gc(MaxSize::Bytes(1), false).await;
    assert_eq!(
        result
            .evicted
            .iter()
            .map(|e| e.tier.as_str())
            .collect::<Vec<_>>(),
        ["leftovers", "superseded"]
    );
    assert!(result.restricted.is_some());
    // Planned: its nodes' current results, and its run hashes, stay.
    pin.nodes(["p.py:b".to_string()]);
    pin.run_hashes([&"hc".to_string()]);
    let result = f.gc(MaxSize::Bytes(1), false).await;
    assert_eq!(paths(&result), [("current".to_string(), files[1].clone())]);
    assert!(f.exists(&files[2]) && f.exists(&files[3]));
    drop(pin);
    let result = f.gc(MaxSize::Bytes(1), false).await;
    assert_eq!(result.evicted_files, 2);
}

#[tokio::test]
async fn a_live_run_in_another_process_limits_eviction_to_tiers_1_and_2() {
    let (f, files) = project().await;
    f.sql(&format!(
        "INSERT INTO runs (run_id, command, files, status, pid, host) VALUES \
         ('elsewhere', 'get', '[]', 'running', {}, '{}')",
        std::process::id(),
        crate::db::local_host()
    ))
    .await;
    let predicted = f.gc(MaxSize::Bytes(1), true).await;
    let result = f.gc(MaxSize::Bytes(1), false).await;
    assert_eq!(
        result.restricted.as_deref(),
        Some("run elsewhere is in progress")
    );
    assert_eq!(
        result
            .evicted
            .iter()
            .map(|e| e.tier.as_str())
            .collect::<Vec<_>>(),
        ["leftovers", "superseded"]
    );
    assert_eq!(paths(&predicted), paths(&result));
    assert!(files[1..].iter().all(|rel| f.exists(rel)));
    // Once it has finished, the rest may go.
    f.sql("UPDATE runs SET status = 'success' WHERE run_id = 'elsewhere'")
        .await;
    assert_eq!(f.gc(MaxSize::Bytes(1), false).await.evicted_files, 3);
}

#[tokio::test]
async fn a_manual_node_keeps_its_current_result_without_a_store_copy() {
    let f = Fixture::new().await;
    std::fs::write(
        f.root().join("p.py"),
        "from barca import asset, Manual\n\n@asset(freshness=Manual)\ndef m() -> int:\n    return 1\n\n@asset()\ndef a() -> int:\n    return 2\n",
    )
    .unwrap();
    let manual = f.result("p.py:m", "hm", 100, "2026-10-01 00:00:00").await;
    let older = f.result("p.py:m", "hm0", 100, "2026-09-01 00:00:00").await;
    f.sql("UPDATE materializations SET id = 0 WHERE run_hash = 'hm0'")
        .await;
    let plain = f.result("p.py:a", "ha", 100, "2026-10-01 00:00:00").await;
    let result = f.gc(MaxSize::Bytes(1), false).await;
    assert_eq!(
        paths(&result),
        [
            ("superseded".to_string(), older),
            ("current".to_string(), plain)
        ]
    );
    assert!(f.exists(&manual));
}

#[tokio::test]
async fn a_source_file_that_does_not_parse_protects_every_current_result() {
    let (f, files) = project().await;
    std::fs::write(f.root().join("p.py"), "from barca import asset\ndef (:\n").unwrap();
    let result = f.gc(MaxSize::Bytes(1), false).await;
    assert_eq!(
        result
            .evicted
            .iter()
            .map(|e| e.tier.as_str())
            .collect::<Vec<_>>(),
        ["leftovers", "superseded"]
    );
    assert!(files[1..].iter().all(|rel| f.exists(rel)));
}

#[tokio::test]
async fn only_files_under_barca_are_ever_deleted() {
    let f = Fixture::new().await;
    let outside = f.root().parent().unwrap().join("elsewhere.json");
    std::fs::write(&outside, vec![b'x'; 100]).unwrap();
    f.row(
        "p.py:a",
        "h1",
        &outside.display().to_string(),
        100,
        "2026-10-01 00:00:00",
    )
    .await;
    f.row("p.py:a", "h2", "../escape.json", 100, "2026-10-02 00:00:00")
        .await;
    std::fs::write(f.root().parent().unwrap().join("escape.json"), b"x").unwrap();
    let result = f.gc(MaxSize::Bytes(1), false).await;
    assert!(result.evicted.is_empty(), "{result:?}");
    assert!(outside.exists());
    assert!(f.root().parent().unwrap().join("escape.json").exists());
    // The metadata DB and its lock are never candidates.
    assert!(Path::new(&f.db).exists());
}

#[tokio::test]
async fn staging_of_a_running_process_and_fresh_sql_copies_stay() {
    let f = Fixture::new().await;
    let staging = f.root().join(".barca/staging");
    let dead = staging.join("999999999");
    let live = staging.join(std::process::id().to_string());
    for dir in [&dead, &live] {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join("upload-x"), b"data").unwrap();
    }
    let sql = f.root().join(".barca/sql-cache/s3/bucket");
    std::fs::create_dir_all(&sql).unwrap();
    std::fs::write(sql.join("fresh.parquet"), b"data").unwrap();
    std::fs::write(sql.join("fresh.parquet.version"), b"{}").unwrap();
    let old = sql.join("old.parquet");
    std::fs::write(&old, b"data").unwrap();
    std::fs::write(sql.join("old.parquet.version"), b"{}").unwrap();
    let long_ago = std::time::SystemTime::now() - Duration::from_secs(3600);
    for file in [&old, &sql.join("old.parquet.version")] {
        std::fs::File::options()
            .write(true)
            .open(file)
            .unwrap()
            .set_modified(long_ago)
            .unwrap();
    }
    let result = f.gc(MaxSize::Off, false).await;
    let evicted: Vec<&str> = result.evicted.iter().map(|e| e.path.as_str()).collect();
    assert_eq!(
        evicted,
        [
            ".barca/staging/999999999",
            ".barca/sql-cache/s3/bucket/old.parquet"
        ]
    );
    assert!(!dead.exists() && live.exists());
    assert!(!sql.join("old.parquet.version").exists());
    assert!(sql.join("fresh.parquet").exists());
}

#[tokio::test]
async fn read_results_are_no_longer_evicted_and_move_their_last_read_time() {
    let f = Fixture::new().await;
    let rel = f.result("p.py:a", "h1", 10, "2026-10-01 00:00:00").await;
    mark_evicted(&f.db, &[rel.as_str()]).await;
    assert_eq!(
        crate::db::evicted_among(&f.db, &[rel.as_str()]).await,
        HashSet::from([rel.clone()])
    );
    crate::db::touch_read(&f.db, std::slice::from_ref(&rel)).await;
    assert!(
        crate::db::evicted_among(&f.db, &[rel.as_str()])
            .await
            .is_empty()
    );
    let (rows, _) = rows_of(&f.project, &envs(&f.project)[0]).await.unwrap();
    assert!(rows[0].last_read_at.is_some());
}

/// Under the limit, the check after a command does not walk the artifact directories or read
/// the rows: one aggregate query per environment (the 100,000-row bound is measured on the
/// release binary in `python/tests/test_cache_limit.py`).
#[tokio::test]
async fn under_the_limit_the_check_after_a_command_does_not_walk() {
    let (f, _) = project().await;
    let limit = MaxSize::Bytes(u64::MAX / 2);
    assert!(read_state(&f.project).is_none());
    // The first check walks: there is no correction yet.
    assert!(
        check_in(&f.project, limit, &Own::default(), None)
            .await
            .unwrap()
            .is_none()
    );
    let mut state = read_state(&f.project).unwrap();
    state.surveyed_at -= 60;
    write_state(&f.project, &state);
    assert!(
        check_in(&f.project, limit, &Own::default(), None)
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        read_state(&f.project).unwrap().surveyed_at,
        state.surveyed_at
    );
    // A day later it walks again.
    state.surveyed_at -= SURVEY_EVERY.as_secs();
    write_state(&f.project, &state);
    assert!(
        check_in(&f.project, limit, &Own::default(), None)
            .await
            .unwrap()
            .is_none()
    );
    assert!(read_state(&f.project).unwrap().surveyed_at > state.surveyed_at);
}

#[tokio::test]
async fn a_working_set_larger_than_the_limit_is_not_planned_again_until_it_grows() {
    let (f, files) = project().await;
    let own = Own {
        recorded: files.iter().cloned().collect(),
        ..Default::default()
    };
    let first = check_in(&f.project, MaxSize::Bytes(1), &own, None)
        .await
        .unwrap();
    assert_eq!(first.unwrap().evicted_files, 1);
    let state = read_state(&f.project).unwrap();
    assert!(state.short_at > 0);
    // Nothing more could go: the next check does not plan (nothing walked, nothing evicted).
    let again = check_in(&f.project, MaxSize::Bytes(1), &own, None)
        .await
        .unwrap();
    assert_eq!(again.unwrap().evicted_files, 0);
    assert_eq!(
        read_state(&f.project).unwrap().surveyed_at,
        state.surveyed_at
    );
}

#[tokio::test]
async fn a_version_1_database_from_an_earlier_release_gains_the_columns() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("old.db").display().to_string();
    crate::db::init_db(&path).await.unwrap();
    {
        let _g = crate::db::db_guard().await;
        let (_h, conn) = crate::db::open_conn(&path).await.unwrap();
        conn.execute("DROP INDEX idx_mat_path", ()).await.unwrap();
        conn.execute("ALTER TABLE materializations DROP COLUMN evicted_at", ())
            .await
            .unwrap();
        conn.execute("ALTER TABLE materializations DROP COLUMN last_read_at", ())
            .await
            .unwrap();
        conn.execute(
            "INSERT INTO materializations (node_id, status) VALUES ('p.py:a', 'success')",
            (),
        )
        .await
        .unwrap();
        assert!(crate::db_schema::additions_missing(&conn).await.unwrap());
    }
    crate::db::init_db(&path).await.unwrap();
    let _g = crate::db::db_guard().await;
    let (_h, conn) = crate::db::open_conn(&path).await.unwrap();
    assert!(!crate::db_schema::additions_missing(&conn).await.unwrap());
    let mut rows = conn
        .query(
            "SELECT COUNT(*) FROM materializations WHERE evicted_at IS NULL AND last_read_at IS NULL",
            (),
        )
        .await
        .unwrap();
    assert_eq!(
        rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap(),
        1
    );
    assert_eq!(crate::db_schema::read_version(&conn).await.unwrap(), 1);
}
