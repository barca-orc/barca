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
            libsql::params![node, hash, recorded, bytes as i64, created],
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
    assert!(
        !evicted.contains(&old) && !evicted.contains(&cold),
        "{evicted:?}"
    );
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

/// #577: the ranges lazy inputs kept are counted and evicted as leftovers, a directory per store
/// object, least recently used first; one used in the last few minutes stays.
#[tokio::test]
async fn range_cache_directories_are_leftovers_and_fresh_ones_stay() {
    let f = Fixture::new().await;
    let cache = f.root().join(".barca/range-cache");
    let set_modified = |path: &std::path::Path, ago: u64| {
        std::fs::File::open(path)
            .unwrap()
            .set_modified(std::time::SystemTime::now() - Duration::from_secs(ago))
            .unwrap();
    };
    for (name, ago) in [("h1-old", 7200), ("h2-older", 9000), ("h3-fresh", 0)] {
        let dir = cache.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        let range = dir.join("0000000000000000-0000000000000004");
        std::fs::write(&range, b"data").unwrap();
        set_modified(&range, ago);
        set_modified(&dir, ago);
    }
    let dry = f.gc(MaxSize::Off, true).await;
    let leftovers: Vec<(&str, u64)> = dry
        .evicted
        .iter()
        .map(|e| (e.path.as_str(), e.bytes))
        .collect();
    assert_eq!(
        leftovers,
        [
            (".barca/range-cache/h2-older", 4),
            (".barca/range-cache/h1-old", 4)
        ]
    );
    f.gc(MaxSize::Off, false).await;
    assert!(!cache.join("h1-old").exists() && !cache.join("h2-older").exists());
    assert!(cache.join("h3-fresh").exists());
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
    let (rows, _) = rows_of(&f.project, &envs(&f.project)[0], None)
        .await
        .unwrap();
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
        // The index on evicted rows came later still.
        conn.execute("DROP INDEX idx_mat_evicted", ())
            .await
            .unwrap();
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

/// #560: a command that wrote nothing (a warm run) is not checked again when the last check,
/// under the same limit, found the cache under it. One that wrote something is.
#[tokio::test]
async fn a_command_that_wrote_nothing_skips_the_check_the_last_one_passed() {
    let (f, files) = project().await;
    let total = f.total().await;
    let limit = MaxSize::Bytes(total + 10_000);
    let wrote = Own {
        recorded: files.iter().cloned().collect(),
        ..Default::default()
    };
    let nothing = Own {
        wrote_nothing: true,
        ..wrote.clone()
    };
    // Surveys, finds the cache under the limit, and records that.
    assert!(
        check_in(&f.project, limit, &wrote, None)
            .await
            .unwrap()
            .is_none()
    );
    // Grown past the limit behind the check's back (another process, say).
    std::fs::write(f.root().join(".barca/grown"), vec![b'x'; 50_000]).unwrap();
    assert!(
        check_in(&f.project, limit, &nothing, None)
            .await
            .unwrap()
            .is_none(),
        "a command that wrote nothing is not checked again"
    );
    // Under another limit it is checked.
    let lower = MaxSize::Bytes(total + 20_000);
    assert!(
        check_in(&f.project, lower, &nothing, None)
            .await
            .unwrap()
            .is_some()
    );
    // A command that wrote something is checked, and finds the growth.
    assert!(
        check_in(&f.project, limit, &wrote, None)
            .await
            .unwrap()
            .is_some()
    );
}

/// A file in an artifact directory with no row, last written `ago` seconds ago.
fn unnamed(f: &Fixture, rel: &str, bytes: usize, ago: u64) -> String {
    let path = f.root().join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, vec![b'x'; bytes]).unwrap();
    std::fs::File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_modified(SystemTime::now() - Duration::from_secs(ago))
        .unwrap();
    rel.to_string()
}

fn held(result: &GcResult, what: &str) -> u64 {
    result
        .held
        .iter()
        .find(|h| h.what == what)
        .map_or(0, |h| h.bytes)
}

