# Roadmap

Milestones 0 and 1 are implemented. Later milestones remain proposed. See [docs/architecture.md](docs/architecture.md) for the intended runtime, [docs/milestone-1.md](docs/milestone-1.md) for the current implementation boundary, and [README.md](README.md) for runnable commands.

## Milestone 0 — Design bootstrap

**Status: present.** This repository's README, architecture doc, and roadmap exist and describe the intended system.

Acceptance:
- README, architecture, and roadmap documents committed and internally consistent.
- The initial bootstrap distinguished design intent from implemented behavior.

## Milestone 1 — Registry and read-only inspection (fake adapters)

**Status: implemented.** Rust CLI, SQLite registry, local inspection service, and stdio MCP connector. Automated tests use simulated bindings and actual CLI/MCP subprocesses; real session discovery and worker execution are not included.

Goal: prove the registry, ID scheme, and inspection surface work end-to-end against simulated agents, with no real provider calls.

Acceptance:
- CLI commands `agentisan whoami`, `agentisan teams list`, `agentisan agents inspect <agent-id>` return correct, read-only results against a fake adapter.
- MCP tools expose the same inspection results as the CLI, including exact record IDs and explicit capability limits.
- Registry correctly marks a binding **unbound** when no trusted per-call or host identity is available, and never infers the newest matching session as a substitute.
- Tests demonstrate: repeated/duplicate agent registration is idempotent; two distinct callers with similar native identifiers are not conflated (caller isolation).

## Milestone 2 — Durable single worker and trusted human decisions

Goal: one bounded worker job can complete, recover, or report uncertain effects after a crash, and a human decision can gate it.

Acceptance:
- Killing the service process around dispatch and restarting it preserves the operation identity and reconciles known results; unresolved external effects stay unknown and require inspection before another attempt.
- A human decision (approve/reject) is recorded with evidence, scope, and resolution, and only a trusted approval path is accepted as consent.
- Tests demonstrate duplicate request deduplication, lost-ack/reconnect recovery with an idempotent test executor, preservation of uncertain side effects, stale-owner rejection, rejection of stale approval after a material change, and human rejection blocking only dependent work. Fake-executor tests do not establish exactly-once external effects.

## Milestone 3 — Real providers with verified containment and accounting

Goal: connect at least two real model providers as workers, with enforced budgets and isolated execution.

Acceptance:
- Two independent providers can run worker jobs under the same root objective, each with its own credentials/billing.
- Budget reservations are atomic: a concurrency test shows simultaneous service-admitted calls cannot reserve more than the shared allowance. Provider billing and usage coverage are reported separately, including unmetered client-owned coordinator activity.
- Isolated writer workspace and constrained tool execution are enforced for at least one code-executing worker; a worktree alone is documented as insufficient and not relied upon.
- Tests demonstrate: budget race with unknown/missing usage keeps the reservation held rather than releasing it; cancellation is distinguished from confirmed stop and leaves a receipt; a no-progress worker is stopped by stagnation checks rather than running indefinitely.
- A short written comparison of total cost and human effort per accepted outcome across matched controller settings is produced, distinct from unit-test results, to avoid treating fake-adapter tests as proof of live behavior.

## Milestone 4 — 1-to-N peer collaboration and existing-client links

Goal: a main agent driven from an existing CLI/Desktop client can delegate to multiple workers that exchange bounded peer messages, with native links back into supported clients.

Acceptance:
- A main agent in at least one existing CLI or Desktop client delegates to 2+ workers; workers exchange scoped peer messages without becoming a coordinator themselves.
- CLI and MCP inspection work without a separate browser UI. Where an installed URI handler and native adapter support it, an `agentisan://agents/<agent-id>` link opens the verified native session for inspection. Opening a link does not resume execution, send a message, or approve work; unsupported native links are explicitly reported.
- Tests demonstrate: stale-ownership rejection when a second main agent attempts to claim an already-owned objective.

## Milestone 5 — Multi-team policy and service-owned coordinator (later)

Goal: support policy scoped across multiple teams in a group, and an optional service-hosted coordinator that can continue independently while its host is available.

Acceptance:
- Group-level policy (e.g., budget or permission limits) is enforced consistently across more than one team without requiring an always-connected client.
- A service-owned coordinator can resume driving an objective after the originating client disconnects, under explicit ownership transfer, with stale-owner rejection still enforced.
- This milestone remains exploratory until Milestones 1–4 are complete; its scope may change based on findings from earlier milestones.
