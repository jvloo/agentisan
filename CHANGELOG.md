# Changelog

## Unreleased

- Added core-v2 durable turn protocol: schema version 5, short-lived per-turn lease
  credentials, ownership epochs, stable inbox reads, staged sends, and atomic `turn_commit`.
- Added role-scoped agent and observer MCP profiles and kept connector/admin operations outside
  the model-facing surface.
- Persisted lead result proposals independently of native process completion so they remain
  inspectable and independently verifiable after failure or restart.
- Added scheduler fail-closed health behavior and recovery of abandoned verifier reservations.
- Clarified migration compatibility and current validation limits, including the terminal-only
  watcher assertion.
