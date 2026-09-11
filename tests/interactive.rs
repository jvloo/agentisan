use agentisan::{
    fixture,
    interactive::{
        self, CancelArgs, InitialWorkItem, StartArgs, StatusArgs, UpdateAction, UpdateArgs,
    },
    model::{AgentRole, Group, Team},
    registry::Registry,
    teams::{self, Action, MemberConfig, Provider, TeamConfig},
    worker::TurnResult,
};

async fn setup() -> (tempfile::TempDir, Registry, Vec<String>) {
    let temp = tempfile::tempdir().unwrap();
    let data = temp.path().join("state");
    let registry = Registry::open(&fixture::database_path(&data).unwrap())
        .await
        .unwrap();
    let config = TeamConfig {
        group: Group {
            id: "interactive".into(),
            name: "Interactive".into(),
        },
        team: Team {
            id: "interactive".into(),
            group_id: "interactive".into(),
            name: "Interactive".into(),
        },
        agents: ["lead", "a", "b", "c"]
            .iter()
            .map(|id| MemberConfig {
                id: (*id).into(),
                name: id.to_string(),
                role: if *id == "lead" {
                    AgentRole::Lead
                } else {
                    AgentRole::Worker
                },
                provider: Provider::Claude,
                executable: std::env::current_exe().unwrap(),
                model: "test".into(),
                effort: "low".into(),
                instructions: String::new(),
            })
            .collect(),
    };
    teams::create(&registry, &data, &config).await.unwrap();
    let tokens = ["lead", "a", "b", "c"]
        .iter()
        .map(|id| {
            fixture::read_credential(&data.join(format!("managed/interactive/{id}.token"))).unwrap()
        })
        .collect();
    (temp, registry, tokens)
}

fn start_args(key: &str) -> StartArgs {
    StartArgs {
        objective: "Have both workers review Phase 1 and exchange findings".into(),
        live: true,
        connector_instance: "connector_one".into(),
        workers: vec!["a".into(), "b".into()],
        initial_work: vec![
            InitialWorkItem {
                assignee: "a".into(),
                objective: "Review reliability".into(),
                done_criteria: vec!["Report evidence".into()],
            },
            InitialWorkItem {
                assignee: "b".into(),
                objective: "Review developer experience".into(),
                done_criteria: vec!["Report evidence".into()],
            },
        ],
        idempotency_key: key.into(),
        max_turns: Some(8),
        max_messages: Some(24),
        timeout_seconds: Some(120),
        turn_timeout_seconds: Some(20),
    }
}

fn success(id: &str) -> TurnResult {
    TurnResult {
        native_id: format!("native_{id}"),
        output: format!("finished {id}"),
        usage: serde_json::json!({"known":false}),
        artifacts: std::path::PathBuf::from("none"),
    }
}

async fn receive_send_commit(
    registry: &Registry,
    work: &teams::Work,
    sends: &[(&str, &str, &str)],
) {
    let token = fixture::read_credential(&work.credential_file).unwrap();
    teams::act(
        registry,
        &token,
        Action::Receive {
            run_id: work.run_id.clone(),
        },
    )
    .await
    .unwrap();
    for (to, body, key) in sends {
        teams::act(
            registry,
            &token,
            Action::Send {
                run_id: work.run_id.clone(),
                to: (*to).into(),
                body: (*body).into(),
                reply_to: None,
                idempotency_key: (*key).into(),
            },
        )
        .await
        .unwrap();
    }
    teams::act(
        registry,
        &token,
        Action::Commit {
            run_id: work.run_id.clone(),
            idempotency_key: format!("commit_{}", work.agent.id.as_str()),
        },
    )
    .await
    .unwrap();
    teams::finish(registry, work, Ok(success(work.agent.id.as_str())))
        .await
        .unwrap();
}

fn update_args(
    start: &serde_json::Value,
    version: i64,
    action: UpdateAction,
    key: &str,
) -> UpdateArgs {
    UpdateArgs {
        run_id: start["run_id"].as_str().unwrap().into(),
        control_handle: start["control_handle"].as_str().unwrap().into(),
        connector_instance: "connector_one".into(),
        expected_version: version,
        expected_epoch: start["coordination_epoch"].as_i64().unwrap(),
        idempotency_key: key.into(),
        action,
    }
}

