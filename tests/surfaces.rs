//! Exercise the actual binary, HTTP boundary, and MCP stdio protocol without model calls.
use agentisan::{
    fixture,
    model::{AgentRole, Group, Team},
    registry::Registry,
    server,
    teams::{self, MemberConfig, Provider, TeamConfig},
};
use serde_json::{Value, json};
use std::{path::PathBuf, process::Stdio, time::Duration};
use tempfile::TempDir;
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines},
    process::{Child, ChildStdin, ChildStdout, Command},
};

const BIN: &str = env!("CARGO_BIN_EXE_agentisan");

struct Demo {
    dir: TempDir,
    service: Child,
    endpoint: String,
}
impl Demo {
    async fn start() -> Self {
        let dir = tempfile::tempdir().unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let out = Command::new(BIN)
            .args([
                "--data-dir",
                dir.path().to_str().unwrap(),
                "init",
                "--fixture",
            ])
            .arg(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("examples/registry.json"))
            .kill_on_drop(true)
            .output()
            .await
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let (service, endpoint) = Self::launch(&dir).await;
        Self {
            dir,
            service,
            endpoint,
        }
    }
    async fn launch(dir: &TempDir) -> (Child, String) {
        let mut service = Command::new(BIN)
            .args([
                "--data-dir",
                dir.path().to_str().unwrap(),
                "serve",
                "--listen",
                "127.0.0.1:0",
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let mut lines = BufReader::new(service.stdout.take().unwrap()).lines();
        let line = tokio::time::timeout(Duration::from_secs(10), lines.next_line())
            .await
            .unwrap()
            .unwrap()
            .expect("readiness");
        let ready: Value = serde_json::from_str(&line).unwrap();
        (
            service,
            format!("http://{}", ready["address"].as_str().unwrap()),
        )
    }
    fn credential(&self, principal: &str) -> PathBuf {
        self.dir
            .path()
            .join("credentials")
            .join(format!("{principal}.token"))
    }
    fn command(&self, principal: Option<&str>) -> Command {
        let mut command = Command::new(BIN);
        command.args(["--endpoint", &self.endpoint]);
        if let Some(p) = principal {
            command.arg("--credential-file").arg(self.credential(p));
        }
        command.kill_on_drop(true);
        command
    }
    async fn cli(&self, principal: Option<&str>, args: &[&str]) -> Value {
        let out = self.command(principal).args(args).output().await.unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        serde_json::from_slice(&out.stdout).unwrap()
    }
}

struct Mcp {
    child: Child,
    input: ChildStdin,
    output: Lines<BufReader<ChildStdout>>,
    seq: u64,
}
impl Mcp {
    async fn start(demo: &Demo, principal: Option<&str>, profile: &str) -> Self {
        let credential = principal.map(|value| demo.credential(value));
        Self::start_at(&demo.endpoint, credential.as_deref(), profile).await
    }

    async fn start_at(endpoint: &str, credential: Option<&std::path::Path>, profile: &str) -> Self {
        let mut command = Command::new(BIN);
        command.args(["--endpoint", endpoint]);
        if let Some(path) = credential {
            command.arg("--credential-file").arg(path);
        }
        let mut child = command
            .args(["mcp", "--profile", profile])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        let input = child.stdin.take().unwrap();
        let output = BufReader::new(child.stdout.take().unwrap()).lines();
        let mut client = Self {
            child,
            input,
            output,
            seq: 0,
        };
        let init = client.request("initialize", json!({"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"agentisan-test","version":"1"}})).await;
        assert!(init.get("result").is_some(), "{init}");
        client
            .send(json!({"jsonrpc":"2.0","method":"notifications/initialized"}))
            .await;
        client
    }
    async fn send(&mut self, value: Value) {
        self.input
            .write_all(format!("{value}\n").as_bytes())
            .await
            .unwrap();
        self.input.flush().await.unwrap();
    }
    async fn request(&mut self, method: &str, params: Value) -> Value {
        self.seq += 1;
        let id = self.seq;
        self.send(json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}))
            .await;
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let line = self
                    .output
                    .next_line()
                    .await
                    .unwrap()
                    .expect("MCP response");
                let value: Value = serde_json::from_str(&line).unwrap();
                if value.get("id") == Some(&json!(id)) {
                    return value;
                }
            }
        })
        .await
        .expect("MCP request deadline")
    }
    async fn call(&mut self, name: &str, arguments: Value) -> Value {
        self.request("tools/call", json!({"name":name,"arguments":arguments}))
            .await
    }
}

