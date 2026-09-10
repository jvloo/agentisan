use agentisan::{
    fixture,
    model::*,
    registry::Registry,
    teams::{self, Action, MemberConfig, Provider, TeamConfig},
};
use serde_json::Value;

async fn setup() -> (tempfile::TempDir, Registry, Vec<String>) {
    let temp = tempfile::tempdir().unwrap();
    let data = temp.path().join("state");
    let registry = Registry::open(&fixture::database_path(&data).unwrap())
        .await
        .unwrap();
    let config = TeamConfig {
        group: Group {
            id: "group".into(),
            name: "Group".into(),
        },
        team: Team {
            id: "team".into(),
            group_id: "group".into(),
            name: "Team".into(),
        },
        agents: ["lead", "a", "b"]
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
                model: "test-model".into(),
                effort: "low".into(),
                instructions: String::new(),
            })
            .collect(),
    };
    teams::create(&registry, &data, &config).await.unwrap();
    let tokens = ["lead", "a", "b"]
        .iter()
        .map(|id| fixture::read_credential(&data.join(format!("managed/team/{id}.token"))).unwrap())
        .collect();
    (temp, registry, tokens)
}
async fn send(registry: &Registry, token: &str, run: &str, to: &str, key: &str) -> Value {
    teams::act(
        registry,
        token,
        Action::Send {
            run_id: run.into(),
            to: to.into(),
            body: format!("Message {key}"),
            reply_to: None,
            idempotency_key: key.into(),
        },
    )
    .await
    .unwrap()
}

#[tokio::test]
async fn message_identity_deduplication_delivery_and_peer_scope_are_persistent() {
    let (_temp, r, t) = setup().await;
    let run = teams::start(&r, "team", "Assign independent tasks", 12, 12, 120, 20)
        .await
        .unwrap();
    let lead = teams::next(&r).await.unwrap().unwrap();
    assert_eq!(lead.agent.id.as_str(), "lead");
    assert!(
        teams::act(
            &r,
            &t[1],
            Action::Receive {
                run_id: run.clone()
            }
        )
        .await
        .is_err()
    );
    let inbox = teams::act(
        &r,
        &t[0],
        Action::Receive {
            run_id: run.clone(),
        },
    )
    .await
    .unwrap();
    assert_eq!(inbox["messages"][0]["from"], "human");
    let first = send(&r, &t[0], &run, "a", "assign_a").await;
    let duplicate = send(&r, &t[0], &run, "a", "assign_a").await;
    assert_eq!(first["message_id"], duplicate["message_id"]);
    assert_eq!(duplicate["duplicate"], true);
    assert!(
        teams::act(
            &r,
            &t[0],
            Action::Send {
                run_id: run.clone(),
                to: "b".into(),
                body: "changed".into(),
                reply_to: None,
                idempotency_key: "assign_a".into()
            }
        )
        .await
        .is_err()
    );
    send(&r, &t[0], &run, "b", "assign_b").await;
    assert!(
        teams::act(
            &r,
            &t[0],
            Action::Complete {
                run_id: run.clone(),
                result: "too soon".into()
            }
        )
        .await
        .is_err()
    );
    // Finish a simulated native turn; no provider process is invoked in this test.
    teams::finish(
        &r,
        &lead,
        Ok(agentisan::worker::TurnResult {
            native_id: uuid::Uuid::new_v4().to_string(),
            output: "Assigned".into(),
            usage: serde_json::json!(null),
            artifacts: std::path::PathBuf::new(),
        }),
    )
    .await
    .unwrap();
    let a = teams::next(&r).await.unwrap().unwrap();
    assert_eq!(a.agent.id.as_str(), "a");
    teams::act(
        &r,
        &t[1],
        Action::Receive {
            run_id: run.clone(),
        },
    )
    .await
    .unwrap();
    send(&r, &t[1], &run, "b", "peer").await;
    assert!(
        teams::act(
            &r,
            &t[1],
            Action::Complete {
                run_id: run.clone(),
                result: "worker cannot finish run".into()
            }
        )
        .await
        .is_err()
    );
    let history = teams::messages(&r, &t[0], &run).await.unwrap();
    let list = history["messages"].as_array().unwrap();
    assert_eq!(list.len(), 4);
    assert_eq!(list[3]["from"], "a");
    assert_eq!(list[3]["to"], "b");
    assert!(teams::messages(&r, "invalid", &run).await.is_err());
    r.close().await;
}

