#![cfg(unix)]
use agentisan::{
    fixture,
    model::*,
    registry::{Registry, credential_hash},
    teams::{MemberConfig, Provider, Work, now},
    worker,
};
use std::{
    os::unix::fs::PermissionsExt,
    time::{Duration, Instant},
};

#[tokio::test]
async fn blocked_stdin_is_inside_the_turn_deadline_and_owned_children_are_stopped() {
    let temp = tempfile::tempdir().unwrap();
    let data = temp.path().join("state");
    let registry = Registry::open(&fixture::database_path(&data).unwrap())
        .await
        .unwrap();
    let fx: Fixture = serde_json::from_str(include_str!("../examples/registry.json")).unwrap();
    let creds: Vec<_> = fx
        .principals
        .iter()
        .map(|p| CredentialHash {
            principal_id: p.id.clone(),
            sha256: credential_hash(p.id.as_str()),
        })
        .collect();
    registry.register_fixture(&fx, &creds).await.unwrap();
    let script = temp.path().join("mock-cli.sh");
    std::fs::write(&script,r#"#!/bin/sh
if [ "$1" = "--version" ]; then echo 'mock-cli (test fixture)'; exit 0; fi
if [ "$1" = "auth" ]; then echo '{"loggedIn":true,"authMethod":"claude.ai","apiProvider":"firstParty"}'; exit 0; fi
sleep 60 &
printf '%s' "$!" > "$0.child"
wait
"#).unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
    let work = Work {
        run_id: "run_limit_test".into(),
        turn_id: "turn_limit_test".into(),
        agent: MemberConfig {
            id: "inventory_lead".into(),
            name: "Lead".into(),
            role: AgentRole::Lead,
            provider: Provider::Claude,
            executable: script.clone(),
            model: "mock-model".into(),
            effort: "low".into(),
            instructions: "x".repeat(524_288),
        },
        credential_file: temp.path().join("unused-token"),
        native_id: None,
        deadline: now() + 30,
        turn_timeout: 2,
        team_id: "csv_export".into(),
    };
    let start = Instant::now();
    let result = tokio::time::timeout(
        Duration::from_secs(6),
        worker::run_with_host(
            registry.clone(),
            work,
            &data,
            "http://127.0.0.1:1",
            std::path::Path::new(env!("CARGO_BIN_EXE_agentisan")),
        ),
    )
    .await
    .expect("supervisor must return within its own deadline");
    let error = result.err().unwrap().to_string();
    assert!(error.contains("stdin transfer"), "{error}");
    assert!(start.elapsed() < Duration::from_secs(5));
    let child: i32 = std::fs::read_to_string(format!("{}.child", script.display()))
        .unwrap()
        .parse()
        .unwrap();
    // macOS/Linux may briefly keep a killed child as a zombie until reaped.
    let mut stopped = false;
    for _ in 0..30 {
        if matches!(
            nix::sys::signal::kill(nix::unistd::Pid::from_raw(child), None),
            Err(nix::errno::Errno::ESRCH)
        ) {
            stopped = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(
        stopped,
        "owned child must not remain running after deadline"
    );
    registry.close().await;
}

async fn mock_work(
    script_body: &str,
    timeout: u64,
) -> (tempfile::TempDir, Registry, Work, std::path::PathBuf) {
    let temp = tempfile::tempdir().unwrap();
    let data = temp.path().join("state");
    let registry = Registry::open(&fixture::database_path(&data).unwrap())
        .await
        .unwrap();
    let fx: Fixture = serde_json::from_str(include_str!("../examples/registry.json")).unwrap();
    let hashes: Vec<_> = fx
        .principals
        .iter()
        .map(|p| CredentialHash {
            principal_id: p.id.clone(),
            sha256: credential_hash(p.id.as_str()),
        })
        .collect();
    registry.register_fixture(&fx, &hashes).await.unwrap();
    let script = temp.path().join("mock.sh");
    std::fs::write(&script, script_body).unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
    let work = Work {
        run_id: "run_mock".into(),
        turn_id: "turn_mock".into(),
        agent: MemberConfig {
            id: "inventory_lead".into(),
            name: "Lead".into(),
            role: AgentRole::Lead,
            provider: Provider::Claude,
            executable: script,
            model: "mock".into(),
            effort: "low".into(),
            instructions: "Test".into(),
        },
        credential_file: temp.path().join("unused"),
        native_id: None,
        deadline: now() + 30,
        turn_timeout: timeout,
        team_id: "csv_export".into(),
    };
    (temp, registry, work, data)
}

#[tokio::test]
async fn slow_preflight_obeys_the_original_turn_deadline() {
    let (_temp, registry, work, data) = mock_work("#!/bin/sh\nexec sleep 60\n", 1).await;
    let start = Instant::now();
    let result = tokio::time::timeout(
        Duration::from_secs(4),
        worker::run_with_host(
            registry.clone(),
            work,
            &data,
            "http://127.0.0.1:1",
            std::path::Path::new(env!("CARGO_BIN_EXE_agentisan")),
        ),
    )
    .await
    .expect("preflight must be bounded by the turn deadline");
    assert!(
        result
            .err()
            .unwrap()
            .to_string()
            .contains("preflight timed out")
    );
    assert!(start.elapsed() < Duration::from_secs(3));
    registry.close().await;
}

#[tokio::test]
async fn a_flooding_native_cli_cannot_write_more_than_the_output_cap() {
    let script = r#"#!/bin/sh
if [ "$1" = "--version" ]; then echo mock; exit 0; fi
if [ "$1" = "auth" ]; then echo '{"loggedIn":true,"authMethod":"claude.ai","apiProvider":"firstParty"}'; exit 0; fi
dd if=/dev/zero bs=1048576 count=20 2>/dev/null
"#;
    let (_temp, registry, work, data) = mock_work(script, 5).await;
    let result = worker::run_with_host(
        registry.clone(),
        work,
        &data,
        "http://127.0.0.1:1",
        std::path::Path::new(env!("CARGO_BIN_EXE_agentisan")),
    )
    .await;
    assert!(
        result
            .err()
            .unwrap()
            .to_string()
            .contains("output limit exceeded")
    );
    let logs = data.join("runs/run_mock/inventory_lead/turn_mock");
    let bytes = std::fs::metadata(logs.join("stdout.jsonl")).unwrap().len()
        + std::fs::metadata(logs.join("stderr.txt")).unwrap().len();
    assert!(bytes <= 8_388_608);
    assert!(logs.join("output-truncated.json").is_file());
    registry.close().await;
}
