use agentisan::{
    fixture::{initialize, read_credential},
    model::{GroupId, Query},
    registry::{Registry, RegistryError},
};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

#[tokio::test]
async fn newer_database_version_is_rejected_without_changing_schema() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("future.sqlite");
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(
            SqliteConnectOptions::new()
                .filename(&path)
                .create_if_missing(true),
        )
        .await
        .unwrap();
    sqlx::query("PRAGMA user_version=99")
        .execute(&pool)
        .await
        .unwrap();
    assert!(matches!(
        Registry::open(&path).await,
        Err(RegistryError::Invalid(_))
    ));
    let version: i64 = sqlx::query_scalar("PRAGMA user_version")
        .fetch_one(&pool)
        .await
        .unwrap();
    let tables: i64 = sqlx::query_scalar("SELECT count(*) FROM sqlite_master WHERE type='table'")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(version, 99);
    assert_eq!(tables, 0);
    pool.close().await;
}

#[tokio::test]
async fn schema_seven_migrates_controller_requests_atomically() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("old.sqlite");
    Registry::open(&path).await.unwrap().close().await;
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(SqliteConnectOptions::new().filename(&path))
        .await
        .unwrap();
    sqlx::raw_sql("DROP TABLE run_requests; PRAGMA user_version=7;")
        .execute(&pool)
        .await
        .unwrap();
    pool.close().await;

    Registry::open(&path).await.unwrap().close().await;
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(SqliteConnectOptions::new().filename(&path))
        .await
        .unwrap();
    let version: i64 = sqlx::query_scalar("PRAGMA user_version")
        .fetch_one(&pool)
        .await
        .unwrap();
    let table: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM sqlite_master WHERE type='table' AND name='run_requests'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!((version, table), (8, 1));
    pool.close().await;
}

#[tokio::test]
async fn local_initializer_preserves_credentials_and_inspection_is_read_only() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("state");
    let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/registry.json");
    initialize(&data, &fixture).await.unwrap();
    let credential = data.join("credentials/inventory_reader.token");
    let token = read_credential(&credential).unwrap();
    initialize(&data, &fixture).await.unwrap();
    assert_eq!(token, read_credential(&credential).unwrap());
    let path = data.join("registry.sqlite3");
    let registry = Registry::open(&path).await.unwrap();
    let watcher = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(SqliteConnectOptions::new().filename(&path))
        .await
        .unwrap();
    let version_before: i64 = sqlx::query_scalar("PRAGMA data_version")
        .fetch_one(&watcher)
        .await
        .unwrap();
    registry
        .inspect(Some(&token), Query::Whoami {})
        .await
        .unwrap();
    registry
        .inspect(Some(&token), Query::GroupsList {})
        .await
        .unwrap();
    registry
        .inspect(
            Some(&token),
            Query::TeamsList {
                group_id: GroupId::from("inventory"),
            },
        )
        .await
        .unwrap();
    let version_after: i64 = sqlx::query_scalar("PRAGMA data_version")
        .fetch_one(&watcher)
        .await
        .unwrap();
    assert_eq!(
        version_before, version_after,
        "inspection must not commit database changes"
    );
    registry.close().await;
    watcher.close().await;
}

#[cfg(unix)]
#[test]
fn credentials_reject_shared_permissions_and_symlinks() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("credential");
    std::fs::write(&file, "a".repeat(64)).unwrap();
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert!(read_credential(&file).is_err());
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).unwrap();
    assert!(read_credential(&file).is_ok());
    let alias = dir.path().join("alias");
    symlink(&file, &alias).unwrap();
    assert!(read_credential(&alias).is_err());

    let state = dir.path().join("state");
    agentisan::fixture::private_dir(&state).unwrap();
    let absent = dir.path().join("must-not-be-created");
    symlink(&absent, state.join("registry.sqlite3")).unwrap();
    assert!(agentisan::fixture::database_path(&state).is_err());
    assert!(!absent.exists());
}

