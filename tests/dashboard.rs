use agentisan::{
    dashboard::{self, OpenTarget},
    fixture,
    model::{AgentRole, Group, Team},
    registry::Registry,
    teams::{self, MemberConfig, Provider, TeamConfig},
};

#[test]
fn no_subcommand_uses_dashboard_and_fails_cleanly_without_a_terminal() {
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_agentisan"))
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("dashboard requires an interactive terminal")
    );
}

#[tokio::test]
async fn native_open_refuses_a_run_still_owned_by_agentisan() {
    let temp = tempfile::tempdir().unwrap();
    let data = temp.path().join("state");
    let registry = Registry::open(&fixture::database_path(&data).unwrap())
        .await
        .unwrap();
    let config = TeamConfig {
        group: Group {
            id: "dashboard_group".into(),
            name: "Dashboard".into(),
        },
        team: Team {
            id: "dashboard_team".into(),
            group_id: "dashboard_group".into(),
            name: "Dashboard team".into(),
        },
        agents: ["lead", "worker"]
            .iter()
            .map(|id| MemberConfig {
                id: (*id).into(),
                name: (*id).into(),
                role: if *id == "lead" {
                    AgentRole::Lead
                } else {
                    AgentRole::Worker
                },
                provider: Provider::Codex,
                executable: std::env::current_exe().unwrap(),
                model: "test".into(),
                effort: "low".into(),
                instructions: String::new(),
            })
            .collect(),
    };
    teams::create(&registry, &data, &config).await.unwrap();
    let run = teams::start(&registry, "dashboard_team", "Remain owned", 4, 8, 60, 10)
        .await
        .unwrap();
    let error = dashboard::open_native(&data, &run, "lead", OpenTarget::Cli)
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("is queued"), "{error}");
    registry.close().await;
}

#[tokio::test]
async fn read_only_registry_open_never_migrates_an_older_schema() {
    let temp = tempfile::tempdir().unwrap();
    let database = temp.path().join("registry.sqlite3");
    Registry::open(&database).await.unwrap().close().await;
    let pool = sqlx::SqlitePool::connect(&format!("sqlite://{}", database.display()))
        .await
        .unwrap();
    sqlx::raw_sql("ALTER TABLE turn_leases DROP COLUMN input_snapshot; PRAGMA user_version=6;")
        .execute(&pool)
        .await
        .unwrap();
    pool.close().await;

    let error = Registry::open_read_only(&database)
        .await
        .err()
        .expect("older schema must require explicit migration")
        .to_string();
    assert!(error.contains("schema version"), "{error}");
    let check = sqlx::SqlitePool::connect(&format!("sqlite://{}", database.display()))
        .await
        .unwrap();
    let version: i64 = sqlx::query_scalar("PRAGMA user_version")
        .fetch_one(&check)
        .await
        .unwrap();
    let snapshots: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM pragma_table_info('turn_leases') WHERE name='input_snapshot'",
    )
    .fetch_one(&check)
    .await
    .unwrap();
    assert_eq!(version, 6);
    assert_eq!(snapshots, 0);
    check.close().await;
}
