use crate::teams::{MemberConfig, Provider};
use anyhow::{Context, Result, anyhow, bail};
use serde_json::Value;
use std::path::Path;

pub fn build(
    member: &MemberConfig,
    mcp_exe: &Path,
    mcp_args: &[String],
    native_id: Option<&str>,
    new_claude_id: &str,
) -> Vec<String> {
    match member.provider {
        Provider::Claude => build_claude(member, mcp_exe, mcp_args, native_id, new_claude_id),
        Provider::Codex => build_codex(member, mcp_exe, mcp_args, native_id),
    }
}

fn build_claude(
    member: &MemberConfig,
    mcp_exe: &Path,
    mcp_args: &[String],
    native_id: Option<&str>,
    new_claude_id: &str,
) -> Vec<String> {
    let mcp_config = serde_json::json!({
        "mcpServers": {
            "agentisan": {
                "command": mcp_exe.to_string_lossy(),
                "args": mcp_args,
            }
        }
    })
    .to_string();

    let mut args = vec![
        "-p".to_string(),
        "--restricted".to_string(),
        "--strict-mcp-config".to_string(),
        "--setting-sources".to_string(),
        "".to_string(),
        "--settings".to_string(),
        r#"{"disableAllHooks":true}"#.to_string(),
        "--permission-mode".to_string(),
        "dontAsk".to_string(),
        "--permission-prompts".to_string(),
        "none".to_string(),
        "--tools".to_string(),
        "".to_string(),
        "--allowedTools".to_string(),
        "mcp__agentisan__*".to_string(),
        "--disable-slash-commands".to_string(),
        "--no-chrome".to_string(),
        "--model".to_string(),
        member.model.clone(),
        "--effort".to_string(),
        member.effort.clone(),
        "--output-format".to_string(),
        "stream-json".to_string(),
        "--verbose".to_string(),
        "--max-budget-usd".to_string(),
        "0.50".to_string(),
        "--mcp-config".to_string(),
        mcp_config,
    ];

    match native_id {
        Some(id) => {
            args.push("--resume".to_string());
            args.push(id.to_string());
        }
        None => {
            args.push("--session-id".to_string());
            args.push(new_claude_id.to_string());
        }
    }

    args
}