#[tokio::test]
async fn files_no_row_names_are_leftovers_and_recent_ones_stay() {
    // What `barca state reset` leaves (#552): files the old database named, which the new one
    // does not. They are leftovers; one written in the last minute may be a step's whose row
    // is about to be written.
    let f = Fixture::new().await;
    let named = f.result("p.py:a", "h1", 1000, "2026-10-01 00:00:00").await;
    let old = unnamed(&f, ".barca/artifacts/p.py--a/h0.json", 3000, 3600);
    let fresh = unnamed(&f, ".barca/artifacts/p.py--a/h9.json", 200, 0);
    // Under a limit that keeps the named result: the old file goes, the fresh one stays.
    let limit = Fixture::limit_for(f.total().await - 3000);
    let predicted = f.gc(limit, true).await;
    // `held` is what is left after evicting.
    assert_eq!(held(&predicted, "unreferenced"), 200, "{predicted:?}");
    assert_eq!(held(&predicted, "results"), 1000, "{predicted:?}");
    let result = f.gc(limit, false).await;
    assert_eq!(paths(&predicted), paths(&result));
    assert_eq!(paths(&result), [("leftovers".to_string(), old.clone())]);
    assert!(!f.exists(&old));
    assert!(f.exists(&fresh), "a file written a moment ago stays");
    assert!(f.exists(&named));
    // Past every leftover, the named file goes as a current result, never as a leftover.
    let result = f.gc(MaxSize::Bytes(1), false).await;
    assert_eq!(paths(&result), [("current".to_string(), named.clone())]);
    assert_eq!(held(&result, "unreferenced"), 200);
}

#[tokio::test]
async fn after_the_database_is_replaced_the_next_check_finds_the_old_files() {
    let f = Fixture::new().await;
    let a = f.result("p.py:a", "h1", 4000, "2026-10-01 00:00:00").await;
    let limit = MaxSize::Bytes(f.total().await + 1000);
    assert!(
        check_in(&f.project, limit, &Own::default(), None)
            .await
            .unwrap()
            .is_none()
    );
    // `barca state reset`: a new, empty database; the file stays, named by nothing.
    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{suffix}", f.db));
    }
    crate::db::init_db(&f.db).await.unwrap();
    // Written a few seconds ago, but before the database: no run can be about to name it.
    unnamed(&f, &a, 4000, 5);
    forget_survey_in(&f.project);
    // A smaller limit: without a fresh walk the estimate would count no results at all.
    let limit = MaxSize::Bytes(2000);
    let report = check_in(&f.project, limit, &Own::default(), None)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(report.evicted_files, 1, "{report:?}");
    assert!(!f.exists(&a));
}

#[tokio::test]
async fn files_no_row_names_stay_while_a_run_is_in_flight() {
    let f = Fixture::new().await;
    let old = unnamed(&f, ".barca/artifacts/p.py--a/h0.json", 3000, 3600);
    // Another process: its rows are not written yet.
    f.sql(&format!(
        "INSERT INTO runs (run_id, command, files, status, pid, host) VALUES \
         ('elsewhere', 'get', '[]', 'running', {}, '{}')",
        std::process::id(),
        crate::db::local_host()
    ))
    .await;
    let result = f.gc(MaxSize::Bytes(1), false).await;
    assert!(result.evicted.is_empty(), "{result:?}");
    assert_eq!(held(&result, "unreferenced"), 3000);
    f.sql("UPDATE runs SET status = 'success' WHERE run_id = 'elsewhere'")
        .await;
    // A run in this process.
    let pin = pin_at("this-process", f.root());
    pin.nodes(["p.py:z".to_string()]);
    assert!(f.gc(MaxSize::Bytes(1), false).await.evicted.is_empty());
    drop(pin);
    assert_eq!(f.gc(MaxSize::Bytes(1), false).await.evicted_files, 1);
    assert!(!f.exists(&old));
}

#[tokio::test]
async fn a_file_another_environment_names_is_not_a_leftover() {
    let f = Fixture::new().await;
    let dev_db = f.root().join(".barca/envs/dev/metadata.db");
    std::fs::create_dir_all(dev_db.parent().unwrap()).unwrap();
    let dev_db = dev_db.display().to_string();
    crate::db::init_db(&dev_db).await.unwrap();
    let rel = unnamed(&f, ".barca/envs/dev/artifacts/p.py--a/h1.json", 1000, 3600);
    {
        let _g = crate::db::db_guard().await;
        let (_h, conn) = crate::db::open_conn(&dev_db).await.unwrap();
        conn.execute(
            "INSERT INTO materializations (node_id, run_hash, artifact_path, artifact_format, \
             artifact_size_bytes, status, created_at) VALUES ('p.py:a', 'h1', ?1, 'json', 1000, \
             'success', '2026-10-01 00:00:00')",
            [rel.as_str()],
        )
        .await
        .unwrap();
    }
    // `barca gc --env default` reads only the default environment's results, but every
    // environment's rows say which files are named.
    let opts = GcOptions {
        max_size: Some(MaxSize::Bytes(1)),
        dry_run: false,
        env: Some(crate::config::DEFAULT_ENV.to_string()),
    };
    let result = gc_in(&f.project, MaxSize::Off, &opts).await.unwrap();
    assert!(result.evicted.is_empty(), "{result:?}");
    assert!(f.exists(&rel));
}