#[tokio::test]
async fn run_limits_and_interrupted_turns_stop_without_replay() {
    let (_temp, r, t) = setup().await;
    let run = teams::start(&r, "team", "Work", 1, 2, 120, 20)
        .await
        .unwrap();
    assert!(
        teams::start(&r, "team", "Second owner", 4, 4, 120, 20)
            .await
            .is_err()
    );
    let work = teams::next(&r).await.unwrap().unwrap();
    teams::act(
        &r,
        &t[0],
        Action::Receive {
            run_id: run.clone(),
        },
    )
    .await
    .unwrap();
    send(&r, &t[0], &run, "a", "only_message").await;
    assert!(
        teams::act(
            &r,
            &t[0],
            Action::Send {
                run_id: run.clone(),
                to: "b".into(),
                body: "over budget".into(),
                reply_to: None,
                idempotency_key: "excess".into()
            }
        )
        .await
        .is_err()
    );
    teams::finish(
        &r,
        &work,
        Ok(agentisan::worker::TurnResult {
            native_id: uuid::Uuid::new_v4().to_string(),
            output: "Assigned".into(),
            usage: serde_json::json!(null),
            artifacts: std::path::PathBuf::new(),
        }),
    )
    .await
    .unwrap();
    assert!(teams::next(&r).await.unwrap().is_none());
    assert_eq!(
        teams::inspect_run(&r, &t[0], &run).await.unwrap()["state"],
        "exhausted"
    );
    let next = teams::start(&r, "team", "New explicit objective", 4, 8, 120, 20)
        .await
        .unwrap();
    teams::next(&r).await.unwrap().unwrap();
    teams::recover_interrupted(&r).await.unwrap();
    let state = teams::inspect_run(&r, &t[0], &next).await.unwrap();
    assert_eq!(state["state"], "interrupted");
    assert_eq!(state["turns"][0]["state"], "unknown");
    assert!(teams::next(&r).await.unwrap().is_none());
    r.close().await;
}

#[tokio::test]
async fn native_bindings_cannot_silently_change_on_resume() {
    let (_temp, r, t) = setup().await;
    let id = uuid::Uuid::new_v4().to_string();
    let run = teams::start(&r, "team", "Work", 4, 8, 120, 20)
        .await
        .unwrap();
    teams::next(&r).await.unwrap().unwrap();
    teams::record_binding(&r, &run, "lead", &id).await.unwrap();
    teams::record_binding(&r, &run, "lead", &id).await.unwrap();
    assert!(
        teams::record_binding(&r, &run, "lead", &uuid::Uuid::new_v4().to_string())
            .await
            .is_err()
    );
    let inspected = r
        .inspect(
            Some(&t[0]),
            Query::AgentsInspect {
                agent_id: "lead".into(),
            },
        )
        .await
        .unwrap();
    assert_eq!(inspected["source"], "managed_cli");
    assert_eq!(inspected["agent"]["native_binding"]["session_id"], id);
    r.close().await;
}

#[tokio::test]
async fn inspected_resume_preserves_native_ids_and_original_limits() {
    let (_temp, r, t) = setup().await;
    let run = teams::start(&r, "team", "Review", 4, 8, 120, 20)
        .await
        .unwrap();
    let work = teams::next(&r).await.unwrap().unwrap();
    let native = uuid::Uuid::new_v4().to_string();
    teams::record_binding(&r, &run, "lead", &native)
        .await
        .unwrap();
    // Successful but unproductive native turn: the message was never received.
    teams::finish(
        &r,
        &work,
        Ok(agentisan::worker::TurnResult {
            native_id: native.clone(),
            output: "Tool unavailable".into(),
            usage: serde_json::json!(null),
            artifacts: std::path::PathBuf::new(),
        }),
    )
    .await
    .unwrap();
    let before = teams::inspect_run(&r, &t[0], &run).await.unwrap();
    assert_eq!(before["state"], "stalled");
    teams::resume(&r, &run).await.unwrap();
    let after = teams::inspect_run(&r, &t[0], &run).await.unwrap();
    assert_eq!(before["turn_count"], after["turn_count"]);
    assert_eq!(before["deadline"], after["deadline"]);
    let resumed = teams::next(&r).await.unwrap().unwrap();
    assert_eq!(resumed.native_id.as_deref(), Some(native.as_str()));
    teams::finish(
        &r,
        &resumed,
        Err(anyhow::anyhow!("uncertain native failure")),
    )
    .await
    .unwrap();
    assert!(teams::resume(&r, &run).await.is_err());
    r.close().await;
}

