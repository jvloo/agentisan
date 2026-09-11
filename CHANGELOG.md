# Changelog

## Unreleased

- Added interactive v3 Phase 1 with schema version 9 and an `operator` MCP profile. A host chat can
  dispatch initial work directly to configured workers, observe committed peer messages, accept
  worker reports, finish the run, or request cancellation without launching the configured model lead.
- Added per-run control handles, connector leases, coordination epochs, optimistic versions, and
  idempotent controller mutation receipts. Read-only status calls never renew ownership.
- Live-tested one controller with two low-effort Claude Sonnet workers: three worker turns, both peer
  directions, two reports, two native sessions, and zero configured-lead turns.
- Added a controller MCP profile for Codex Desktop and other MCP planning chats. A managed lead
  credential can start and inspect its one service-owned team without claiming the host chat as the
  native lead session.
- Added atomic, idempotent controller run starts in schema version 8, with explicit live-provider
  authorization and a worker-scheduler health fence.
- Added `agentisan run TEAM --objective "..." --live` as the concise CLI start path while retaining
  the file-oriented `teams run` command.
- Added a default live terminal dashboard with clickable agent filtering, automatic refresh, and
  combined run, assignment, decision, budget, and message-route views.
- Added safe exact-session opening for released Claude Code and Codex CLI agents; active and
  unsupported Desktop targets fail closed.
- Native chats opened from the dashboard now return to the same refreshed dashboard on exit.
- Added `dashboard RUN_ID --agent AGENT_ID` for pre-filtered per-agent terminal views.
- Added `agentisan dashboard [RUN_ID]` and `agentisan open RUN_ID AGENT_ID`; existing subcommands
  remain available for automation.
- Added core-v2 durable turn protocol: schema version 7, short-lived per-turn lease
  credentials, ownership epochs, complete stable inbox snapshots, staged sends, and atomic
  `turn_commit`.
- Added role-scoped agent, observer, and controller MCP profiles. Administrative operations remain
  outside the model-facing surface.
- Persisted lead result proposals independently of native process completion so they remain
  inspectable and independently verifiable after failure or restart.
- Added scheduler fail-closed health behavior and recovery of abandoned verifier reservations.
- Added first-class assignments with reserved turn/message slices and explicit lifecycle updates.
- Derived assignment scope hashes from canonical objective and completion criteria instead of
  accepting model-selected revision identifiers.
- Added starvation-free scheduling around exhausted assignments and inspected assignment extension
  that reallocates only capacity remaining inside the original run limits.
- Added scoped human decision requests with trusted local resolution/invalidation; blocking decisions
  pause only their dependent worker.
- Added inspected no-effect reconciliation for unknown turns and durable proposal verification after
  native failure.
- Preserved already-published effects when reconciling a committed turn interrupted before its
  native completion receipt.
- Refreshed product claims and live-validation evidence for the final core-v2 implementation:
  81 ordinary tests, both autonomous Sonnet/Luna topologies, and the interactive Claude-worker topology.
- Clarified the provider-agnostic coordination model separately from the currently shipped Claude
  Code and Codex CLI adapters.
- Clarified migration compatibility and current validation limits, including the terminal-only
  watcher assertion.