#[tokio::test]
async fn agent_mcp_reads_stable_input_and_commits_it_once() {
    let temp = tempfile::tempdir().unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let registry = Registry::open(&fixture::database_path(temp.path()).unwrap())
        .await
        .unwrap();
    teams::create(
        &registry,
        temp.path(),
        &TeamConfig {
            group: Group {
                id: "managed".into(),
                name: "Managed".into(),
            },
            team: Team {
                id: "managed".into(),
                group_id: "managed".into(),
                name: "Managed".into(),
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
    let observer =
        fixture::read_credential(&temp.path().join("managed/managed/lead.token")).unwrap();
    let run = teams::start(&registry, "managed", "MCP input", 2, 2, 60, 10)
        .await
        .unwrap();
    let work = teams::next(&registry).await.unwrap().unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let service_registry = registry.clone();
    let service = tokio::spawn(async move {
        axum::serve(listener, server::router(service_registry))
            .await
            .unwrap();
    });
    let mut mcp = Mcp::start_at(&endpoint, Some(&work.credential_file), "agent").await;

    let context = tool_json(&mcp.call("agent_context_get", json!({})).await);
    assert_eq!(context["identity"]["evidence"], "turn_lease");
    assert_eq!(context["identity"]["lease"]["run_id"], run);
    let first = tool_json(&mcp.call("inbox_read", json!({"run_id":run})).await);
    let second = tool_json(&mcp.call("inbox_read", json!({"run_id":run})).await);
    assert_eq!(first, second);
    assert_eq!(first["messages"][0]["from"], "human");
    assert!(
        teams::messages(&registry, &observer, &run).await.unwrap()["messages"][0]["delivered_turn"]
            .is_null()
    );
    let commit = tool_json(
        &mcp.call(
            "turn_commit",
            json!({"run_id":run,"idempotency_key":"commit_once"}),
        )
        .await,
    );
    let repeated = tool_json(
        &mcp.call(
            "turn_commit",
            json!({"run_id":run,"idempotency_key":"commit_once"}),
        )
        .await,
    );
    assert_eq!(commit, repeated);
    assert_eq!(commit["status"], "committed");
    assert!(
        teams::messages(&registry, &observer, &run).await.unwrap()["messages"][0]["delivered_turn"]
            .is_string()
    );
    mcp.child.kill().await.unwrap();

    let mut controller = Mcp::start_at(
        &endpoint,
        Some(&temp.path().join("managed/managed/lead.token")),
        "controller",
    )
    .await;
    let catalog = controller.request("tools/list", json!({})).await;
    let names: Vec<_> = catalog["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect();
    assert_eq!(names.len(), 5);
    assert!(names.contains(&"team_run_start"));
    let context = tool_json(&controller.call("controller_context_get", json!({})).await);
    assert_eq!(context["team_id"], "managed");
    assert_eq!(context["chat_identity"], "not_asserted");
    let start = controller
        .call(
            "team_run_start",
            json!({
                "objective":"Another objective",
                "live":true,
                "idempotency_key":"desktop_1",
                "max_turns":null,
                "max_messages":null,
                "timeout_seconds":null,
                "turn_timeout_seconds":null
            }),
        )
        .await;
    assert_eq!(start["result"]["isError"], true);
    assert_eq!(
        serde_json::from_str::<Value>(start["result"]["content"][0]["text"].as_str().unwrap())
            .unwrap()["error"]["code"],
        "scheduler_unavailable"
    );
    controller.child.kill().await.unwrap();

    let mut operator = Mcp::start_at(
        &endpoint,
        Some(&temp.path().join("managed/managed/lead.token")),
        "operator",
    )
    .await;
    let catalog = operator.request("tools/list", json!({})).await;
    let tools = catalog["result"]["tools"].as_array().unwrap();
    let mut names: Vec<_> = tools
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect();
    names.sort_unstable();
    assert_eq!(
        names,
        ["team_cancel", "team_start", "team_status", "team_update"]
    );
    let start_tool = tools
        .iter()
        .find(|tool| tool["name"] == "team_start")
        .unwrap();
    assert_eq!(start_tool["inputSchema"]["additionalProperties"], false);
    assert!(
        start_tool["inputSchema"]["properties"]
            .get("connector_instance")
            .is_none()
    );
    let start = operator
        .call(
            "team_start",
            json!({
                "objective":"Interactive objective",
                "live":true,
                "workers":["worker"],
                "initial_work":[{
                    "assignee":"worker",
                    "objective":"Review",
                    "done_criteria":["Report evidence"]
                }],
                "idempotency_key":"operator_1",
                "max_turns":4,
                "max_messages":8,
                "timeout_seconds":60,
                "turn_timeout_seconds":10
            }),
        )
        .await;
    assert_eq!(start["result"]["isError"], true);
    assert_eq!(
        serde_json::from_str::<Value>(start["result"]["content"][0]["text"].as_str().unwrap())
            .unwrap()["error"]["code"],
        "scheduler_unavailable"
    );
    operator.child.kill().await.unwrap();
    service.abort();
    registry.close().await;
}
fn tool_json(response: &Value) -> Value {
    assert_ne!(response["result"]["isError"], true, "{response}");
    serde_json::from_str(response["result"]["content"][0]["text"].as_str().unwrap()).unwrap()
}

#[tokio::test]
async fn explicit_mcp_profiles_expose_separate_least_privilege_catalogs() {
    let demo = Demo::start().await;

    let missing_profile = demo
        .command(Some("inventory_reader"))
        .arg("mcp")
        .output()
        .await
        .unwrap();
    assert!(!missing_profile.status.success());

    let mut observer = Mcp::start(&demo, Some("inventory_reader"), "observer").await;
    let observer_tools = observer.request("tools/list", json!({})).await;
    let mut observer_names: Vec<_> = observer_tools["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect();
    observer_names.sort_unstable();
    assert_eq!(
        observer_names,
        [
            "agents_inspect",
            "agents_list",
            "groups_list",
            "messages_list",
            "runs_inspect",
            "teams_list",
            "whoami",
        ]
    );
    assert!(
        observer_tools["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .all(|tool| tool["annotations"]["readOnlyHint"] == true)
    );

    let mut agent = Mcp::start(&demo, Some("inventory_reader"), "agent").await;
    let agent_tools = agent.request("tools/list", json!({})).await;
    let mut agent_names: Vec<_> = agent_tools["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect();
    agent_names.sort_unstable();
    assert_eq!(
        agent_names,
        [
            "agent_context_get",
            "assignment_create",
            "assignment_update",
            "decision_request",
            "inbox_read",
            "message_send",
            "result_propose",
            "turn_commit"
        ]
    );
    for tool in agent_tools["result"]["tools"].as_array().unwrap() {
        assert_eq!(tool["inputSchema"]["additionalProperties"], false, "{tool}");
        assert_eq!(tool["annotations"]["destructiveHint"], false);
    }
    let inbox = agent_tools["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|tool| tool["name"] == "inbox_read")
        .unwrap();
    assert_eq!(inbox["annotations"]["readOnlyHint"], false);
    assert_eq!(inbox["annotations"]["idempotentHint"], true);
    let commit = agent_tools["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|tool| tool["name"] == "turn_commit")
        .unwrap();
    assert_eq!(commit["annotations"]["readOnlyHint"], false);
    assert_eq!(commit["annotations"]["idempotentHint"], true);
    assert_eq!(
        tool_json(&agent.call("agent_context_get", json!({})).await)["identity"]["agent_id"],
        "inventory_lead"
    );

    let broad_read = agent.call("groups_list", json!({})).await;
    assert!(broad_read.get("error").is_some(), "{broad_read}");
    let spoof = agent
        .call(
            "message_send",
            json!({
                "run_id":"run_fake",
                "to":"inventory_lead",
                "body":"hello",
                "reply_to":null,
                "idempotency_key":"send_1",
                "sender":"support_lead"
            }),
        )
        .await;
    assert!(spoof.get("error").is_some() || spoof["result"]["isError"] == true);
    let rejected = agent.call("inbox_read", json!({"run_id":"run_fake"})).await;
    assert_eq!(rejected["result"]["isError"], true);
    assert_eq!(
        serde_json::from_str::<Value>(rejected["result"]["content"][0]["text"].as_str().unwrap())
            .unwrap()["error"]["code"],
        "lease_not_active"
    );
}

#[tokio::test]
async fn cli_and_mcp_parity_with_connector_disconnect_and_service_restart() {
    let mut demo = Demo::start().await;
    let mut mcp = Mcp::start(&demo, Some("inventory_reader"), "observer").await;
    let tools = mcp.request("tools/list", json!({})).await;
    let names: Vec<_> = tools["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x["name"].as_str().unwrap())
        .collect();
    assert_eq!(names.len(), 7);
    for tool in tools["result"]["tools"].as_array().unwrap() {
        assert_eq!(tool["annotations"]["readOnlyHint"], true);
        assert_eq!(tool["annotations"]["destructiveHint"], false);
    }
    for name in [
        "whoami",
        "groups_list",
        "teams_list",
        "agents_list",
        "agents_inspect",
    ] {
        assert!(names.contains(&name));
    }
    for (args, tool, params) in [
        (vec!["whoami"], "whoami", json!({})),
        (vec!["groups", "list"], "groups_list", json!({})),
        (
            vec!["teams", "list", "--group", "inventory"],
            "teams_list",
            json!({"group_id":"inventory"}),
        ),
        (
            vec!["agents", "list", "--team", "csv_export"],
            "agents_list",
            json!({"team_id":"csv_export"}),
        ),
        (
            vec!["agents", "inspect", "inventory_worker"],
            "agents_inspect",
            json!({"agent_id":"inventory_worker"}),
        ),
    ] {
        assert_eq!(
            demo.cli(Some("inventory_reader"), &args).await,
            tool_json(&mcp.call(tool, params).await)
        );
    }
    mcp.child.kill().await.unwrap();
    let before = demo
        .cli(
            Some("inventory_reader"),
            &["agents", "inspect", "inventory_worker"],
        )
        .await;
    assert_eq!(before["capabilities"]["execute"], false);
    assert_eq!(before["capabilities"]["native_resume"], false);
    demo.service.kill().await.unwrap();
    let (service, endpoint) = Demo::launch(&demo.dir).await;
    demo.service = service;
    demo.endpoint = endpoint;
    assert_eq!(
        before,
        demo.cli(
            Some("inventory_reader"),
            &["agents", "inspect", "inventory_worker"]
        )
        .await
    );
}

#[tokio::test]
async fn caller_isolation_and_unbound_identity_through_real_interfaces() {
    let demo = Demo::start().await;
    let groups = demo
        .cli(Some("inventory_reader"), &["groups", "list"])
        .await;
    assert_eq!(groups["groups"].as_array().unwrap().len(), 1);
    assert_eq!(groups["groups"][0]["id"], "inventory");
    assert_eq!(demo.cli(None, &["whoami"]).await["status"], "unbound");
    assert_eq!(
        demo.cli(Some("observer"), &["whoami"]).await["status"],
        "unbound"
    );
    let out = demo
        .command(Some("inventory_reader"))
        .args(["agents", "inspect", "support_lead"])
        .output()
        .await
        .unwrap();
    assert!(!out.status.success());
    let mut mcp = Mcp::start(&demo, Some("inventory_reader"), "observer").await;
    let denied = mcp
        .call("agents_inspect", json!({"agent_id":"support_lead"}))
        .await;
    let missing = mcp
        .call("agents_inspect", json!({"agent_id":"missing"}))
        .await;
    assert_eq!(denied["result"], missing["result"]);
    assert_eq!(denied["result"]["isError"], true);
    let spoof = mcp
        .call(
            "agents_inspect",
            json!({"agent_id":"support_lead","principal_id":"support_reader"}),
        )
        .await;
    assert!(spoof.get("error").is_some() || spoof["result"]["isError"] == true);
    let mut anonymous = Mcp::start(&demo, None, "observer").await;
    assert_eq!(
        tool_json(&anonymous.call("whoami", json!({})).await)["status"],
        "unbound"
    );
    assert_eq!(
        anonymous.call("groups_list", json!({})).await["result"]["isError"],
        true
    );
}

#[tokio::test]
async fn http_boundary_rejects_spoofed_identity_origin_and_invalid_credentials() {
    let demo = Demo::start().await;
    let http = reqwest::Client::builder().no_proxy().build().unwrap();
    let url = format!("{}/v1/inspect", demo.endpoint);
    let token = std::fs::read_to_string(demo.credential("inventory_reader")).unwrap();
    let spoof = http
        .post(&url)
        .bearer_auth(token.trim())
        .json(&json!({"method":"groups_list","principal_id":"support_reader"}))
        .send()
        .await
        .unwrap();
    assert_eq!(spoof.status(), 422);
    let origin = http
        .post(&url)
        .header("Origin", "https://example.com")
        .bearer_auth(token.trim())
        .json(&json!({"method":"groups_list"}))
        .send()
        .await
        .unwrap();
    assert_eq!(origin.status(), 403);
    let invalid = http
        .post(&url)
        .bearer_auth("invalid")
        .json(&json!({"method":"whoami"}))
        .send()
        .await
        .unwrap();
    assert_eq!(invalid.status(), 401);
    let mutation = http
        .post(format!("{}/v1/register", demo.endpoint))
        .bearer_auth(token.trim())
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(mutation.status(), 404);
}

#[test]
fn clients_reject_endpoints_outside_local_origin_contract() {
    use agentisan::client::Client;
    for url in [
        "http://example.com",
        "http://localhost:7437",
        "http://user:pass@127.0.0.1",
        "http://127.0.0.1/path",
        "http://127.0.0.1?x=y",
    ] {
        assert!(Client::new(url, None).is_err(), "{url}");
    }
    assert!(Client::new("http://127.0.0.1:7437", None).is_ok());
}