#[tokio::test]
async fn initializer_lock_covers_other_processes_before_any_credential_creation() {
    let temp = tempfile::tempdir().unwrap();
    let data = temp.path().join("state");
    let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/registry.json");
    let lock = agentisan::fixture::admin_lock(&data).unwrap();
    let out = tokio::process::Command::new(env!("CARGO_BIN_EXE_agentisan"))
        .arg("--data-dir")
        .arg(&data)
        .args(["init", "--fixture"])
        .arg(&fixture)
        .kill_on_drop(true)
        .output()
        .await
        .unwrap();
    assert!(!out.status.success());
    assert!(!data.join("credentials").exists());
    drop(lock);
    initialize(&data, &fixture).await.unwrap();
    assert!(data.join("credentials/inventory_reader.token").is_file());
}

#[tokio::test]
async fn case_distinct_principals_remain_distinct_and_legacy_files_migrate() {
    use agentisan::model::{Fixture, Principal};
    let temp = tempfile::tempdir().unwrap();
    let data = temp.path().join("state");
    let mut fx: Fixture = serde_json::from_str(include_str!("../examples/registry.json")).unwrap();
    fx.principals = vec![
        Principal {
            id: "Ops".into(),
            agent_id: None,
            group_ids: vec!["inventory".into()],
        },
        Principal {
            id: "ops".into(),
            agent_id: None,
            group_ids: vec!["inventory".into()],
        },
    ];
    let path = temp.path().join("fixture.json");
    std::fs::write(&path, serde_json::to_vec(&fx).unwrap()).unwrap();
    let files = initialize(&data, &path).await.unwrap();
    assert_ne!(
        files[0].to_string_lossy().to_lowercase(),
        files[1].to_string_lossy().to_lowercase()
    );
    let first = read_credential(&files[0]).unwrap();
    let second = read_credential(&files[1]).unwrap();
    assert_ne!(first, second);
    let r = Registry::open(&data.join("registry.sqlite3"))
        .await
        .unwrap();
    assert_eq!(
        r.inspect(Some(&first), Query::Whoami {}).await.unwrap()["principal_id"],
        "Ops"
    );
    assert_eq!(
        r.inspect(Some(&second), Query::Whoami {}).await.unwrap()["principal_id"],
        "ops"
    );
    r.close().await;

    // A separate legacy installation had only the uppercase identity registered.
    let legacy_data = temp.path().join("legacy-state");
    fx.principals.truncate(1);
    std::fs::write(&path, serde_json::to_vec(&fx).unwrap()).unwrap();
    let files = initialize(&legacy_data, &path).await.unwrap();
    let token = read_credential(&files[0]).unwrap();
    let legacy = legacy_data.join("credentials/Ops.token");
    std::fs::rename(&files[0], &legacy).unwrap();
    let migrated = initialize(&legacy_data, &path).await.unwrap();
    assert!(legacy.is_file());
    assert_eq!(read_credential(&migrated[0]).unwrap(), token);
}

#[tokio::test]
async fn failed_first_import_does_not_make_the_service_ready() {
    let temp = tempfile::tempdir().unwrap();
    let data = temp.path().join("state");
    agentisan::fixture::private_dir(&data).unwrap();
    agentisan::fixture::private_dir(&data.join("credentials")).unwrap();
    let bad = data.join("credentials/support_reader.token");
    std::fs::write(&bad, "invalid credential").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&bad, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/registry.json");
    assert!(initialize(&data, &fixture).await.is_err());
    assert!(!data.join("credentials/inventory_reader.token").exists());
    let registry = Registry::open(&data.join("registry.sqlite3"))
        .await
        .unwrap();
    assert!(!registry.is_initialized().await.unwrap());
    registry.close().await;
    let result = tokio::process::Command::new(env!("CARGO_BIN_EXE_agentisan"))
        .arg("--data-dir")
        .arg(&data)
        .args(["serve", "--listen", "127.0.0.1:0"])
        .kill_on_drop(true)
        .output()
        .await
        .unwrap();
    assert!(!result.status.success());
    assert!(result.stdout.is_empty());
    std::fs::remove_file(bad).unwrap();
    initialize(&data, &fixture).await.unwrap();
    let registry = Registry::open(&data.join("registry.sqlite3"))
        .await
        .unwrap();
    assert!(registry.is_initialized().await.unwrap());
    registry.close().await;
}
