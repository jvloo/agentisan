# ADR 0001: Rust core with shared CLI and MCP inspection

Status: accepted for Milestone 1.

## Decision

Use Rust for the local service, registry, CLI, and MCP connector. A single Cargo package
shares typed domain records and inspection behavior. CLI and stdio MCP requests reach one
loopback service; the connector does not own persistence or the daemon lifecycle.

The first implementation uses Tokio, clap, official RMCP, SQLx with SQLite, and a small
Axum/reqwest HTTP boundary. Cargo.lock pins dependencies; rust-toolchain.toml pins the
tested compiler and developer tools. No agent framework or durable job engine is selected.

## Reasons and alternatives

Agentisan needs explicit identities, predictable background operation, and a distributable
CLI. Rust's types help distinguish canonical and native IDs, while one native executable
keeps the core installation independent of Python and Node runtimes. Python or TypeScript
remain reasonable adapter languages and offer different iteration and integration tradeoffs.

RMCP avoids reimplementing MCP framing and protocol negotiation. SQLx/SQLite supplies local
transactions and persistent records. Ordinary query routing and authorization require no
model call. These components are sufficient for fixture inspection; they are not substitutes
for durable execution of effectful jobs.

Before Milestone 2, evaluate existing durable execution components against interruption,
idempotency, ownership, and human-decision requirements. Rust's memory safety does not prove
those properties. A framework or separate workflow service can sit behind a defined adapter
if it meets the requirements without forcing the rest of the system to change language.

## Consequences

- Async Rust, cross-platform packaging, and OS-specific execution controls need maintenance.
- Provider APIs and native app interfaces remain behind capability-aware adapters.
- Token effectiveness depends on context, delegation and acceptance behavior, not Rust speed.
- A local HTTP boundary simplifies existing-client integration but requires explicit
  credentials and a documented trust boundary. Remote hosting and multi-user hardening are
  deferred.
- No separate browser UI or deep-link registration is needed for the first milestone.

## Sources

- [Official Rust MCP SDK](https://github.com/modelcontextprotocol/rust-sdk)
- [SQLx](https://docs.rs/sqlx/latest/sqlx/)
- [Tokio shutdown guidance](https://tokio.rs/tokio/topics/shutdown)
- [Agentisan architecture](../architecture.md)