#[tokio::test]
async fn gc_says_what_holds_the_space_it_cannot_free() {
    let f = Fixture::new().await;
    for (rel, bytes) in [
        (".barca/metadata.db.reset-2026-10-20T00-00-00.000Z", 500_000),
        (".barca/metadata.db.pre-restore", 3000),
        (".barca/backups/2026-10-20T00-00-00Z.db", 2000),
    ] {
        let path = f.root().join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, vec![b'x'; bytes]).unwrap();
    }
    let result = f.gc(MaxSize::Bytes(1), true).await;
    assert!(result.evicted.is_empty());
    assert_eq!(held(&result, "db_copies"), 503_000, "{result:?}");
    assert_eq!(held(&result, "backups"), 2000);
    assert!(held(&result, "metadata") > 0);
    assert_eq!(result.held[0].what, "db_copies", "largest first");
    assert!(
        result
            .held
            .iter()
            .all(|h| h.what != "db_copies" || !h.evictable)
    );
}

/// The store check window (#593) lives in the same file as the survey: a survey keeps it, and
/// it is keyed by environment, store URI and storage options.
#[tokio::test]
async fn a_survey_keeps_the_store_checks_and_each_store_has_its_own() {
    let (f, _) = project().await;
    let mut cfg = crate::config::resolve_in(None, f.root()).unwrap();
    cfg.artifact_root = "s3://bucket/p/default/artifacts".into();
    update_store_checks(&f.project, StoreCheck::of(&cfg, 100), true);
    let mut other = cfg.clone();
    other.storage_options_json = Some(r#"{"s3":{"anon":true}}"#.into());
    update_store_checks(&f.project, StoreCheck::of(&other, 200), true);
    assert_eq!(read_state(&f.project).unwrap().store_checks.len(), 2);
    // Remembering again replaces that store's entry only.
    update_store_checks(&f.project, StoreCheck::of(&cfg, 300), true);
    let checks = read_state(&f.project).unwrap().store_checks;
    assert_eq!(checks.iter().map(|c| c.at).collect::<Vec<_>>(), [200, 300]);

    check_in(
        &f.project,
        MaxSize::Bytes(u64::MAX / 2),
        &Own::default(),
        None,
    )
    .await
    .unwrap();
    let state = read_state(&f.project).unwrap();
    assert!(state.surveyed_at > 0);
    assert_eq!(state.store_checks, checks);

    update_store_checks(&f.project, StoreCheck::of(&other, 0), false);
    let left = read_state(&f.project).unwrap().store_checks;
    assert_eq!(left.len(), 1);
    assert!(left[0].same_store(&StoreCheck::of(&cfg, 0)));
}

#[tokio::test]
async fn routine_estimates_advance_only_over_new_rows_and_surveys_correct_reused_paths() {
    let f = Fixture::new().await;
    f.result("p.py:sensor", "same", 100, "2026-10-01").await;
    f.total().await;
    let environments = envs(&f.project);
    estimate(&f.project, &environments, &read_state(&f.project).unwrap())
        .await
        .unwrap();
    let first = read_state(&f.project).unwrap();
    let recorded = first.recorded.get(&f.db).unwrap();
    assert_eq!((recorded.through, recorded.bytes), (1, 100));
    f.result("p.py:sensor", "same", 150, "2026-10-02").await;
    estimate(&f.project, &environments, &read_state(&f.project).unwrap())
        .await
        .unwrap();
    let second = read_state(&f.project).unwrap();
    let recorded = second.recorded.get(&f.db).unwrap();
    assert_eq!((recorded.through, recorded.bytes), (2, 250));
    estimate(&f.project, &environments, &second).await.unwrap();
    assert_eq!(
        read_state(&f.project)
            .unwrap()
            .recorded
            .get(&f.db)
            .unwrap()
            .bytes,
        250
    );
    f.total().await;
    estimate(&f.project, &environments, &read_state(&f.project).unwrap())
        .await
        .unwrap();
    assert_eq!(
        read_state(&f.project)
            .unwrap()
            .recorded
            .get(&f.db)
            .unwrap()
            .bytes,
        150
    );
    // A replaced/truncated database invalidates the old high-water mark.
    f.sql("DELETE FROM materializations").await;
    estimate(&f.project, &environments, &read_state(&f.project).unwrap())
        .await
        .unwrap();
    let reset = read_state(&f.project).unwrap();
    let recorded = reset.recorded.get(&f.db).unwrap();
    assert_eq!((recorded.through, recorded.bytes), (0, 0));
}

/// A scan reads every row once, in id order, across chunks, whatever the ids.
#[tokio::test]
async fn a_scan_reads_every_row_once_across_chunks() {
    let f = Fixture::new().await;
    let n = SCAN_CHUNK * 2 + 7;
    f.sql(&format!(
        "WITH RECURSIVE r(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM r WHERE i < {n}) \
         INSERT INTO materializations (node_id, run_hash, status) \
         SELECT 'p.py:a[k=' || i || ']', 'h', 'success' FROM r"
    ))
    .await;
    f.sql("UPDATE materializations SET id = 0 WHERE id = 1")
        .await;
    f.sql("UPDATE materializations SET id = -5 WHERE id = 2")
        .await;
    let ids = scan(&f.db, "node_id", "status = 'success'", |row| {
        Ok(row.get::<i64>(0).unwrap())
    })
    .await
    .unwrap();
    assert_eq!(ids.len(), n);
    assert!(
        ids.windows(2).all(|w| w[0] < w[1]),
        "in id order, once each"
    );
    assert_eq!(ids[..2], [-5, 0]);
}

/// A writer is not held for the whole of a long scan: the lock is let go between chunks. One
/// query over every row held it until the last row, and a server's runs waited that long.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_writer_does_not_wait_for_a_whole_scan() {
    let f = Fixture::new().await;
    let n = SCAN_CHUNK * 3;
    f.sql(&format!(
        "WITH RECURSIVE r(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM r WHERE i < {n}) \
         INSERT INTO materializations (node_id, run_hash, status) \
         SELECT 'p.py:a[k=' || i || ']', 'h', 'success' FROM r"
    ))
    .await;
    let db = f.db.clone();
    let scanning = tokio::spawn(async move {
        let rows = scan(&db, "node_id", "status = 'success'", |_| {
            std::thread::sleep(std::time::Duration::from_micros(20));
            Ok(())
        })
        .await
        .unwrap();
        (rows.len(), Instant::now())
    });
    // Once the scan holds the lock for its first chunk.
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    let mut written = Instant::now();
    for i in 0..3 {
        crate::db::create_run(&f.db, &format!("r{i}"), "get", "[]", None, Some(1))
            .await
            .unwrap();
        written = Instant::now();
    }
    let (read, scanned) = scanning.await.unwrap();
    assert!(read >= n);
    assert!(
        written < scanned,
        "the writes landed between chunks, before the scan ended"
    );
}

