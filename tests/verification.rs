#![cfg(unix)]

use agentisan::{
    fixture,
    model::{AgentRole, Group, Team},
    registry::Registry,
    teams::{self, Action, MemberConfig, Provider, TeamConfig},
};
use sqlx::{Connection, SqliteConnection, sqlite::SqliteConnectOptions};
use std::{
    io::{BufRead, BufReader},
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    process::{Command, Stdio},
    time::{Duration, Instant},
};

async fn completed_proposal(data: &std::path::Path, result: &str) -> (String, String) {
    let registry = Registry::open(&fixture::database_path(data).unwrap())
        .await
        .unwrap();
    let config = TeamConfig {
        group: Group {
            id: "verify_group".into(),
            name: "Verification".into(),
        },
        team: Team {
            id: "verify_team".into(),
            group_id: "verify_group".into(),
            name: "Verification".into(),
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
    };
    teams::create(&registry, data, &config).await.unwrap();
    let token = fixture::read_credential(&data.join("managed/verify_team/lead.token")).unwrap();
    let run = teams::start(&registry, "verify_team", "Return proposal", 2, 2, 60, 10)
        .await
        .unwrap();
    let work = teams::next(&registry).await.unwrap().unwrap();
    teams::act(
        &registry,
        &token,
        Action::Receive {
            run_id: run.clone(),
        },
    )
    .await
    .unwrap();
    teams::act(
        &registry,
        &token,
        Action::Complete {
            run_id: run.clone(),
            result: result.into(),
        },
    )
    .await
    .unwrap();
    teams::finish(
        &registry,
        &work,
        Ok(agentisan::worker::TurnResult {
            native_id: uuid::Uuid::new_v4().to_string(),
            output: "Proposed".into(),
            usage: serde_json::Value::Null,
            artifacts: PathBuf::new(),
        }),
    )
    .await
    .unwrap();
    registry.close().await;
    (run, token)
}

fn verifier(path: &std::path::Path, body: &str) {
    std::fs::write(path, body).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
}

#[tokio::test]
async fn deterministic_verifier_accepts_exact_result_and_is_terminal() {
    let temp = tempfile::tempdir().unwrap();
    let data = temp.path().join("state");
    let (run, token) = completed_proposal(&data, "exact proposal bytes").await;
    let check = temp.path().join("accept.sh");
    verifier(
        &check,
        "#!/bin/sh\ninput=$(cat)\n[ \"$input\" = \"exact proposal bytes\" ] || exit 2\nprintf '%s\\n' '{\"accepted\":true,\"summary\":\"checks passed\",\"evidence\":{\"cases\":12}}'\n",
    );
    let output = Command::new(env!("CARGO_BIN_EXE_agentisan"))
        .args([
            "--data-dir",
            data.to_str().unwrap(),
            "runs",
            "verify",
            &run,
            "--verifier",
            check.to_str().unwrap(),
            "--timeout-seconds",
            "5",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let registry = Registry::open(&data.join("registry.sqlite3"))
        .await
        .unwrap();
    let inspected = teams::inspect_run(&registry, &token, &run).await.unwrap();
    assert_eq!(inspected["state"], "completed");
    assert_eq!(inspected["acceptance"], "accepted");
    assert_eq!(inspected["verification"]["summary"], "checks passed");
    registry.close().await;
    let repeated = Command::new(env!("CARGO_BIN_EXE_agentisan"))
        .args([
            "--data-dir",
            data.to_str().unwrap(),
            "runs",
            "verify",
            &run,
            "--verifier",
            check.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(!repeated.status.success());

    let mut service = Command::new(env!("CARGO_BIN_EXE_agentisan"))
        .args([
            "--data-dir",
            data.to_str().unwrap(),
            "serve",
            "--listen",
            "127.0.0.1:0",
        ])
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut ready = String::new();
    BufReader::new(service.stdout.take().unwrap())
        .read_line(&mut ready)
        .unwrap();
    let address = serde_json::from_str::<serde_json::Value>(&ready).unwrap()["address"]
        .as_str()
        .unwrap()
        .to_owned();
    let endpoint = format!("http://{address}");
    let watched = Command::new(env!("CARGO_BIN_EXE_agentisan"))
        .args([
            "--endpoint",
            &endpoint,
            "--credential-file",
            data.join("managed/verify_team/lead.token")
                .to_str()
                .unwrap(),
            "runs",
            "watch",
            &run,
            "--timeout-seconds",
            "2",
        ])
        .output()
        .unwrap();
    service.kill().unwrap();
    service.wait().unwrap();
    assert!(watched.status.success());
    let snapshots: Vec<_> = String::from_utf8(watched.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect();
    assert_eq!(snapshots.len(), 1);
    assert_eq!(snapshots[0]["activity"]["state"], "idle");
    assert_eq!(snapshots[0]["acceptance"], "accepted");
}

#[tokio::test]
async fn malformed_verifier_is_recorded_and_a_later_rejection_is_allowed() {
    let temp = tempfile::tempdir().unwrap();
    let data = temp.path().join("state");
    let (run, token) = completed_proposal(&data, "candidate").await;
    let malformed = temp.path().join("malformed.sh");
    verifier(&malformed, "#!/bin/sh\ncat >/dev/null\nprintf 'not json'\n");
    let failed = Command::new(env!("CARGO_BIN_EXE_agentisan"))
        .args([
            "--data-dir",
            data.to_str().unwrap(),
            "runs",
            "verify",
            &run,
            "--verifier",
            malformed.to_str().unwrap(),
            "--timeout-seconds",
            "5",
        ])
        .output()
        .unwrap();
    assert!(!failed.status.success());
    let registry = Registry::open(&data.join("registry.sqlite3"))
        .await
        .unwrap();
    let after_error = teams::inspect_run(&registry, &token, &run).await.unwrap();
    assert_eq!(after_error["acceptance"], "not_independently_verified");
    assert_eq!(after_error["verification"]["state"], "error");
    registry.close().await;

    let reject = temp.path().join("reject.sh");
    verifier(
        &reject,
        "#!/bin/sh\ncat >/dev/null\nprintf '%s\\n' '{\"accepted\":false,\"summary\":\"case mismatch\",\"evidence\":{\"case\":6}}'\n",
    );
    let rejected = Command::new(env!("CARGO_BIN_EXE_agentisan"))
        .args([
            "--data-dir",
            data.to_str().unwrap(),
            "runs",
            "verify",
            &run,
            "--verifier",
            reject.to_str().unwrap(),
            "--timeout-seconds",
            "5",
        ])
        .output()
        .unwrap();
    assert!(
        rejected.status.success(),
        "{}",
        String::from_utf8_lossy(&rejected.stderr)
    );
    let registry = Registry::open(&data.join("registry.sqlite3"))
        .await
        .unwrap();
    let inspected = teams::inspect_run(&registry, &token, &run).await.unwrap();
    assert_eq!(inspected["acceptance"], "rejected");
    assert_eq!(inspected["verification"]["summary"], "case mismatch");
    registry.close().await;
}

#[tokio::test]
async fn verifier_deadline_records_error_without_acceptance() {
    let temp = tempfile::tempdir().unwrap();
    let data = temp.path().join("state");
    let (run, token) = completed_proposal(&data, "candidate").await;
    let check = temp.path().join("slow.sh");
    verifier(
        &check,
        "#!/bin/sh\ncat >/dev/null\nsleep 10\nprintf '%s\\n' '{\"accepted\":true,\"summary\":\"too late\"}'\n",
    );
    let started = Instant::now();
    let output = Command::new(env!("CARGO_BIN_EXE_agentisan"))
        .args([
            "--data-dir",
            data.to_str().unwrap(),
            "runs",
            "verify",
            &run,
            "--verifier",
            check.to_str().unwrap(),
            "--timeout-seconds",
            "1",
        ])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(started.elapsed() < Duration::from_secs(4));
    let registry = Registry::open(&data.join("registry.sqlite3"))
        .await
        .unwrap();
    let inspected = teams::inspect_run(&registry, &token, &run).await.unwrap();
    assert_eq!(inspected["acceptance"], "not_independently_verified");
    assert_eq!(inspected["verification"]["state"], "error");
    assert!(
        inspected["verification"]["summary"]
            .as_str()
            .unwrap()
            .contains("deadline")
    );
    registry.close().await;
}

#[tokio::test]
async fn later_cli_verification_fences_a_crashed_reservation() {
    let temp = tempfile::tempdir().unwrap();
    let data = temp.path().join("state");
    let (run, token) = completed_proposal(&data, "candidate").await;
    let database = data.join("registry.sqlite3");
    let options = SqliteConnectOptions::new()
        .filename(&database)
        .foreign_keys(true);
    let mut connection = SqliteConnection::connect_with(&options).await.unwrap();
    sqlx::query("INSERT INTO verifications(id,run_id,status,verifier_path,verifier_sha256,result_sha256,started_at,artifacts) VALUES(?,?,'running',?,?,?,?,?)")
        .bind("verification_crashed")
        .bind(&run)
        .bind("/missing/verifier")
        .bind("0".repeat(64))
        .bind("1".repeat(64))
        .bind(1_i64)
        .bind(data.join("runs/crashed").to_string_lossy().as_ref())
        .execute(&mut connection)
        .await
        .unwrap();
    connection.close().await.unwrap();

    let reject = temp.path().join("reject.sh");
    verifier(
        &reject,
        "#!/bin/sh\ncat >/dev/null\nprintf '%s\\n' '{\"accepted\":false,\"summary\":\"rechecked\"}'\n",
    );
    let output = Command::new(env!("CARGO_BIN_EXE_agentisan"))
        .args([
            "--data-dir",
            data.to_str().unwrap(),
            "runs",
            "verify",
            &run,
            "--verifier",
            reject.to_str().unwrap(),
            "--timeout-seconds",
            "5",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let registry = Registry::open(&database).await.unwrap();
    let inspected = teams::inspect_run(&registry, &token, &run).await.unwrap();
    assert_eq!(inspected["verification"]["state"], "rejected");
    registry.close().await;
    let mut connection = SqliteConnection::connect_with(&options).await.unwrap();
    let old: String =
        sqlx::query_scalar("SELECT status FROM verifications WHERE id='verification_crashed'")
            .fetch_one(&mut connection)
            .await
            .unwrap();
    assert_eq!(old, "error");
    connection.close().await.unwrap();
}
