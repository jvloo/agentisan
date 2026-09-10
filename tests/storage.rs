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
