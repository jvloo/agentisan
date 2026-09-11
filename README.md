# Agentisan

**Craft agents into a team.**

Agentisan is a local Rust runtime and MCP server for durable, bounded agent collaboration. Its
coordination model is provider-agnostic: Agentisan owns canonical identity, assignments, messages,
turn commits, human decisions, recovery state, and result verification instead of delegating those
contracts to a model provider.

**Current adapters: Claude Code and Codex CLI.** Core-v2 runs a Claude lead with Codex workers or a
Codex lead with Claude workers on macOS/Linux. One service-owned lead delegates to multiple workers,
and workers exchange scoped messages directly. Existing CLIs retain their native sessions;
Agentisan's CLI and observer MCP profile inspect authoritative runtime state; completed Codex
sessions remain readable in Codex Desktop. The current worker profile supports consultation,
planning, and review, with shell commands, file editing, native delegation, external apps, and
unrelated MCP tools disabled.

Start with the [live-team guide](docs/live-teams.md) and either the
[Claude-led](examples/claude-led-team.json) or [Codex-led](examples/codex-led-team.json)
configuration. The fixture walkthrough below exercises inspection without model calls.

## Shipped in core-v2

| Capability | Current behavior |
|---|---|
| Provider-agnostic coordination | Agentisan owns team identity, work, communication, commit, recovery, and acceptance records independently of provider-native session formats. |
| Current provider adapters | A Claude lead can coordinate Codex workers, or a Codex lead can coordinate Claude workers. Workers can message each other through MCP. |
| Durable turns | Short-lived credentials bind every mutation to one agent, run, turn, and ownership epoch. The first inbox read persists a stable snapshot; `turn_commit` atomically acknowledges inputs and publishes staged work. |
| Bounded delegation | Assignments reserve turn and message slices inside immutable run limits. Exhausted assignments do not starve independent work, and inspected extensions cannot enlarge the root budget. |
| Human decisions | Agents can request a choice against exact scope and artifact hashes. Only the trusted local CLI can resolve or invalidate it; a blocking request pauses its dependent assignment. |
| Recovery | Restarted in-flight work becomes explicit `unknown` state. No-effect reconciliation discards uncommitted staging, preserves already-committed effects, and requires an inspected resume. |
| Inspection and acceptance | Running `agentisan` opens a live, read-only terminal dashboard. CLI and observer MCP retain credential-scoped machine interfaces. Result proposals survive native failure and remain separate from deterministic verification and human acceptance. |

The latest live acceptance used Claude Sonnet and Codex Luna at low effort in both lead directions.
Each topology completed five native turns, two assignments, eleven persistent messages, all six
required lead/worker/peer routes, and three distinct durable native sessions. See the
[sanitized validation report](validation/live-teams.md).

## Claims Agentisan does not make

- It does not guarantee exactly-once behavior for arbitrary external side effects.
- Run, turn, and assignment limits are not provider token or billing caps.
- Current managed workers cannot execute shell commands or edit project files.
- Codex Desktop is a verified transcript inspection surface, not an authenticated active-writer
  adapter; Claude Desktop integration is not shipped.
- Additional provider adapters, direct model APIs, Windows CLI supervision, native approval UI,
  URI/deep-link handlers, concurrent native turns, and general effect reconciliation are not shipped.

## Run a real team

Build Agentisan, then choose the [Claude-led](examples/claude-led-team.json) or
[Codex-led](examples/codex-led-team.json) template:

```sh
cargo build --locked
```

