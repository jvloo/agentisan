# Changelog

## Unreleased

- Added core-v2 durable turn protocol: schema version 7, short-lived per-turn lease
  credentials, ownership epochs, complete stable inbox snapshots, staged sends, and atomic
  `turn_commit`.
- Added role-scoped agent and observer MCP profiles and kept connector/admin operations outside
  the model-facing surface.
- Persisted lead result proposals independently of native process completion so they remain
  inspectable and independently verifiable after failure or restart.
- Added scheduler fail-closed health behavior and recovery of abandoned verifier reservations.
- Added first-class assignments with reserved turn/message slices and explicit lifecycle updates.
- Derive assignment scope hashes from canonical objective and completion criteria instead of
  accepting model-selected revision identifiers.
- Added starvation-free scheduling around exhausted assignments and inspected assignment extension
  that reallocates only capacity remaining inside the original run limits.
- Added scoped human decision requests with trusted local resolution/invalidation; blocking decisions
  pause only their dependent worker.
- Added inspected no-effect reconciliation for unknown turns and durable proposal verification after
  native failure.
- Preserve already-published effects when reconciling a committed turn interrupted before its
  native completion receipt.
- Clarified migration compatibility and current validation limits, including the terminal-only
  watcher assertion.
