# Live Desktop inspection and result verification

Validated on macOS on 2026-09-11 (Asia/Kuala_Lumpur) with one Claude Haiku lead and two
Codex Luna workers at low effort. The team implemented and reviewed a small pure Python
function. It used five native CLI invocations, six agent messages, all six directed lead/worker
and peer routes, and three stable native sessions. The run completed in 81 seconds without
model retries or escalation.

Both Codex worker conversations were navigable through Codex Desktop while the Agentisan run
was active. Readback exposed their MCP receives, direct peer messages, review, and proposed code.
A later read returned the implementer's second turn before the lead finished. The native client
reported an active externally driven turn as `interrupted`, so Agentisan now exposes a derived,
authoritative `activity` record and a bounded `runs watch` command. Native client state remains
advisory; Desktop is useful for transcript inspection and does not become a competing writer.

The proposed function passed 312 independent fixed and generated checks. The combined submission
still failed acceptance: one agent-produced expected result was wrong, and the reviewer described
a mutation bug that was absent from the draft. This is the intended distinction between successful
message delivery, execution completion, and result acceptance.

After implementing the exact-result verifier, the preserved run was checked again. Agentisan
recorded `acceptance: rejected`, the failed case and actual output, hashes of the proposed result
and verifier, and the verifier artifact location. A malformed or timed-out verifier records an
error without acceptance; an accepted or rejected receipt is terminal. Tests cover acceptance,
rejection after an error, bounded timeout, and authoritative activity. The current watcher test
exercises a terminal snapshot; it does not prove a changed snapshot arriving during an active run.

The committed report omits raw run, credential, message, and native session identifiers. The
private local evidence remains under the ignored `.agentisan/` directory. This validation proves
refreshable transcript inspection, not automatic visual streaming or safe cross-client writer
handoff.
