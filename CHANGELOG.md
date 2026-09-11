# Changelog

## Unreleased

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
- Added role-scoped agent and observer MCP profiles and kept connector/admin operations outside
  the model-facing surface.
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
  62 ordinary tests and both Sonnet/Luna low-effort provider topologies.
- Clarified the provider-agnostic coordination model separately from the currently shipped Claude
  Code and Codex CLI adapters.
- Clarified migration compatibility and current validation limits, including the terminal-only
  watcher assertion.
