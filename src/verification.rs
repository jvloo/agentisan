use crate::{
    fixture::private_dir,
    registry::{Registry, RegistryError},
    teams,
};
use anyhow::{Context, Result, bail};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::Row;
use std::path::{Path, PathBuf};

pub const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS verifications (
  id TEXT PRIMARY KEY,
  run_id TEXT NOT NULL REFERENCES runs(id),
  status TEXT NOT NULL CHECK(status IN ('running','accepted','rejected','error')),
  verifier_path TEXT NOT NULL,
  verifier_sha256 TEXT NOT NULL,
  result_sha256 TEXT NOT NULL,
  started_at INTEGER NOT NULL,
  ended_at INTEGER,
  exit_code INTEGER,
  summary TEXT,
  evidence TEXT,
  artifacts TEXT
);
CREATE UNIQUE INDEX IF NOT EXISTS one_active_or_terminal_verification
ON verifications(run_id) WHERE status IN ('running','accepted','rejected');
PRAGMA user_version=4;
"#;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct VerifierOutput {
    accepted: bool,
    summary: String,
    #[serde(default)]
    evidence: Value,
}

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn valid_run_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

pub async fn inspect(
    registry: &Registry,
    run_id: &str,
) -> std::result::Result<Value, RegistryError> {
    let row = sqlx::query(
        "SELECT status,verifier_path,verifier_sha256,result_sha256,started_at,ended_at,exit_code,summary,evidence,artifacts FROM verifications WHERE run_id=? ORDER BY rowid DESC LIMIT 1",
    )
    .bind(run_id)
    .fetch_optional(&registry.pool)
    .await?;
    let Some(row) = row else {
        return Ok(json!({"state":"not_independently_verified"}));
    };
    let status: String = row.get("status");
    Ok(json!({
        "state": status,
        "verifier": {
            "path": row.get::<String,_>("verifier_path"),
            "sha256": row.get::<String,_>("verifier_sha256")
        },
        "result_sha256": row.get::<String,_>("result_sha256"),
        "started_at": row.get::<i64,_>("started_at"),
        "ended_at": row.get::<Option<i64>,_>("ended_at"),
        "exit_code": row.get::<Option<i64>,_>("exit_code"),
        "summary": row.get::<Option<String>,_>("summary"),
        "evidence": row.get::<Option<String>,_>("evidence").and_then(|v| serde_json::from_str::<Value>(&v).ok()),
        "artifacts": row.get::<Option<String>,_>("artifacts")
    }))
}

async fn reserve(
    registry: &Registry,
    run_id: &str,
    verifier: &Path,
    artifacts: &Path,
) -> Result<(String, String, PathBuf, String)> {
    let verifier = verifier
        .canonicalize()
        .context("cannot resolve verifier executable")?;
    if !verifier.is_file() {
        bail!("verifier must be an existing absolute executable file");
    }
    let verifier_hash = digest(&std::fs::read(&verifier)?);
    let mut tx = registry.pool.begin_with("BEGIN IMMEDIATE").await?;
    let row = sqlx::query("SELECT r.state,p.result FROM runs r LEFT JOIN run_proposals p ON p.run_id=r.id WHERE r.id=?")
        .bind(run_id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| anyhow::anyhow!("run not found"))?;
    if matches!(
        row.get::<String, _>("state").as_str(),
        "queued" | "running" | "completing"
    ) {
        bail!("an agent proposal can be verified only after its native turn settles");
    }
    let result = row
        .get::<Option<String>, _>("result")
        .ok_or_else(|| anyhow::anyhow!("run has no durable proposed result"))?;
    let id = teams::new_id("verification");
    sqlx::query("INSERT INTO verifications(id,run_id,status,verifier_path,verifier_sha256,result_sha256,started_at,artifacts) VALUES(?,?,'running',?,?,?,?,?)")
        .bind(&id)
        .bind(run_id)
        .bind(verifier.to_string_lossy().as_ref())
        .bind(&verifier_hash)
        .bind(digest(result.as_bytes()))
        .bind(teams::now())
        .bind(artifacts.to_string_lossy().as_ref())
        .execute(&mut *tx)
        .await
        .context("run already has an active or terminal verification")?;
    tx.commit().await?;
    Ok((id, result, verifier, verifier_hash))
}