The [live-team guide](docs/live-teams.md#create-and-run-a-team) is the canonical runbook for
private configuration, account authentication, `--live` authorization, bounded submission,
inspection, model selection, and recovery.

## View the team

Run Agentisan without a subcommand to open the terminal dashboard against `.agentisan`:

```sh
agentisan
```

Use another state directory or select an exact run at startup:

```sh
agentisan --data-dir /absolute/path/to/state
agentisan --data-dir /absolute/path/to/state dashboard RUN_ID
```

The dashboard refreshes automatically and combines runs, agents, assignments, decisions, budgets,
and the lead/worker/peer message timeline. Use Up/Down to change runs, click an agent or press Tab to
filter its communication, `r` to refresh, and `q` to quit. It reads SQLite as the trusted local OS
administrator and never takes ownership of a native agent session. It opens only the current
schema in SQLite query-only mode: it never creates or migrates a registry. Start the normal service
once to perform an explicit upgrade before viewing older state.

For a released terminal run, select an agent and press `o`, or use:

```sh
agentisan --data-dir /absolute/path/to/state open RUN_ID AGENT_ID
```

Agentisan resumes the exact recorded Claude Code or Codex CLI session. It refuses while a run is
queued, active, stalled, or interrupted, preventing a competing writer. Codex Desktop does not yet
publish an external exact-thread opening contract. Claude CLI sessions can be transferred with
`/desktop` when appropriate, but Claude Desktop maintains separate history. The `open` command
reports these capability limits instead of guessing.

All existing subcommands remain available for scripts, CI, MCP hosts, and detailed administration.

## Try inspection without model calls

Fixture bindings are simulated. Managed bindings record IDs returned by configured native CLIs.
`bound` identifies the provisioned credential; it does not independently authenticate the
enclosing chat or prove that every caller holding that credential is the native process. Fixture
activity remains unobserved; managed turn activity is tracked separately.

Install Rust through [rustup](https://rust-lang.org/tools/install/). The repository pins the
toolchain in [rust-toolchain.toml](rust-toolchain.toml); dependencies are locked. Build from
the repository root:

```sh
cargo build --locked
./target/debug/agentisan init --fixture examples/registry.json
./target/debug/agentisan serve
```

`init` is an explicit local administrative operation. It creates `.agentisan/registry.sqlite3`
and random credential files under `.agentisan/credentials/`. It prints their paths, never
the credentials. The state directory is ignored by Git. Repeating the same import preserves
existing credentials; conflicting definitions are rejected atomically.

With the service running, use another terminal:

```sh
./target/debug/agentisan whoami
./target/debug/agentisan --credential-file .agentisan/credentials/inventory_reader.token whoami
./target/debug/agentisan --credential-file .agentisan/credentials/inventory_reader.token groups list
./target/debug/agentisan --credential-file .agentisan/credentials/inventory_reader.token teams list --group inventory
./target/debug/agentisan --credential-file .agentisan/credentials/inventory_reader.token agents inspect inventory_worker
```

The inventory reader can inspect `inventory_worker` but cannot inspect `support_lead`.
The `observer` credential has inventory read access without a simulated agent binding.
An invalid credential is an error; it is never silently downgraded to anonymous access.

Use `--data-dir` on `init` and `serve` for another state directory. The service defaults to
`127.0.0.1:7437`; `serve --listen 127.0.0.1:0` chooses a free port and prints the actual
address in a JSON readiness line. Clients use `--endpoint http://127.0.0.1:PORT` for that address.
Only numeric loopback addresses are supported. Windows executables have an `.exe` suffix.

## Connect an existing MCP client

Start `agentisan serve` separately. Configure a stdio MCP server whose command is the
absolute path to the built `agentisan` executable, using arguments shaped like:

```json
{
  "command": "/absolute/path/to/agentisan",
  "args": [
    "--endpoint", "http://127.0.0.1:7437",
    "--credential-file", "/absolute/path/to/inventory_reader.token",
    "mcp", "--profile", "observer"
  ]
}
```

The enclosing configuration format depends on the client. The observer profile exposes `whoami`,
`groups_list`, `teams_list`, `agents_list`, `agents_inspect`, `runs_inspect`, and `messages_list`
for inspection. The agent profile exposes `agent_context_get`, `inbox_read`, `message_send`,
`assignment_update`, `decision_request`, `turn_commit`, and, for leads, `assignment_create` and
`result_propose` during an active, short-lived turn lease. These
profiles do not expose shell execution, administrative registration, or human approval. Each
connector has one explicitly provisioned credential;
sharing it across conversations shares access and does not identify those conversations.

Agentisan's runtime state is authoritative while it owns a managed turn. A native Desktop client
may render an externally driven CLI session as interrupted even while Agentisan records it as
running. Use `runs inspect` or the bounded watcher for liveness; use native history to inspect the
conversation and MCP calls:

```sh
./target/debug/agentisan --credential-file /absolute/path/to/lead.token \
  runs watch RUN_ID --interval-ms 500 --timeout-seconds 300
```

An agent-proposed result is execution-complete but remains `not_independently_verified`. A trusted
local administrator can select a deterministic verifier executable. Agentisan sends the exact
result bytes on stdin; the verifier must exit successfully and emit one JSON object shaped as
`{"accepted":true|false,"summary":"...","evidence":...}`. No shell is used, and agents cannot
invoke this command through MCP:

```sh
./target/debug/agentisan --data-dir /absolute/path/to/state runs verify RUN_ID \
  --verifier /absolute/path/to/verifier --timeout-seconds 30
```

The verifier executable is trusted local code. The receipt records hashes of the verifier file and
proposed result. A malformed, failed, oversized, or timed-out verifier is recorded as `error` and
does not become acceptance; an accepted or rejected receipt is terminal for that run.

Trusted local recovery and human-decision commands stay outside model-facing MCP:

```sh
./target/debug/agentisan assignments inspect ASSIGNMENT_ID
./target/debug/agentisan assignments extend ASSIGNMENT_ID \
  --add-turns 1 --deadline-seconds 120 --after-inspection
./target/debug/agentisan decisions inspect DECISION_ID
./target/debug/agentisan decisions resolve DECISION_ID \
  --scope-hash SHA256 --artifact-hash SHA256 --choice approve
./target/debug/agentisan runs reconcile TURN_ID --no-effect --after-inspection
./target/debug/agentisan runs resume RUN_ID --after-inspection
```

Assignment extension reallocates capacity inside the original run limits. No-effect reconciliation
requires inspecting the native evidence first; it preserves effects already published by a committed
turn and discards only private staging from an uncommitted turn.

The MCP connector queries the separate service. Closing the connector leaves the service
and its records available. Closing the service requires restarting it before inspection can
continue. No browser UI is required, and no URI handler is installed.

## Development and limits

```sh
cargo fmt --all -- --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --locked
```

Ordinary tests cover query-only dashboard access, scrolled mouse selection, exact historical-run
lookup, native-open safety, registry invariants, messaging/limits, CLI/HTTP/MCP parity,
initialization locking, persistence, and Unix process deadlines. They make no model calls. The separate
[opt-in live test](docs/live-teams.md#repeat-the-live-acceptance-test) runs both provider
topologies and verifies actual message routes and native session continuity. CI runs the
ordinary checks on Linux, macOS, and Windows; Windows CLI execution is not supported yet.

This is a local development milestone. The OS account controlling the data directory is
trusted and can administer every fixture identity. Credential-based group filtering is not
isolation against that account. Keep credentials and database files private; do not expose
the service through a proxy or tunnel. See the [Milestone 1 contract](docs/milestone-1.md).

## Design and roadmap

- [Architecture](docs/architecture.md): the intended team runtime and its boundaries.
- [Rust decision](docs/decisions/0001-rust-core.md): stack choice and deferred decisions.
- [Durable protocol decision](docs/decisions/0002-durable-agent-protocol.md): leases, atomic
  publication, recovery, and trust boundaries.
- [Live teams](docs/live-teams.md): real execution, inspection, limits, and validation.
- [Desktop inspection validation](validation/desktop-inspection.md): active-run readback and
  deterministic rejection of a flawed agent-produced bundle.
- [Roadmap](ROADMAP.md): remaining milestones and acceptance criteria.
- [Changelog](CHANGELOG.md): shipped core-v2 behavior.
- [Agent contribution instructions](AGENTS.md).
- [MIT license](LICENSE).
