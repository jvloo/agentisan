# Agentisan

**Agent teams. Your tools. Your control.**

Agentisan is a local toolkit for making groups of agent teams identifiable and inspectable
from existing CLI and Desktop clients. Its longer-term goal is bounded collaboration across
providers, with persistent work records and explicit human decisions.

**Current status: Milestone 1, simulated-agent registry.** The Rust CLI, local inspection
service, SQLite persistence, and stdio MCP connector are implemented. Worker execution,
native session discovery, agent messaging, budgets, approvals, and native deep links remain
planned. No model API credentials or paid model calls are needed for this milestone.

## What works

- Import explicit fake-adapter fixtures containing groups, teams, agents, and scoped readers.
- Inspect the same records through CLI commands and five read-only MCP tools.
- Keep canonical agent IDs distinct from native session, thread, and subagent IDs.
- Preserve records and credential bindings across service restarts.
- Reject conflicting registrations; repeating an identical fixture is idempotent.
- Restrict inspection to the groups granted to the connector's credential.
- Return `unbound` when no credential or no simulated agent binding is present.

All bindings currently come from a fixture. A `bound` result **does not prove that a real
Claude, Codex, or other native conversation is the caller**. Connection and activity are
reported as unverified and unobserved; the example agents are not live processes.

## Build and try

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
`groups_list`, `teams_list`, `agents_list`, and `agents_inspect`. There are no mutation or
execution tools. Each connector has one explicitly provisioned credential; sharing that
connector across conversations shares its access and does not identify those conversations.

The MCP connector queries the separate service. Closing the connector leaves the service
and its records available. Closing the service requires restarting it before inspection can
continue. No browser UI is required, and no URI handler is installed.

## Development and limits

```sh
cargo fmt --all -- --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --locked
```

Tests cover registry invariants and the actual CLI, HTTP, and MCP stdio interfaces using
temporary data. They exercise caller isolation, conflicting imports, connector exit, and
service restart. They do not establish live-provider, execution-sandbox, or job-recovery
guarantees. CI runs the checks on Linux, macOS, and Windows.

This is a local development milestone. The OS account controlling the data directory is
trusted and can administer every fixture identity. Credential-based group filtering is not
isolation against that account. Keep credentials and database files private; do not expose
the service through a proxy or tunnel. See the [Milestone 1 contract](docs/milestone-1.md).

## Design and roadmap

- [Architecture](docs/architecture.md): the intended team runtime and its boundaries.
- [Rust decision](docs/decisions/0001-rust-core.md): stack choice and deferred decisions.
- [Roadmap](ROADMAP.md): remaining milestones and acceptance criteria.
- [Agent contribution instructions](AGENTS.md).
- [MIT license](LICENSE).
