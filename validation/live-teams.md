# Live Claude/Codex team acceptance

Validated on macOS on 2026-09-11 (Asia/Kuala_Lumpur), using the reusable managed-team
implementation and `tests/live_teams.rs`. The test was explicitly enabled; normal CI does
not call models. Sanitized results are in [live-teams-results.json](live-teams-results.json).

| Configuration | Native CLI invocations | Agent messages | Verified directed routes | Native sessions |
|---|---:|---:|---:|---:|
| Claude lead, two Codex workers | 5 | 6 | 6 | 3 |
| Codex lead, two Claude workers | 5 | 6 | 6 | 3 |

Each run also has one initial human-objective message. The directed routes are lead to each
worker, each worker to its peer, and each worker back to the lead. All required messages had
delivery receipts. Every member's native ID stayed unchanged across its turns, and the three
members had distinct native sessions. Both leads proposed completion after receiving worker
reports. Actual MCP calls performed communication; the test did not fabricate model responses.

The reusable test asks specialists to develop a verification plan. Earlier development runs
reviewed supplied excerpts of Agentisan's actual broker and supervisor, also exchanging
findings in both directions. A stdin-deadline issue from that review was corrected and covered
by a blocked-pipe/owned-child regression test. A process-group anchor with its own watchdog
was added before the final live acceptance run.

The current host's native session files were checked for the earlier completed review runs,
and Codex's app interface could read all three corresponding Codex conversations after
completion. Live inspection through Agentisan's own CLI/MCP avoids taking a competing writer.
This is not a claim that native Desktop apps can attach to an actively owned CLI writer.

Development failures remain recorded privately: an outer sandbox prevented native session
storage; disabling Codex's code-mode host also disabled MCP delivery; and a parser incorrectly
rejected an empty final-text item after successful MCP calls. The first issue required normal
host access to native session stores. The latter two were fixed without enabling shell/edit
tools or fabricating a final reply. A stalled run was explicitly resumed with its original
deadline, invocation count, and native IDs after inspection. Failed calls with incomplete
usage are not counted as free. This report is not a token-cost comparison.

Raw objectives, credentials, message bodies, run IDs, and native session IDs stay in private
local state. The committed summary contains no such identifiers. The acceptance test proves
communication and native session continuity for these tested CLI profiles. It does not prove
general task quality, arbitrary code execution, direct API-worker support, human approval,
Windows CLI supervision, or exactly-once recovery of external effects.
