//! Opt-in real-provider acceptance test. Never runs in ordinary CI.
#![cfg(unix)]
use agentisan::{
    fixture,
    model::*,
    registry::Registry,
    teams::{self, MemberConfig, Provider, TeamConfig},
};
use serde_json::Value;
use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    process::Stdio,
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    process::{Child, Command},
};

struct Service(Child);
impl Drop for Service {
    fn drop(&mut self) {
        if let Some(id) = self.0.id() {
            let _ = nix::sys::signal::kill(
                nix::unistd::Pid::from_raw(id as i32),
                nix::sys::signal::Signal::SIGINT,
            );
        }
    }
}

#[tokio::test]
#[ignore = "real account model calls: requires explicit AGENTISAN_LIVE_TESTS=1 and CLI/state paths"]
async fn both_provider_topologies_exchange_real_mcp_messages() {
    assert_eq!(std::env::var("AGENTISAN_LIVE_TESTS").as_deref(), Ok("1"));
    let data = PathBuf::from(
        std::env::var("AGENTISAN_LIVE_STATE_DIR")
            .expect("choose a private persistent output directory"),
    );
    assert!(data.is_absolute());
    let claude =
        PathBuf::from(std::env::var("AGENTISAN_CLAUDE_BIN").expect("absolute Claude CLI path"));
    let codex =
        PathBuf::from(std::env::var("AGENTISAN_CODEX_BIN").expect("absolute Codex CLI path"));
    let registry = Registry::open(&fixture::database_path(&data).unwrap())
        .await
        .unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_agentisan"))
        .arg("--data-dir")
        .arg(&data)
        .args(["serve", "--listen", "127.0.0.1:0", "--enable-cli-workers"])
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap();
    let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
    let readiness = tokio::time::timeout(Duration::from_secs(10), lines.next_line())
        .await
        .unwrap()
        .unwrap()
        .expect("service readiness");
    let _: Value = serde_json::from_str(&readiness).unwrap();
    let _service = Service(child);
    for lead_provider in [Provider::Claude, Provider::Codex] {
        let prefix = teams::new_id("acceptance");
        let ids = [
            format!("{prefix}_lead"),
            format!("{prefix}_a"),
            format!("{prefix}_b"),
        ];
        let mut members = Vec::new();
        for i in 0..3 {
            let provider = if i == 0 {
                lead_provider.clone()
            } else {
                match lead_provider {
                    Provider::Claude => Provider::Codex,
                    Provider::Codex => Provider::Claude,
                }
            };
            let instructions = if i == 0 {
                "Coordinate the objective. Assign both workers and require each to exchange its own findings directly with the other before reporting to you. Integrate both reports.".into()
            } else {
                format!(
                    "You specialize in {}. Propose two concrete verification checks for an agent runtime. Send your own findings to peer {} once. After receiving the peer's findings, report your combined conclusion to {}. No acknowledgement-only loops. End your turn while waiting for peer input.",
                    if i == 1 {
                        "identity binding, permission scope and message deduplication"
                    } else {
                        "process supervision, deadlines and interrupted execution"
                    },
                    ids[if i == 1 { 2 } else { 1 }],
                    ids[0]
                )
            };
            members.push(MemberConfig {
                id: AgentId(ids[i].clone()),
                name: format!("Agent {i}"),
                role: if i == 0 {
                    AgentRole::Lead
                } else {
                    AgentRole::Worker
                },
                executable: if matches!(provider, Provider::Claude) {
                    claude.clone()
                } else {
                    codex.clone()
                },
                model: if matches!(provider, Provider::Claude) {
                    "sonnet".into()
                } else {
                    "gpt-6-astra".into()
                },
                effort: "low".into(),
                provider,
                instructions,
            });
        }
        let config = TeamConfig {
            group: Group {
                id: GroupId(prefix.clone()),
                name: "Live acceptance".into(),
            },
            team: Team {
                id: TeamId(prefix.clone()),
                group_id: GroupId(prefix.clone()),
                name: "Cross-provider team".into(),
            },
            agents: members,
        };
        teams::create(&registry, &data, &config).await.unwrap();
        let token = fixture::read_credential(
            &data
                .join("managed")
                .join(&prefix)
                .join(format!("{}.token", ids[0])),
        )
        .unwrap();
        let run=teams::start(&registry,&prefix,"Produce a concise verification plan for a persistent agent-team runtime. Use both specialists, require a direct exchange of findings in both directions, then receive a report from each before proposing completion.",16,40,600,90).await.unwrap();
        let start = Instant::now();
        let completed = loop {
            let state = teams::inspect_run(&registry, &token, &run).await.unwrap();
            match state["state"].as_str().unwrap() {
                "completed" => break state,
                "queued" | "running" | "completing" => {}
                other => panic!("run {run} stopped as {other}: {}", state["error"]),
            }
            assert!(
                start.elapsed() < Duration::from_secs(620),
                "live acceptance deadline"
            );
            tokio::time::sleep(Duration::from_secs(2)).await;
        };
        let history = teams::messages(&registry, &token, &run).await.unwrap();
        let messages = history["messages"].as_array().unwrap();
        for (from, to) in [(0, 1), (0, 2), (1, 2), (2, 1), (1, 0), (2, 0)] {
            assert!(
                messages.iter().any(|m| m["from"] == ids[from]
                    && m["to"] == ids[to]
                    && m["body"].as_str().is_some_and(|s| s.len() > 20)
                    && !m["delivered_turn"].is_null()),
                "missing delivered route {} -> {}",
                ids[from],
                ids[to]
            );
        }
        let mut sessions: HashMap<String, HashSet<String>> = HashMap::new();
        for turn in completed["turns"].as_array().unwrap() {
            assert_eq!(turn["state"], "completed");
            sessions
                .entry(turn["agent_id"].as_str().unwrap().into())
                .or_default()
                .insert(turn["native_id"].as_str().unwrap().into());
        }
        assert_eq!(sessions.len(), 3);
        assert!(sessions.values().all(|s| s.len() == 1));
        assert_eq!(sessions.values().flatten().collect::<HashSet<_>>().len(), 3);
        std::fs::write(
            data.join(format!("{run}-result.json")),
            serde_json::to_vec_pretty(&completed).unwrap(),
        )
        .unwrap();
        std::fs::write(
            data.join(format!("{run}-messages.json")),
            serde_json::to_vec_pretty(&history).unwrap(),
        )
        .unwrap();
        println!(
            "{} lead: completed {run}; all six communication routes verified",
            lead_provider.as_str()
        );
    }
    registry.close().await;
}
