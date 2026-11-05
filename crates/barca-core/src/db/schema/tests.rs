use super::*;

#[tokio::test]
async fn schema_has_timing_columns() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("test.db").to_string_lossy().to_string();
    init_db(&db_path).await.unwrap();

    let (_db, conn) = open_conn(&db_path).await.unwrap();
    let mut rows = conn
        .query("PRAGMA table_info(materializations)", ())
        .await
        .unwrap();
    let mut columns = Vec::new();
    while let Some(row) = rows.next().await.unwrap() {
        columns.push(row.get::<String>(1).unwrap());
    }
    assert!(columns.contains(&"cpu_seconds".to_string()));
    assert!(columns.contains(&"max_rss_bytes".to_string()));
}

#[tokio::test]
async fn schema_has_artifact_columns() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("test.db").to_string_lossy().to_string();
    init_db(&db_path).await.unwrap();

    let (_db, conn) = open_conn(&db_path).await.unwrap();
    let mut rows = conn
        .query("PRAGMA table_info(materializations)", ())
        .await
        .unwrap();
    let mut columns = Vec::new();
    while let Some(row) = rows.next().await.unwrap() {
        columns.push(row.get::<String>(1).unwrap());
    }

    assert!(columns.contains(&"artifact_path".to_string()));
    assert!(columns.contains(&"artifact_format".to_string()));
    assert!(columns.contains(&"artifact_size_bytes".to_string()));
    // Error/retry tracking columns.
    assert!(columns.contains(&"error_message".to_string()));
    assert!(columns.contains(&"error_traceback".to_string()));
    assert!(columns.contains(&"attempts".to_string()));
    // Old column still exists for backward compat
    assert!(columns.contains(&"output_json".to_string()));
}