/// A row's file found by the walk is not looked at again: the walk's sizes give the same rows.
#[tokio::test]
async fn rows_from_the_walk_are_the_rows_from_the_disk() {
    let (f, _) = project().await;
    let env = &envs(&f.project)[0];
    let walked = walk(&f.project, true);
    let sizes: HashMap<PathBuf, u64> = walked
        .files
        .iter()
        .map(|(path, bytes, _)| (f.project.key(path), *bytes))
        .collect();
    let (from_disk, recorded) = rows_of(&f.project, env, None).await.unwrap();
    let (from_walk, recorded_walk) = rows_of(&f.project, env, Some(&sizes)).await.unwrap();
    let view = |rows: &[plan::Row]| -> Vec<(i64, bool, u64)> {
        rows.iter().map(|r| (r.id, r.present, r.bytes)).collect()
    };
    assert_eq!(view(&from_walk), view(&from_disk));
    assert_eq!(recorded_walk, recorded);
    // A file the walk did not see (written since) is looked at on disk.
    let (rows, _) = rows_of(&f.project, env, Some(&HashMap::new()))
        .await
        .unwrap();
    assert_eq!(view(&rows), view(&from_disk));
}

/// Under `serve`'s runtime the check runs on a thread of its own: the event loop's other tasks
/// go on while it blocks.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn a_blocking_check_leaves_the_event_loop_running() {
    let ticks = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let ticker = tokio::spawn({
        let ticks = ticks.clone();
        async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                ticks.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }
        }
    });
    tokio::spawn(off_the_event_loop(async {
        std::thread::sleep(std::time::Duration::from_millis(300));
    }))
    .await
    .unwrap();
    ticker.abort();
    assert!(
        ticks.load(std::sync::atomic::Ordering::Relaxed) >= 10,
        "the ticker ran while the check blocked"
    );
}

/// One survey of a `.barca/` at a time in a process; another one is free to start.
#[test]
fn a_second_survey_of_the_same_cache_waits_for_none() {
    let dir = tempfile::tempdir().unwrap();
    let (a, b) = (dir.path().join("a/.barca"), dir.path().join("b/.barca"));
    let first = Surveying::start(&a).expect("nothing surveys a yet");
    assert!(Surveying::start(&a).is_none(), "a is being surveyed");
    assert!(Surveying::start(&b).is_some(), "b is another cache");
    drop(first);
    assert!(Surveying::start(&a).is_some());
}