async fn record_error(registry: &Registry, id: &str, message: &str) -> Result<()> {
    sqlx::query("UPDATE verifications SET status='error',ended_at=?,summary=? WHERE id=? AND status='running'")
        .bind(teams::now())
        .bind(message.chars().take(4096).collect::<String>())
        .bind(id)
        .execute(&registry.pool)
        .await?;
    Ok(())
}

/// A verifier has no durable completion receipt until `record_result` (or
/// `record_error`) commits. Normal CLI verification holds `admin.lock` for the
/// whole invocation, so after a crashed verifier releases that lock the next
/// invocation can safely fence its abandoned reservation before starting work.
///
/// This deliberately is not called by the HTTP service: a service start cannot
/// prove that a separately launched administrative verifier is no longer alive.
async fn recover_abandoned_reservations(registry: &Registry) -> Result<()> {
    sqlx::query(
        "UPDATE verifications SET status='error',ended_at=?,summary=? WHERE status='running'",
    )
    .bind(teams::now())
    .bind(
        "verification process ended before a completion receipt; inspect artifacts before retrying",
    )
    .execute(&registry.pool)
    .await?;
    Ok(())
}

async fn record_result(registry: &Registry, id: &str, output: VerifierOutput) -> Result<Value> {
    if output.summary.trim().is_empty() || output.summary.len() > 4096 {
        bail!("verifier summary must contain 1 to 4096 bytes");
    }
    let evidence = serde_json::to_string(&output.evidence)?;
    if evidence.len() > 32_768 {
        bail!("verifier evidence exceeds 32768 bytes");
    }
    let status = if output.accepted {
        "accepted"
    } else {
        "rejected"
    };
    let mut tx = registry.pool.begin_with("BEGIN IMMEDIATE").await?;
    let changed = sqlx::query("UPDATE verifications SET status=?,ended_at=?,exit_code=0,summary=?,evidence=? WHERE id=? AND status='running'")
        .bind(status)
        .bind(teams::now())
        .bind(&output.summary)
        .bind(&evidence)
        .bind(id)
        .execute(&mut *tx)
        .await?
        .rows_affected();
    if changed != 1 {
        bail!("verification reservation is no longer active");
    }
    tx.commit().await?;
    Ok(json!({"status":status,"summary":output.summary,"evidence":output.evidence}))
}

pub async fn verify(
    registry: &Registry,
    data_dir: &Path,
    run_id: &str,
    verifier: &Path,
    timeout_seconds: u64,
) -> Result<Value> {
    if !valid_run_id(run_id) || !verifier.is_absolute() || !(1..=300).contains(&timeout_seconds) {
        bail!("verification requires an absolute executable and a 1 to 300 second deadline");
    }
    // Serialize the public CLI path. This makes a released lock evidence that a
    // previous CLI verifier cannot still own its reservation.
    let _admin_lock = crate::fixture::admin_lock(data_dir)?;
    recover_abandoned_reservations(registry).await?;
    let attempt = teams::new_id("verification");
    let artifacts = data_dir.join("runs").join(run_id).join(&attempt);
    private_dir(&artifacts)?;
    let (id, result, verifier, verifier_hash) =
        reserve(registry, run_id, verifier, &artifacts).await?;
    let outcome = run_verifier(&verifier, result.as_bytes(), timeout_seconds, &artifacts)
        .await
        .and_then(|output| {
            let current_hash = digest(&std::fs::read(&verifier)?);
            if current_hash != verifier_hash {
                bail!("verifier executable changed during verification");
            }
            Ok(output)
        });
    match outcome {
        Ok(output) => match record_result(registry, &id, output).await {
            Ok(value) => Ok(value),
            Err(error) => {
                record_error(registry, &id, &error.to_string()).await?;
                Err(error)
            }
        },
        Err(error) => {
            record_error(registry, &id, &error.to_string()).await?;
            Err(error)
        }
    }
}

#[cfg(not(unix))]
async fn run_verifier(
    _verifier: &Path,
    _input: &[u8],
    _timeout_seconds: u64,
    _artifacts: &Path,
) -> Result<VerifierOutput> {
    bail!("bounded verifier execution currently requires macOS or Linux")
}

