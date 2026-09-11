use agentisan::{
    fixture,
    model::{AgentRole, Group, Team},
    registry::Registry,
    teams::{self, MemberConfig, Provider, TeamConfig},
};

const BIN: &str = env!("CARGO_BIN_EXE_agentisan");

#[tokio::test]
async fn top_level_run_accepts_inline_objective_and_queues_once() {
    let temp = tempfile::tempdir().unwrap();
    let data = temp.path().join("state");
    let registry = Registry::open(&fixture::database_path(&data).unwrap())
        .await
        .unwrap();
    teams::create(
        &registry,
        &data,
        &TeamConfig {
            group: Group {
                id: "g".into(),
                name: "Group".into(),
            },
            team: Team {
                id: "t".into(),
                group_id: "g".into(),
                name: "Team".into(),
            },
            agents: vec![MemberConfig {
                id: "lead".into(),
                name: "Lead".into(),
                role: AgentRole::Lead,
                provider: Provider::Claude,
                executable: std::env::current_exe().unwrap(),
                model: "test".into(),
                effort: "low".into(),
                instructions: String::new(),
            }],
        },
    )
    .await
    .unwrap();
    registry.close().await;

    let denied = std::process::Command::new(BIN)
        .arg("--data-dir")
        .arg(&data)
        .args(["run", "t", "--objective", "Plan from CLI"])
        .output()
        .unwrap();
    assert!(!denied.status.success());
    assert!(String::from_utf8_lossy(&denied.stderr).contains("--live"));

    let output = std::process::Command::new(BIN)
        .arg("--data-dir")
        .arg(&data)
        .args(["run", "t", "--objective", "Plan from CLI", "--live"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["state"], "queued");

    let registry = Registry::open(&fixture::database_path(&data).unwrap())
        .await
        .unwrap();
    let token = fixture::read_credential(&data.join("managed/t/lead.token")).unwrap();
    let inspected = teams::inspect_run(&registry, &token, value["run_id"].as_str().unwrap())
        .await
        .unwrap();
    assert_eq!(inspected["team_id"], "t");
    assert_eq!(inspected["pending_message_count"], 1);
    registry.close().await;
}
