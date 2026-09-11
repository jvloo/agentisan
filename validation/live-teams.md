# Live Claude/Codex team acceptance

Validated on macOS on 2026-09-11 (Asia/Kuala_Lumpur), using the schema-v7 core-v2 runtime and
`tests/live_teams.rs`. The test was explicitly enabled; normal CI does not call models. Sanitized
results are in [live-teams-results.json](live-teams-results.json).

| Configuration | Profile | Native turns | Persistent messages | Assignments | Verified routes | Native sessions |
|---|---|---:|---:|---:|---:|---:|
| Claude lead, two Codex workers | Sonnet + Luna, low | 5 | 11 | 2 | 6 | 3 |
| Codex lead, two Claude workers | Luna + Sonnet, low | 5 | 11 | 2 | 6 | 3 |

Each lead created two bounded assignments. The persistent-message count includes the initial
human objective and broker-generated assignment/report records. The six checked routes are lead to
each worker, each worker to its peer, and each worker back to the lead. All checked messages had
delivery receipts; every run ended with zero pending messages. Every member's native ID stayed
unchanged across turns, and the three members had distinct native sessions. Both leads closed the
assignments and proposed completion after receiving reports. Actual MCP calls performed the work;
the test did not fabricate model responses.

The reusable test asks specialists to develop a verification plan. Core-v2 adds short-lived turn
leases, stable input snapshots, staged work with atomic commit, durable proposals, bounded
assignments, scoped decision records, and inspected recovery. Ordinary tests cover those state
transitions; the live run establishes that both real-provider topologies can use the assignment
and messaging tools successfully.

Codex's app interface could read the corresponding Codex conversations after completion. Live
inspection through Agentisan's CLI or observer MCP profile does not take a competing native writer.
This is not a claim that a Desktop app can attach to or control an actively owned CLI turn.

Development failures remain recorded privately. Regression tests now cover native-session storage
boundaries, MCP-only turns, bounded stdin/output, stalled-run resume, stale leases, uncommitted and
committed restart recovery, assignment-budget races, and stable inbox retries. Failed calls with
incomplete usage are not counted as free. This report is not a token-cost comparison.

Raw objectives, credentials, message bodies, run IDs, and native session IDs stay in private local
state. The committed summary contains no such identifiers. The acceptance test proves
assignment-driven communication and native session continuity for these tested CLI profiles. It
does not prove general task quality, arbitrary code execution, direct API workers, authenticated
native approval UI, Windows CLI supervision, provider billing caps, or exactly-once recovery of
arbitrary external effects.