#[cfg(unix)]
async fn run_verifier(
    verifier: &Path,
    input: &[u8],
    timeout_seconds: u64,
    artifacts: &Path,
) -> Result<VerifierOutput> {
    use nix::{
        sys::signal::{Signal, killpg},
        unistd::Pid,
    };
    use std::{
        fs::OpenOptions,
        os::unix::{fs::OpenOptionsExt, process::CommandExt},
        process::Stdio,
        sync::Arc,
        time::{Duration, Instant},
    };
    use tokio::{io::AsyncWriteExt, process::Command};

    fn output_file(path: &Path) -> Result<std::fs::File> {
        Ok(OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)?)
    }

    struct ProcessGroup(Option<i32>);
    impl ProcessGroup {
        fn kill(&mut self) {
            if let Some(pid) = self.0.take() {
                let _ = killpg(Pid::from_raw(pid), Signal::SIGKILL);
            }
        }
    }
    impl Drop for ProcessGroup {
        fn drop(&mut self) {
            self.kill();
        }
    }

    let stdout_path = artifacts.join("stdout.json");
    let stderr_path = artifacts.join("stderr.txt");
    let exit_path = artifacts.join("native-exit.json");
    let stdout_file = output_file(&stdout_path)?;
    let stderr_file = output_file(&stderr_path)?;
    let deadline = Instant::now() + Duration::from_secs(timeout_seconds);
    let host = std::env::current_exe()?;
    let mut command = Command::new(host);
    command
        .arg("worker-host")
        .arg("--exit-file")
        .arg(&exit_path)
        .arg("--timeout-ms")
        .arg((timeout_seconds * 1000).to_string())
        .arg("--")
        .arg(verifier)
        .current_dir(artifacts)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("LANG", "C")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    command.as_std_mut().process_group(0);
    let mut child = command
        .spawn()
        .context("cannot start verifier supervisor")?;
    let pid = child
        .id()
        .ok_or_else(|| anyhow::anyhow!("missing verifier process ID"))? as i32;
    let mut group = ProcessGroup(Some(pid));
    let capture_state = Arc::new(crate::capture::CaptureState::new(65_536));
    let capture = tokio::spawn(crate::capture::capture(
        child
            .stdout
            .take()
            .ok_or_else(|| anyhow::anyhow!("missing verifier stdout"))?,
        child
            .stderr
            .take()
            .ok_or_else(|| anyhow::anyhow!("missing verifier stderr"))?,
        tokio::fs::File::from_std(stdout_file),
        tokio::fs::File::from_std(stderr_file),
        capture_state.clone(),
    ));
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| anyhow::anyhow!("missing verifier stdin"))?;
    tokio::time::timeout(deadline.saturating_duration_since(Instant::now()), async {
        stdin.write_all(input).await?;
        stdin.shutdown().await
    })
    .await
    .context("verifier stdin deadline reached")??;
    drop(stdin);

    let receipt = loop {
        if capture_state.truncated() {
            break Err(anyhow::anyhow!("verifier output exceeded 64 KiB"));
        }
        if exit_path.is_file() {
            let value: Value = serde_json::from_slice(&std::fs::read(&exit_path)?)?;
            break Ok(value);
        }
        if Instant::now() >= deadline {
            break Err(anyhow::anyhow!("verifier deadline reached"));
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    };

    let _ = killpg(Pid::from_raw(pid), Signal::SIGTERM);
    tokio::time::sleep(Duration::from_millis(100)).await;
    group.kill();
    let _ = tokio::time::timeout(Duration::from_secs(1), child.wait()).await;
    tokio::time::timeout(Duration::from_secs(1), capture)
        .await
        .context("verifier output capture did not finish")???;

    let receipt = receipt?;
    if receipt["success"] != true {
        bail!("verifier exited unsuccessfully");
    }
    let stdout = std::fs::read(&stdout_path)?;
    let output: VerifierOutput =
        serde_json::from_slice(&stdout).context("verifier stdout must be one JSON result")?;
    if output.summary.trim().is_empty() || output.summary.len() > 4096 {
        bail!("verifier summary must contain 1 to 4096 bytes");
    }
    if serde_json::to_vec(&output.evidence)?.len() > 32_768 {
        bail!("verifier evidence exceeds 32 KiB");
    }
    Ok(output)
}