#[tokio::test]
async fn concurrent_sends_cannot_spend_the_same_message_allowance() {
    let (temp, r, t) = setup().await;
    let other = Registry::open(&temp.path().join("state/registry.sqlite3"))
        .await
        .unwrap();
    let run = teams::start(&r, "team", "Work", 4, 2, 120, 20)
        .await
        .unwrap();
    teams::next(&r).await.unwrap().unwrap();
    teams::act(
        &r,
        &t[0],
        Action::Receive {
            run_id: run.clone(),
        },
    )
    .await
    .unwrap();
    let one = Action::Send {
        run_id: run.clone(),
        to: "a".into(),
        body: "First concurrent send".into(),
        reply_to: None,
        idempotency_key: "one".into(),
    };
    let two = Action::Send {
        run_id: run.clone(),
        to: "b".into(),
        body: "Second concurrent send".into(),
        reply_to: None,
        idempotency_key: "two".into(),
    };
    let (a, b) = tokio::join!(teams::act(&r, &t[0], one), teams::act(&other, &t[0], two));
    assert_eq!(usize::from(a.is_ok()) + usize::from(b.is_ok()), 1);
    assert_eq!(
        teams::messages(&r, &t[0], &run).await.unwrap()["messages"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    r.close().await;
    other.close().await;
}

#[tokio::test]
async fn completion_requires_worker_reports_and_recipients_must_be_managed() {
    let (_temp, r, t) = setup().await;
    let ghost = Fixture {
        schema_version: 1,
        groups: vec![Group {
            id: "group".into(),
            name: "Group".into(),
        }],
        teams: vec![Team {
            id: "team".into(),
            group_id: "group".into(),
            name: "Team".into(),
        }],
        agents: vec![Agent {
            id: "ghost".into(),
            team_id: "team".into(),
            name: "Unmanaged fixture".into(),
            role: AgentRole::Worker,
            parent_agent_id: None,
            native_binding: NativeBinding {
                adapter: "fake".into(),
                host_id: "test".into(),
                namespace: "fixture".into(),
                session_id: Some("ghost-native".into()),
                thread_id: None,
                subagent_id: None,
            },
        }],
        principals: vec![],
    };
    r.register_fixture(&ghost, &[]).await.unwrap();
    let run = teams::start(&r, "team", "Work", 4, 8, 120, 20)
        .await
        .unwrap();
    teams::next(&r).await.unwrap().unwrap();
    teams::act(
        &r,
        &t[0],
        Action::Receive {
            run_id: run.clone(),
        },
    )
    .await
    .unwrap();
    assert!(
        teams::act(
            &r,
            &t[0],
            Action::Complete {
                run_id: run.clone(),
                result: "No reports yet".into()
            }
        )
        .await
        .is_err()
    );
    assert!(
        teams::act(
            &r,
            &t[0],
            Action::Send {
                run_id: run.clone(),
                to: "ghost".into(),
                body: "No runnable member".into(),
                reply_to: None,
                idempotency_key: "ghost".into()
            }
        )
        .await
        .is_err()
    );
    r.close().await;
}

#[tokio::test]
async fn resuming_an_older_run_never_borrows_a_newer_runs_native_session() {
    let (_temp, r, _tokens) = setup().await;
    let mut history = Vec::new();
    for _ in 0..2 {
        let run = teams::start(&r, "team", "Work", 4, 8, 120, 20)
            .await
            .unwrap();
        let work = teams::next(&r).await.unwrap().unwrap();
        assert!(work.native_id.is_none());
        let native = uuid::Uuid::new_v4().to_string();
        teams::record_binding(&r, &run, "lead", &native)
            .await
            .unwrap();
        teams::finish(
            &r,
            &work,
            Ok(agentisan::worker::TurnResult {
                native_id: native.clone(),
                output: "Awaiting restored tool access".into(),
                usage: serde_json::json!(null),
                artifacts: std::path::PathBuf::new(),
            }),
        )
        .await
        .unwrap();
        history.push((run, native));
    }
    teams::resume(&r, &history[0].0).await.unwrap();
    let resumed = teams::next(&r).await.unwrap().unwrap();
    assert_eq!(resumed.native_id.as_deref(), Some(history[0].1.as_str()));
    assert!(
        teams::record_binding(&r, &history[0].0, "lead", &history[1].1)
            .await
            .is_err()
    );
    teams::record_binding(&r, &history[0].0, "lead", &history[0].1)
        .await
        .unwrap();
    r.close().await;
}
