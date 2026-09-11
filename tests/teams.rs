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

async fn commit(registry: &Registry, work: &teams::Work) -> Value {
    teams::act(
        registry,
        &fixture::read_credential(&work.credential_file).unwrap(),
        Action::Commit {
            run_id: work.run_id.clone(),
            idempotency_key: "commit".into(),
        },
    )
    .await
    .unwrap()
}

#[tokio::test]
async fn message_identity_deduplication_delivery_and_peer_scope_are_persistent() {
    let (_temp, r, mut t) = setup().await;
    let run = teams::start(&r, "team", "Assign independent tasks", 12, 12, 120, 20)
        .await
        .unwrap();
    let lead = teams::next(&r).await.unwrap().unwrap();
    t[0] = fixture::read_credential(&lead.credential_file).unwrap();
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
    commit(&r, &lead).await;
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
    t[1] = fixture::read_credential(&a.credential_file).unwrap();
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
    commit(&r, &a).await;
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
    let (_temp, r, mut t) = setup().await;
    let run = teams::start(&r, "team", "Work", 1, 2, 120, 20)
        .await
        .unwrap();
    assert!(
        teams::start(&r, "team", "Second owner", 4, 4, 120, 20)
            .await
            .is_err()
    );
    let work = teams::next(&r).await.unwrap().unwrap();
    t[0] = fixture::read_credential(&work.credential_file).unwrap();
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
    commit(&r, &work).await;
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
    let queued = r
        .inspect(
            Some(&t[0]),
            Query::AgentsInspect {
                agent_id: "lead".into(),
            },
        )
        .await
        .unwrap();
    assert_eq!(queued["activity"]["state"], "waiting");
    assert_eq!(queued["activity"]["run_state"], "queued");
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
    assert_eq!(inspected["activity"]["state"], "running");
    assert_eq!(inspected["activity"]["source"], "agentisan_runtime");
    assert_eq!(
        inspected["activity"]["native_client_status"],
        "advisory_while_agentisan_owns_the_run"
    );
    let run_state = teams::inspect_run(&r, &t[0], &run).await.unwrap();
    assert_eq!(run_state["activity"]["state"], "running");
    assert_eq!(run_state["activity"]["active_agent_id"], "lead");
    assert_eq!(run_state["message_count"], 1);
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
    let (temp, r, mut t) = setup().await;
    let other = Registry::open(&temp.path().join("state/registry.sqlite3"))
        .await
        .unwrap();
    let run = teams::start(&r, "team", "Work", 4, 2, 120, 20)
        .await
        .unwrap();
    let work = teams::next(&r).await.unwrap().unwrap();
    t[0] = fixture::read_credential(&work.credential_file).unwrap();
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
    commit(&r, &work).await;
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
    let (_temp, r, mut t) = setup().await;
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
    let work = teams::next(&r).await.unwrap().unwrap();
    t[0] = fixture::read_credential(&work.credential_file).unwrap();
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

fn native_success() -> agentisan::worker::TurnResult {
    agentisan::worker::TurnResult {
        native_id: uuid::Uuid::new_v4().to_string(),
        output: "Finished native invocation".into(),
        usage: serde_json::json!(null),
        artifacts: std::path::PathBuf::new(),
    }
}

#[tokio::test]
async fn turn_lease_identity_is_scoped_below_observer_access() {
    let (_temp, registry, _tokens) = setup().await;
    let run = teams::start(&registry, "team", "Scoped work", 4, 8, 120, 20)
        .await
        .unwrap();
    let work = teams::next(&registry).await.unwrap().unwrap();
    let lease = fixture::read_credential(&work.credential_file).unwrap();
    let identity = registry
        .inspect(Some(&lease), Query::Whoami {})
        .await
        .unwrap();
    assert_eq!(identity["evidence"], "turn_lease");
    assert_eq!(identity["lease"]["run_id"], run);
    assert_eq!(identity["lease"]["turn_id"], work.turn_id);
    assert!(
        registry
            .inspect(
                Some(&lease),
                Query::AgentsInspect {
                    agent_id: "lead".into()
                }
            )
            .await
            .is_ok()
    );
    for query in [
        Query::GroupsList {},
        Query::AgentsInspect {
            agent_id: "a".into(),
        },
        Query::RunsInspect {
            run_id: run.clone(),
        },
        Query::MessagesList {
            run_id: run.clone(),
        },
    ] {
        assert!(registry.inspect(Some(&lease), query).await.is_err());
    }
    registry.close().await;
}

#[tokio::test]
async fn lost_read_response_does_not_acknowledge_and_commit_publishes_atomically() {
    let (_temp, r, observers) = setup().await;
    let run = teams::start(&r, "team", "Review", 8, 16, 120, 20)
        .await
        .unwrap();
    let work = teams::next(&r).await.unwrap().unwrap();
    let token = fixture::read_credential(&work.credential_file).unwrap();
    let read = Action::Receive {
        run_id: run.clone(),
    };
    assert!(teams::act(&r, &observers[0], read.clone()).await.is_err());
    let first = teams::act(&r, &token, read.clone()).await.unwrap();
    let repeated = teams::act(&r, &token, read).await.unwrap();
    assert_eq!(
        first, repeated,
        "lost transport responses are safely readable again"
    );
    assert_eq!(first["ownership_epoch"], 1);
    assert_eq!(
        teams::inspect_run(&r, &observers[0], &run).await.unwrap()["pending_message_count"],
        1
    );
    let staged = send(&r, &token, &run, "a", "assignment").await;
    assert_eq!(staged["status"], "staged");
    assert_eq!(
        teams::messages(&r, &observers[0], &run).await.unwrap()["messages"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let receipt = commit(&r, &work).await;
    assert_eq!(
        receipt,
        commit(&r, &work).await,
        "duplicate commit returns its exact durable receipt"
    );
    let messages = teams::messages(&r, &observers[0], &run).await.unwrap();
    assert_eq!(messages["messages"].as_array().unwrap().len(), 2);
    assert_eq!(messages["messages"][0]["delivered_turn"], work.turn_id);
    assert!(
        teams::act(
            &r,
            &token,
            Action::Send {
                run_id: run.clone(),
                to: "b".into(),
                body: "late".into(),
                reply_to: None,
                idempotency_key: "late".into(),
            }
        )
        .await
        .is_err()
    );
    assert!(
        teams::act(
            &r,
            &token,
            Action::Commit {
                run_id: run.clone(),
                idempotency_key: "changed".into()
            }
        )
        .await
        .is_err()
    );
    teams::finish(&r, &work, Ok(native_success()))
        .await
        .unwrap();
    assert_eq!(
        teams::next(&r).await.unwrap().unwrap().agent.id.as_str(),
        "a"
    );
    r.close().await;
}

#[tokio::test]
async fn successful_native_exit_without_commit_keeps_inputs_and_fences_old_lease() {
    let (_temp, r, observers) = setup().await;
    let run = teams::start(&r, "team", "Review", 8, 16, 120, 20)
        .await
        .unwrap();
    let first = teams::next(&r).await.unwrap().unwrap();
    let old = fixture::read_credential(&first.credential_file).unwrap();
    let read = Action::Receive {
        run_id: run.clone(),
    };
    let inbox = teams::act(&r, &old, read.clone()).await.unwrap();
    teams::finish(&r, &first, Ok(native_success()))
        .await
        .unwrap();
    assert_eq!(
        teams::inspect_run(&r, &observers[0], &run).await.unwrap()["pending_message_count"],
        1
    );
    assert!(
        teams::act(
            &r,
            &old,
            Action::Commit {
                run_id: run.clone(),
                idempotency_key: "late".into()
            }
        )
        .await
        .is_err()
    );
    teams::resume(&r, &run).await.unwrap();
    let second = teams::next(&r).await.unwrap().unwrap();
    let new = fixture::read_credential(&second.credential_file).unwrap();
    assert_ne!(old, new);
    let next_inbox = teams::act(&r, &new, read.clone()).await.unwrap();
    assert_eq!(next_inbox["ownership_epoch"], 2);
    assert_eq!(next_inbox["messages"], inbox["messages"]);
    assert!(teams::act(&r, &old, read).await.is_err());
    assert!(
        teams::record_work_binding(&r, &first, &uuid::Uuid::new_v4().to_string())
            .await
            .is_err()
    );
    assert!(
        teams::finish(&r, &first, Ok(native_success()))
            .await
            .is_err()
    );
    r.close().await;
}

#[tokio::test]
async fn native_failure_never_publishes_uncommitted_outbox_or_consumes_input() {
    let (_temp, r, observers) = setup().await;
    let run = teams::start(&r, "team", "Review", 8, 16, 120, 20)
        .await
        .unwrap();
    let work = teams::next(&r).await.unwrap().unwrap();
    let token = fixture::read_credential(&work.credential_file).unwrap();
    teams::act(
        &r,
        &token,
        Action::Receive {
            run_id: run.clone(),
        },
    )
    .await
    .unwrap();
    send(&r, &token, &run, "a", "uncommitted").await;
    teams::finish(
        &r,
        &work,
        Err(anyhow::anyhow!("transport lost after staging")),
    )
    .await
    .unwrap();
    let state = teams::inspect_run(&r, &observers[0], &run).await.unwrap();
    assert_eq!(state["state"], "failed");
    assert_eq!(state["message_count"], 1);
    assert_eq!(state["pending_message_count"], 1);
    assert!(teams::resume(&r, &run).await.is_err());
    assert!(teams::next(&r).await.unwrap().is_none());
    r.close().await;
}

#[tokio::test]
async fn committed_proposal_survives_native_failure_and_service_restart() {
    for crash in [false, true] {
        let (_temp, r, observers) = setup().await;
        let run = teams::start(&r, "team", "Review", 8, 16, 120, 20)
            .await
            .unwrap();
        let lead = teams::next(&r).await.unwrap().unwrap();
        let token = fixture::read_credential(&lead.credential_file).unwrap();
        send(&r, &token, &run, "a", "assign_a").await;
        send(&r, &token, &run, "b", "assign_b").await;
        commit(&r, &lead).await;
        teams::finish(&r, &lead, Ok(native_success()))
            .await
            .unwrap();
        for _ in 0..2 {
            let worker = teams::next(&r).await.unwrap().unwrap();
            let token = fixture::read_credential(&worker.credential_file).unwrap();
            send(&r, &token, &run, "lead", "report").await;
            commit(&r, &worker).await;
            teams::finish(&r, &worker, Ok(native_success()))
                .await
                .unwrap();
        }
        let lead = teams::next(&r).await.unwrap().unwrap();
        let token = fixture::read_credential(&lead.credential_file).unwrap();
        let action = Action::Complete {
            run_id: run.clone(),
            result: "Exact durable proposed bytes\n".into(),
        };
        let receipt = teams::act(&r, &token, action.clone()).await.unwrap();
        assert_eq!(
            receipt,
            teams::act(&r, &token, action.clone()).await.unwrap()
        );
        if crash {
            teams::recover_interrupted(&r).await.unwrap();
        } else {
            teams::finish(
                &r,
                &lead,
                Err(anyhow::anyhow!("native process failed after proposal")),
            )
            .await
            .unwrap();
        }
        let state = teams::inspect_run(&r, &observers[0], &run).await.unwrap();
        assert_eq!(state["state"], if crash { "interrupted" } else { "failed" });
        assert_eq!(
            state["proposal"]["result"],
            "Exact durable proposed bytes\n"
        );
        assert_eq!(state["result"], state["proposal"]["result"]);
        assert_eq!(state["pending_message_count"], 0);
        assert_eq!(state["acceptance"], "not_independently_verified");
        assert!(teams::next(&r).await.unwrap().is_none());
        if crash {
            assert!(teams::act(&r, &token, action).await.is_err());
        }
        r.close().await;
    }
}

#[tokio::test]
async fn migration_preserves_v4_records_and_old_tokens_are_read_only() {
    let (temp, r, observers) = setup().await;
    let run = teams::start(&r, "team", "Legacy objective", 8, 16, 120, 20)
        .await
        .unwrap();
    let path = temp.path().join("state/registry.sqlite3");
    r.close().await;
    let pool = sqlx::SqlitePool::connect(&format!("sqlite:{}", path.display()))
        .await
        .unwrap();
    sqlx::raw_sql("DROP TABLE turn_inputs; DROP TABLE turn_leases; DROP TABLE agent_epochs; DROP TABLE run_proposals; ALTER TABLE messages DROP COLUMN staged_turn; PRAGMA user_version=4;").execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO turns(id,run_id,agent_id,state,started_at) VALUES('legacy_turn',?,'lead','completed',?)").bind(&run).bind(teams::now()).execute(&pool).await.unwrap();
    sqlx::query(
        "UPDATE runs SET state='completed',result='Historical result bytes',turns=1 WHERE id=?",
    )
    .bind(&run)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query("UPDATE messages SET delivered_turn='legacy_turn' WHERE run_id=?")
        .bind(&run)
        .execute(&pool)
        .await
        .unwrap();
    pool.close().await;
    let upgraded = Registry::open(&path).await.unwrap();
    assert_eq!(
        teams::inspect_run(&upgraded, &observers[0], &run)
            .await
            .unwrap()["message_count"],
        1
    );
    let old_state = teams::inspect_run(&upgraded, &observers[0], &run)
        .await
        .unwrap();
    assert_eq!(old_state["proposal"]["result"], "Historical result bytes");
    assert_eq!(old_state["proposal"]["turn_id"], "legacy_turn");
    assert_eq!(old_state["pending_message_count"], 0);
    let run = teams::start(&upgraded, "team", "New objective", 8, 16, 120, 20)
        .await
        .unwrap();
    let work = teams::next(&upgraded).await.unwrap().unwrap();
    assert!(
        teams::act(
            &upgraded,
            &observers[0],
            Action::Commit {
                run_id: run.clone(),
                idempotency_key: "old_authority".into()
            }
        )
        .await
        .is_err()
    );
    assert_eq!(commit(&upgraded, &work).await["status"], "committed");
    upgraded.close().await;
}

#[tokio::test]
async fn expired_lease_cannot_publish_or_acknowledge_work() {
    let (temp, r, observers) = setup().await;
    let run = teams::start(&r, "team", "Work", 8, 16, 120, 20)
        .await
        .unwrap();
    let work = teams::next(&r).await.unwrap().unwrap();
    let token = fixture::read_credential(&work.credential_file).unwrap();
    send(&r, &token, &run, "a", "staged").await;
    let pool = sqlx::SqlitePool::connect(&format!(
        "sqlite:{}",
        temp.path().join("state/registry.sqlite3").display()
    ))
    .await
    .unwrap();
    sqlx::query("UPDATE turn_leases SET expires_at=0 WHERE turn_id=?")
        .bind(&work.turn_id)
        .execute(&pool)
        .await
        .unwrap();
    pool.close().await;
    assert!(
        teams::act(
            &r,
            &token,
            Action::Commit {
                run_id: run.clone(),
                idempotency_key: "expired".into()
            }
        )
        .await
        .is_err()
    );
    let state = teams::inspect_run(&r, &observers[0], &run).await.unwrap();
    assert_eq!(state["message_count"], 1);
    assert_eq!(state["pending_message_count"], 1);
    r.close().await;
}
