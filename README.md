# Agentisan

**Agent teams. Your tools. Your control.**

Agentisan is a local toolkit for making groups of agent teams identifiable and inspectable
from existing CLI and Desktop clients. Its longer-term goal is bounded collaboration across
providers, with persistent work records and explicit human decisions.

**Current status: reusable native CLI teams.** The Rust service can run a Claude lead with
Codex workers and the reverse, with real peer-to-peer MCP messages and persistent native
session IDs. Live execution currently supports macOS/Linux and consultation profiles;
shell/file-editing tools are disabled. Human approval workflows, general job reconciliation,
direct model-API workers, and native deep-link opening remain planned.

Start with the [live-team guide](docs/live-teams.md) and either the
[Claude-led](examples/claude-led-team.json) or [Codex-led](examples/codex-led-team.json)
configuration. The fixture walkthrough below exercises inspection without model calls.

## What works

- Import explicit fake-adapter fixtures containing groups, teams, agents, and scoped readers.
- Inspect the same records through CLI commands and read-only MCP tools.
- Keep canonical agent IDs distinct from native session, thread, and subagent IDs.
- Preserve records and credential bindings across service restarts.
- Reject conflicting registrations; repeating an identical fixture is idempotent.
- Restrict inspection to the groups granted to the connector's credential.
- Return `unbound` when no credential or no simulated agent binding is present.
- Register reusable managed teams, submit objectives, and inspect persistent run/message history.
- Deliver real messages through MCP between the lead and workers and directly between workers.
- Bound CLI invocations, message count, elapsed time, and output; stop stalled work.
- Preserve exact native sessions between turns, with explicit resume of inspected unread work.
- Stream changed authoritative run snapshots with a bounded, read-only CLI watch command.
- Run an administrator-selected deterministic verifier against the exact proposed result and
  persist an accepted, rejected, or error receipt with result and verifier hashes.

Fixture bindings remain simulated. Managed bindings record IDs returned by configured native
CLIs. `bound` identifies the provisioned credential; it does not independently authenticate
the enclosing chat or prove that every caller holding that credential is the native process.
Fixture activity remains unobserved; managed turn activity is tracked separately.

## Build and try the fixture registry

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
    "mcp"
  ]
}
```

The enclosing configuration format depends on the client. The connector exposes `whoami`,
`groups_list`, `teams_list`, `agents_list`, `agents_inspect`, `runs_inspect`, and `messages_list`
for inspection. Managed agents additionally use `messages_receive`, `messages_send`, and
`runs_complete` during their active turn. These do not expose shell execution, administrative
registration, or human approval. Each connector has one explicitly provisioned credential;
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

The MCP connector queries the separate service. Closing the connector leaves the service
and its records available. Closing the service requires restarting it before inspection can
continue. No browser UI is required, and no URI handler is installed.

## Development and limits

```sh
cargo fmt --all -- --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --locked
```

Ordinary tests cover registry invariants, messaging/limits, CLI/HTTP/MCP parity, initialization
locking, persistence, and Unix process deadlines. They make no model calls. The separate
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
- [Live teams](docs/live-teams.md): real execution, inspection, limits, and validation.
- [Desktop inspection validation](validation/desktop-inspection.md): active-run readback and
  deterministic rejection of a flawed agent-produced bundle.
- [Roadmap](ROADMAP.md): remaining milestones and acceptance criteria.
- [Agent contribution instructions](AGENTS.md).
- [MIT license](LICENSE).