fn build_codex(
    member: &MemberConfig,
    mcp_exe: &Path,
    mcp_args: &[String],
    native_id: Option<&str>,
) -> Vec<String> {
    let mut args = vec!["exec".to_string()];
    if native_id.is_some() {
        args.push("resume".to_string());
    }
    args.extend(
        [
            "--ignore-user-config",
            "--strict-config",
            "-c",
            r#"approval_policy="never""#,
            "-c",
            "agents.enabled=false",
            "-c",
            r#"model_provider="openai""#,
            "-c",
            r#"web_search="disabled""#,
            "-c",
            "project_doc_max_bytes=0",
        ]
        .iter()
        .map(|s| s.to_string()),
    );
    args.push("--model".to_string());
    args.push(member.model.clone());
    args.push("--json".to_string());
    args.push("--skip-git-repo-check".to_string());

    const DISABLE: &[&str] = &[
        "apps",
        "plugins",
        "hooks",
        "memories",
        "multi_agent",
        "browser_use",
        "computer_use",
        "image_generation",
        "remote_plugin",
        "shell_snapshot",
        "skill_mcp_dependency_install",
        "in_app_browser",
        "shell_tool",
        "unified_exec",
        "skill_search",
        "view_image",
        "sleep_tool",
        "tool_suggest",
        "workspace_dependencies",
    ];
    for d in DISABLE {
        args.push("--disable".to_string());
        args.push(d.to_string());
    }

    args.push("--enable".to_string());
    args.push("skip_host_skill_discovery".to_string());
    // MCP discovery/calls use this host even when shell and native execution tools are disabled.
    args.push("--enable".to_string());
    args.push("code_mode_host".to_string());

    match native_id {
        Some(_) => {
            args.push("-c".to_string());
            args.push(r#"sandbox_mode="read-only""#.to_string());
        }
        None => {
            args.push("--sandbox".to_string());
            args.push("read-only".to_string());
        }
    }

    args.push("-c".to_string());
    args.push(format!(
        "model_reasoning_effort={}",
        Value::String(member.effort.clone())
    ));

    args.push("-c".to_string());
    args.push(format!(
        "mcp_servers.agentisan.command={}",
        Value::String(mcp_exe.to_string_lossy().to_string())
    ));

    args.push("-c".to_string());
    let args_json: Vec<Value> = mcp_args.iter().map(|a| Value::String(a.clone())).collect();
    args.push(format!(
        "mcp_servers.agentisan.args={}",
        Value::Array(args_json)
    ));

    args.push("-c".to_string());
    args.push("mcp_servers.agentisan.required=true".to_string());

    match native_id {
        Some(id) => {
            args.push(id.to_string());
            args.push("-".to_string());
        }
        None => {
            args.push("-".to_string());
        }
    }

    args
}

fn is_valid_uuid(s: &str) -> bool {
    uuid::Uuid::parse_str(s).is_ok()
}

fn iter_lines(stdout: &str) -> impl Iterator<Item = &str> {
    stdout.lines().filter(|l| !l.trim().is_empty())
}

pub fn initial_native_id(provider: &Provider, stdout: &str) -> Option<String> {
    for line in iter_lines(stdout) {
        let value: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let id = match provider {
            Provider::Claude => {
                let ty = value.get("type").and_then(Value::as_str);
                match ty {
                    Some("system")
                        if value.get("subtype").and_then(Value::as_str) == Some("init") =>
                    {
                        value.get("session_id").and_then(Value::as_str)
                    }
                    Some("result") => value.get("session_id").and_then(Value::as_str),
                    _ => None,
                }
            }
            Provider::Codex => {
                if value.get("type").and_then(Value::as_str) == Some("thread.started") {
                    value.get("thread_id").and_then(Value::as_str)
                } else {
                    None
                }
            }
        };
        if let Some(id) = id
            && is_valid_uuid(id)
        {
            return Some(id.to_string());
        }
    }
    None
}

pub struct Parsed {
    pub native_id: String,
    pub output: String,
    pub usage: Value,
}

pub fn parse(provider: &Provider, stdout: &str, expected: Option<&str>) -> Result<Parsed> {
    let mut events = Vec::new();
    for line in iter_lines(stdout) {
        let value: Value = serde_json::from_str(line).context("malformed event line")?;
        events.push(value);
    }

    match provider {
        Provider::Claude => parse_claude(&events, expected),
        Provider::Codex => parse_codex(&events, expected),
    }
}

fn check_expected(session_id: &str, expected: Option<&str>) -> Result<()> {
    if let Some(expected) = expected
        && expected != session_id
    {
        bail!("session id mismatch");
    }
    Ok(())
}

fn parse_claude(events: &[Value], expected: Option<&str>) -> Result<Parsed> {
    let result = events
        .iter()
        .find(|e| e.get("type").and_then(Value::as_str) == Some("result"))
        .ok_or_else(|| anyhow!("no result event found"))?;

    let subtype = result.get("subtype").and_then(Value::as_str);
    let is_error = result
        .get("is_error")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let terminal_reason = result.get("terminal_reason").and_then(Value::as_str);

    if subtype != Some("success")
        || is_error
        || matches!(terminal_reason, Some(r) if r != "completed")
    {
        bail!("run did not complete successfully");
    }

    let session_id = result
        .get("session_id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("missing session id"))?;
    if !is_valid_uuid(session_id) {
        bail!("invalid session id");
    }
    check_expected(session_id, expected)?;

    let output = result
        .get("result")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("missing result output"))?
        .to_string();
    if output.is_empty() {
        bail!("empty output");
    }

    let mut usage = serde_json::json!({
        "usage": result.get("usage").cloned().unwrap_or(Value::Null),
        "modelUsage": result.get("modelUsage").cloned().unwrap_or(Value::Null),
        "total_cost_usd": result.get("total_cost_usd").cloned().unwrap_or(Value::Null),
        "known": false,
    });
    if result.get("usage").is_some_and(Value::is_object) {
        usage["known"] = Value::Bool(true);
    }

    Ok(Parsed {
        native_id: session_id.to_string(),
        output,
        usage,
    })
}

fn parse_codex(events: &[Value], expected: Option<&str>) -> Result<Parsed> {
    for e in events {
        let ty = e.get("type").and_then(Value::as_str);
        if ty == Some("error") || ty == Some("turn.failed") {
            bail!("run failed");
        }
    }

    let turn_completed = events
        .iter()
        .find(|e| e.get("type").and_then(Value::as_str) == Some("turn.completed"))
        .ok_or_else(|| anyhow!("no turn.completed event found"))?;

    let thread_id = events
        .iter()
        .find(|e| e.get("type").and_then(Value::as_str) == Some("thread.started"))
        .and_then(|e| e.get("thread_id"))
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("missing thread id"))?;
    if !is_valid_uuid(thread_id) {
        bail!("invalid thread id");
    }
    check_expected(thread_id, expected)?;

    let output = events
        .iter()
        .rev()
        .find_map(|e| {
            if e.get("type").and_then(Value::as_str) != Some("item.completed") {
                return None;
            }
            let item = e.get("item")?;
            if item.get("type").and_then(Value::as_str) != Some("agent_message") {
                return None;
            }
            item.get("text").and_then(Value::as_str)
        })
        .ok_or_else(|| anyhow!("missing final agent message"))?
        .to_string();
    // A yield after successful MCP calls may have an empty final text item. The
    // durable message receipts are the contribution; do not invent an agent reply.
    if output.trim().is_empty()
        && !events.iter().any(|e| {
            e["type"] == "item.completed"
                && e["item"]["type"] == "mcp_tool_call"
                && e["item"]["status"] == "completed"
        })
    {
        bail!("empty output without completed tool work");
    }

    let usage_val = turn_completed.get("usage").cloned();
    let known = usage_val.as_ref().is_some_and(Value::is_object);
    let usage = serde_json::json!({
        "usage": usage_val.unwrap_or(Value::Null),
        "known": known,
    });

    Ok(Parsed {
        native_id: thread_id.to_string(),
        output,
        usage,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_missing_completion_rejected() {
        let stdout = r#"{"type":"system","subtype":"init","session_id":"11111111-1111-1111-1111-111111111111"}"#;
        let result = parse(&Provider::Claude, stdout, None);
        assert!(result.is_err());
    }

    #[test]
    fn claude_mismatched_expected_rejected() {
        let stdout = r#"{"type":"result","subtype":"success","is_error":false,"session_id":"11111111-1111-1111-1111-111111111111","result":"hello"}"#;
        let result = parse(
            &Provider::Claude,
            stdout,
            Some("22222222-2222-2222-2222-222222222222"),
        );
        assert!(result.is_err());
    }

    #[test]
    fn missing_claude_usage_remains_unknown() {
        let stdout = r#"{"type":"result","subtype":"success","is_error":false,"terminal_reason":"completed","session_id":"11111111-1111-1111-1111-111111111111","result":"done","usage":null}"#;
        assert_eq!(
            parse(&Provider::Claude, stdout, None).unwrap().usage["known"],
            false
        );
    }

    #[test]
    fn codex_tool_only_yield_has_receipt_and_no_invented_text() {
        let events = [
            serde_json::json!({"type":"thread.started","thread_id":"11111111-1111-1111-1111-111111111111"}),
            serde_json::json!({"type":"item.completed","item":{"type":"mcp_tool_call","status":"completed"}}),
            serde_json::json!({"type":"item.completed","item":{"type":"agent_message","text":""}}),
            serde_json::json!({"type":"turn.completed","usage":null}),
        ];
        let text = events
            .iter()
            .map(Value::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        let parsed = parse(&Provider::Codex, &text, None).unwrap();
        assert!(parsed.output.is_empty());
        assert_eq!(parsed.usage["known"], false);
        assert!(
            parse(
                &Provider::Codex,
                &events
                    .iter()
                    .filter(|e| e["type"] != "turn.completed")
                    .map(Value::to_string)
                    .collect::<Vec<_>>()
                    .join("\n"),
                None
            )
            .is_err()
        );
    }
}
