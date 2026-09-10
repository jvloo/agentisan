//! Exercise the actual binary, HTTP boundary, and MCP stdio protocol without model calls.
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
    async fn start(demo: &Demo, principal: Option<&str>) -> Self {
        let mut child = demo
            .command(principal)
            .arg("mcp")
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
fn tool_json(response: &Value) -> Value {
    assert_ne!(response["result"]["isError"], true, "{response}");
    serde_json::from_str(response["result"]["content"][0]["text"].as_str().unwrap()).unwrap()
}

#[tokio::test]
async fn cli_and_mcp_parity_with_connector_disconnect_and_service_restart() {
    let mut demo = Demo::start().await;
    let mut mcp = Mcp::start(&demo, Some("inventory_reader")).await;
    let tools = mcp.request("tools/list", json!({})).await;
    let names: Vec<_> = tools["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x["name"].as_str().unwrap())
        .collect();
    assert_eq!(names.len(), 5);
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
    let mut mcp = Mcp::start(&demo, Some("inventory_reader")).await;
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
    let mut anonymous = Mcp::start(&demo, None).await;
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