#[tokio::test]
async fn main_chat_controls_workers_without_dispatching_the_managed_lead() {
    let (temp, registry, tokens) = setup().await;
    let started = interactive::start(&registry, &tokens[0], start_args("start_once"))
        .await
        .unwrap();
    let repeated = interactive::start(&registry, &tokens[0], start_args("start_once"))
        .await
        .unwrap();
    assert_eq!(repeated["run_id"], started["run_id"]);
    assert_eq!(repeated["control_handle"], started["control_handle"]);
    assert_eq!(repeated["duplicate"], true);
    let mut changed = start_args("start_once");
    changed.objective = "Changed objective".into();
    let changed = interactive::start(&registry, &tokens[0], changed)
        .await
        .unwrap_err();
    assert!(changed.to_string().contains("idempotency"));

    let first = teams::next(&registry).await.unwrap().unwrap();
    assert_eq!(first.agent.id.as_str(), "a");
    receive_send_commit(
        &registry,
        &first,
        &[
            ("b", "architecture draft", "a_to_b"),
            ("lead", "a report", "a_report"),
        ],
    )
    .await;

    let second = teams::next(&registry).await.unwrap().unwrap();
    assert_eq!(second.agent.id.as_str(), "b");
    receive_send_commit(
        &registry,
        &second,
        &[
            ("a", "validation reply", "b_to_a"),
            ("lead", "b report", "b_report"),
        ],
    )
    .await;

    let third = teams::next(&registry).await.unwrap().unwrap();
    assert_eq!(third.agent.id.as_str(), "a");
    receive_send_commit(&registry, &third, &[]).await;
    assert!(teams::next(&registry).await.unwrap().is_none());

    let database = temp.path().join("state/registry.sqlite3");
    let pool = sqlx::SqlitePool::connect(&format!("sqlite:{}", database.display()))
        .await
        .unwrap();
    let lead_turns: i64 =
        sqlx::query_scalar("SELECT count(*) FROM turns WHERE run_id=? AND agent_id='lead'")
            .bind(started["run_id"].as_str().unwrap())
            .fetch_one(&pool)
            .await
            .unwrap();
    let expires_before: i64 =
        sqlx::query_scalar("SELECT expires_at FROM controller_leases WHERE run_id=?")
            .bind(started["run_id"].as_str().unwrap())
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(lead_turns, 0);

    let status = interactive::status(
        &registry,
        &tokens[0],
        StatusArgs {
            run_id: started["run_id"].as_str().unwrap().into(),
            after_cursor: None,
            timeout_seconds: Some(0),
            message_id: None,
        },
    )
    .await
    .unwrap();
    let expires_after: i64 =
        sqlx::query_scalar("SELECT expires_at FROM controller_leases WHERE run_id=?")
            .bind(started["run_id"].as_str().unwrap())
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(
        expires_before, expires_after,
        "status must remain read-only"
    );
    assert_eq!(status["state"], "waiting_for_controller");
    assert!(
        status["messages"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m["from"] == "a" && m["to"] == "b")
    );
    assert!(
        status["messages"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m["from"] == "b" && m["to"] == "a")
    );
    let reports: Vec<String> = status["controller_inbox"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["id"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(reports.len(), 2);
    let detail = interactive::status(
        &registry,
        &tokens[0],
        StatusArgs {
            run_id: started["run_id"].as_str().unwrap().into(),
            after_cursor: Some(status["cursor"].as_str().unwrap().into()),
            timeout_seconds: Some(30),
            message_id: Some(reports[0].clone()),
        },
    )
    .await
    .unwrap();
    assert!(
        detail["message_detail"]["body"] == "a report"
            || detail["message_detail"]["body"] == "b report"
    );

    let invalid = UpdateArgs {
        control_handle: "not_the_handle".into(),
        ..update_args(
            &started,
            1,
            UpdateAction::AcceptReport {
                message_id: reports[0].clone(),
            },
            "invalid_handle",
        )
    };
    assert!(
        interactive::update(&registry, &tokens[0], invalid)
            .await
            .is_err()
    );

    let mut fenced = update_args(
        &started,
        1,
        UpdateAction::AcceptReport {
            message_id: reports[0].clone(),
        },
        "other_connector",
    );
    fenced.connector_instance = "connector_two".into();
    let fenced = interactive::update(&registry, &tokens[0], fenced)
        .await
        .unwrap_err();
    assert!(fenced.to_string().contains("another connector"));

    sqlx::query("UPDATE controller_leases SET expires_at=0 WHERE run_id=?")
        .bind(started["run_id"].as_str().unwrap())
        .execute(&pool)
        .await
        .unwrap();
    let mut accept_a = update_args(
        &started,
        1,
        UpdateAction::AcceptReport {
            message_id: reports[0].clone(),
        },
        "accept_a",
    );
    accept_a.connector_instance = "connector_two".into();
    let accepted_a = interactive::update(&registry, &tokens[0], accept_a.clone())
        .await
        .unwrap();
    assert_eq!(accepted_a["coordination_epoch"], 2);
    let replayed = interactive::update(&registry, &tokens[0], accept_a)
        .await
        .unwrap();
    assert_eq!(accepted_a, replayed);

    let stale_version = interactive::update(
        &registry,
        &tokens[0],
        update_args(
            &started,
            1,
            UpdateAction::AcceptReport {
                message_id: reports[1].clone(),
            },
            "stale_version",
        ),
    )
    .await
    .unwrap_err();
    assert!(stale_version.to_string().contains("stale run version"));

    let mut stale_epoch = update_args(
        &started,
        accepted_a["version"].as_i64().unwrap(),
        UpdateAction::AcceptReport {
            message_id: reports[1].clone(),
        },
        "stale_epoch",
    );
    stale_epoch.connector_instance = "connector_one".into();
    let stale_epoch = interactive::update(&registry, &tokens[0], stale_epoch)
        .await
        .unwrap_err();
    assert!(stale_epoch.to_string().contains("stale coordination epoch"));

    let mut accept_b = update_args(
        &started,
        accepted_a["version"].as_i64().unwrap(),
        UpdateAction::AcceptReport {
            message_id: reports[1].clone(),
        },
        "accept_b",
    );
    accept_b.connector_instance = "connector_two".into();
    accept_b.expected_epoch = 2;
    let accepted_b = interactive::update(&registry, &tokens[0], accept_b)
        .await
        .unwrap();
    let mut finish = update_args(
        &started,
        accepted_b["version"].as_i64().unwrap(),
        UpdateAction::Finish {
            result: "Both Claude workers reviewed each other and reported".into(),
        },
        "finish",
    );
    finish.connector_instance = "connector_two".into();
    finish.expected_epoch = 2;
    let finished = interactive::update(&registry, &tokens[0], finish)
        .await
        .unwrap();
    assert_eq!(finished["status"], "completed");
    pool.close().await;
    registry.close().await;
}

#[tokio::test]
async fn interactive_cancel_is_confirmed_without_an_active_native_turn() {
    let (_temp, registry, tokens) = setup().await;
    let started = interactive::start(&registry, &tokens[0], start_args("cancel_start"))
        .await
        .unwrap();
    let cancelled = interactive::cancel(
        &registry,
        &tokens[0],
        CancelArgs {
            run_id: started["run_id"].as_str().unwrap().into(),
            control_handle: started["control_handle"].as_str().unwrap().into(),
            connector_instance: "connector_one".into(),
            expected_version: 1,
            expected_epoch: 1,
            idempotency_key: "cancel".into(),
            reason: Some("scope changed".into()),
        },
    )
    .await
    .unwrap();
    assert_eq!(cancelled["outcome"], "confirmed");
    assert_eq!(cancelled["run_state"], "cancelled");
    assert!(teams::next(&registry).await.unwrap().is_none());
    registry.close().await;
}

#[tokio::test]
async fn active_turn_cancellation_remains_unconfirmed_and_fences_new_work() {
    let (_temp, registry, tokens) = setup().await;
    let started = interactive::start(&registry, &tokens[0], start_args("active_cancel_start"))
        .await
        .unwrap();
    let active = teams::next(&registry).await.unwrap().unwrap();
    let cancelled = interactive::cancel(
        &registry,
        &tokens[0],
        CancelArgs {
            run_id: started["run_id"].as_str().unwrap().into(),
            control_handle: started["control_handle"].as_str().unwrap().into(),
            connector_instance: "connector_one".into(),
            expected_version: 1,
            expected_epoch: 1,
            idempotency_key: "active_cancel".into(),
            reason: Some("stop now".into()),
        },
    )
    .await
    .unwrap();
    assert_eq!(cancelled["outcome"], "stop_requested");
    assert_eq!(cancelled["native_effect"], "unknown");
    assert!(teams::next(&registry).await.unwrap().is_none());
    teams::finish(&registry, &active, Ok(success("cancelled_active")))
        .await
        .unwrap();
    let status = interactive::status(
        &registry,
        &tokens[0],
        StatusArgs {
            run_id: started["run_id"].as_str().unwrap().into(),
            after_cursor: None,
            timeout_seconds: Some(0),
            message_id: None,
        },
    )
    .await
    .unwrap();
    assert_eq!(status["state"], "interrupted");
    registry.close().await;
}

#[tokio::test]
async fn interactive_workers_cannot_expand_the_engaged_roster() {
    let (_temp, registry, tokens) = setup().await;
    let started = interactive::start(&registry, &tokens[0], start_args("roster_start"))
        .await
        .unwrap();
    let work = teams::next(&registry).await.unwrap().unwrap();
    let token = fixture::read_credential(&work.credential_file).unwrap();
    teams::act(
        &registry,
        &token,
        Action::Receive {
            run_id: started["run_id"].as_str().unwrap().into(),
        },
    )
    .await
    .unwrap();
    let error = teams::act(
        &registry,
        &token,
        Action::Send {
            run_id: started["run_id"].as_str().unwrap().into(),
            to: "c".into(),
            body: "Join this run".into(),
            reply_to: None,
            idempotency_key: "expand_roster".into(),
        },
    )
    .await
    .unwrap_err();
    assert!(error.to_string().contains("not engaged"));
    registry.close().await;
}
