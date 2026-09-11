# Live CLI teams

Agentisan can run reusable teams with a Claude lead and Codex workers, or a Codex lead and
Claude workers. Each member is a separate native CLI conversation. Members use real MCP
tool calls to receive assignments, send peer messages, and report to the lead. These are
Agentisan-managed worker sessions, not the providers' built-in subagent objects.

The current profiles support consultation, planning, and review of supplied material. They
disable shell, file-editing, native delegation, external apps, and unrelated MCP servers.
Arbitrary coding tools, direct model-API workers, Windows CLI supervision, and authenticated native
approval UI are not implemented. Registry and inspection remain usable on Windows.

## Create and run a team

Build with `cargo build --locked`. Use macOS or Linux, with normal account logins in installed
Claude Code and Codex CLIs. The current profiles have been exercised with Claude Code 2.1.263
and Codex CLI 0.153.4. A provider credential/base-URL override is rejected; this profile is
not an API-key or third-party-provider route. Executables in a team configuration are trusted
local administrator input, and must be absolute paths to binaries you trust.

Copy either [Claude-led](../examples/claude-led-team.json) or
[Codex-led](../examples/codex-led-team.json) configuration into private local storage. Replace
the executable paths and select models available to your accounts. Instructions and the
objective are yours; no task or answer is hardcoded into the runtime.

```sh
mkdir -p -m 700 .agentisan
cp examples/claude-led-team.json .agentisan/team.json
# Edit executable paths, models, and roles in .agentisan/team.json.
./target/debug/agentisan teams create --config .agentisan/team.json
./target/debug/agentisan serve --enable-cli-workers
```

From another terminal, write the objective to `.agentisan/objective.txt`, then submit it:

```sh
./target/debug/agentisan teams run --team claude_team \
  --prompt-file .agentisan/objective.txt --live \
  --max-turns 18 --max-messages 64 \
  --timeout-seconds 900 --turn-timeout-seconds 120
```

The shorter equivalent accepts inline text or the same prompt file:

```sh
./target/debug/agentisan run claude_team \
  --objective "Ask both specialists for evidence, reconcile their findings, and report" --live
```

`teams create` registers one lead and up to seven workers. It does not call a model. `teams run`
requires `--live`, returns a run ID immediately, and queues work for the separate service.
The service needs `--enable-cli-workers` to execute it. The initial objective is addressed to
the lead; the lead decides assignments and sends them through MCP. The scheduler currently
executes one native CLI turn at a time, preserving separate contexts and allowing peer
messages. Concurrent native execution is a future optimization, not a current claim.

Open the live terminal dashboard in another iTerm2 or terminal window:

```sh
./target/debug/agentisan --data-dir "$PWD/.agentisan"
./target/debug/agentisan --data-dir "$PWD/.agentisan" dashboard RUN_ID --agent AGENT_ID
```

Up/Down switches runs, clicking an agent or pressing Tab filters its message routes, and `o` opens
the selected agent's exact native CLI session only after Agentisan has released the run. The
dashboard returns automatically when that native chat exits. It reads authoritative local state
directly and does not require a browser or another model call.

Creating an existing managed team fails rather than silently replacing profiles or credentials.
New objectives on a team get fresh native sessions; old native IDs remain in the turn history.
Only one active run per team is allowed. Client-led coordination through a natively bound
external main agent remains a separate future adapter; current managed leads run in the service.

### Start from a planning chat through MCP

An MCP host such as Codex Desktop can connect with the managed lead's long-lived token and
`mcp --profile controller`. The profile derives the team from that credential and exposes five
tools: `controller_context_get`, `team_members_list`, `team_run_start`, `runs_inspect`, and
`messages_list`. Refine a plan in the host chat, then ask it to start the team with the accepted
objective. The start requires `live: true`, bounded optional limits, and a stable idempotency key.
The worker-enabled service must already be running.

This is service-led execution initiated by a planning chat. The MCP transport does not provide a
trustworthy Codex Desktop conversation ID, so the host chat cannot claim the managed lead's agent
identity or send peer messages on its behalf. The result reports that distinction explicitly. A
future client-led adapter needs a host-authenticated per-conversation binding and ownership transfer.

## Inspect from existing clients

Use the credential directory returned by `teams create`. Common lowercase IDs keep readable
filenames; case-distinct and reserved IDs use a portable encoding. For the supplied Claude-led
template:

```sh
./target/debug/agentisan --credential-file .agentisan/managed/claude_team/claude_lead.token \
  runs inspect RUN_ID
./target/debug/agentisan --credential-file .agentisan/managed/claude_team/claude_lead.token \
  messages list --run RUN_ID
./target/debug/agentisan --credential-file .agentisan/managed/claude_team/claude_lead.token \
  agents inspect codex_a
./target/debug/agentisan --credential-file .agentisan/managed/claude_team/claude_lead.token \
  runs watch RUN_ID --interval-ms 500 --timeout-seconds 300
```

The existing stdio MCP connector exposes the same inspection operations through the observer
profile. The agent profile exposes `agent_context_get`, `inbox_read`, `message_send`,
`assignment_update`, `decision_request`, `turn_commit`, and, for leads, `assignment_create` and
`result_propose`. Mutating tools require a short-lived credential
bound to the agent, run, turn, and current ownership epoch.
They cannot register teams, expand limits, or confer human approval. Read access follows
group grants for observer credentials; lease credentials can inspect only their own active identity.
Message recipients must belong to the same team.

Interactive v3 Phase 1 adds `mcp --profile operator` for the configured lead credential. Its
`team_start` call names exact configured workers and gives each bounded initial work; the scheduler
does not launch the configured lead. `team_status` returns committed peer messages and controller
reports without renewing control. `team_update` accepts reports, sends follow-ups, or finishes a
settled run, while `team_cancel` records confirmed versus unconfirmed stopping. The returned per-run
control handle is required for mutations and must remain private to the controlling chat context.

Native session/thread IDs appear as soon as the native CLI reports them, and every resumed
turn must return the same ID. The native CLI stores remain the normal user stores. Native
history can be opened in the provider's own client after Agentisan releases the session;
Codex app readback was checked after completed runs. Live inspection through Agentisan MCP
does not acquire a native writer. Do not start a competing native resume while a run is
active. Automatic native deep-link opening and cross-client writer handoff are not shipped.
Agentisan's `activity` record is the authoritative liveness source while it owns the run. Native
clients remain useful transcript inspectors, but their running/interrupted label is advisory for
an externally driven CLI session.

## Delivery and completion

Messages are committed before an acceptance receipt is returned. Send keys are scoped to
run and sender; an identical resend returns the original receipt, while changed payloads
under the same key fail. Replies must address the original sender in the same run.
`inbox_read` stores and returns one stable snapshot of messages, assignments, and decisions but
does not acknowledge them; repeated reads return the same snapshot even if an administrator
updates work records meanwhile. Those updates appear on a later turn. `message_send` stages an
outgoing message.
`turn_commit` atomically acknowledges claimed inputs and publishes staged messages. A successful
`result_propose` also commits the lead's inputs and durable proposal atomically. The lead can
create bounded assignments with reserved turn/message slices; assignees report them and the lead
closes them. Agentisan derives the immutable scope hash from the objective and completion criteria
and returns it in the creation receipt; the model does not choose that revision identifier. When
assignments exist, completion requires all of them to be terminal. Legacy runs
without assignments retain the earlier worker-report gate. Native turn completion is verified separately.
The final run result still says `not_independently_verified`: it is an agent proposal, not proof
of correctness or a human approval. A local administrator may run one exact-result verifier:

```sh
./target/debug/agentisan --data-dir "$PWD/.agentisan" runs verify RUN_ID \
  --verifier /absolute/path/to/verifier --timeout-seconds 30
```

The verifier receives the proposed result bytes on stdin and returns one JSON object with
`accepted`, `summary`, and optional `evidence`. Agentisan records accepted/rejected/error separately
from native execution completion, including verifier and result hashes. Agents cannot choose or
invoke the verifier. An error may be inspected and retried; acceptance or rejection is terminal.

Agents end a turn while waiting for peers. New pending messages cause the scheduler to resume
the exact native session. There are no automatic acknowledgement chains or hidden model
calls for routing. An agent that ends without receiving pending input stalls the run.

## Limits and recovery

Limits cover native CLI invocations, accepted messages, per-invocation elapsed time, output
size, and the run deadline. A native CLI invocation may contain multiple model/tool calls;
`max-turns` is not a per-model-request token cap. Claude also has a per-invocation USD 0.50
stopping threshold. Codex has no universal per-task dollar cap. Missing usage remains unknown,
and provider-side work or charges may continue after local cancellation.

Preflight, stdin transfer, and execution consume one monotonic turn deadline. Each preflight
stream is bounded to 64 KiB. Native stdout/stderr are piped through a shared capture task that
writes at most 8 MiB combined, with a separate truncation marker; oversized output never
reaches the raw log files beyond that cap. Field limits remain distinct from transport limits:
the HTTP envelope allows JSON escaping around a valid 16 KiB completion result.

A private process-group anchor retains process identity through cleanup, with a watchdog
that bounds the group even if the service dies. The service does not reap the anchor before
group cleanup. Stdin transfer and execution share the turn deadline. Detached provider modes
are excluded. Ctrl+C on the service interrupts active work and stops owned groups; a sleeping
or powered-off host cannot make progress. Per-run cancellation controls are not yet exposed.

Restarting a worker-enabled service marks in-flight runs interrupted and unconfirmed turns
unknown. It never automatically replays them. For a successfully completed but unproductive
turn with unread messages, an administrator can inspect the cause, fix it, and explicitly use:

```sh
./target/debug/agentisan runs resume RUN_ID --after-inspection
```

This narrow resume path keeps the original deadline, invocation count, and native IDs. It
rejects exhausted deadlines, unresolved unknown turns, and runs without unread messages. After
inspecting an unknown turn and confirming it produced no effect, a local administrator can run:

```sh
./target/debug/agentisan runs reconcile TURN_ID --no-effect --after-inspection
./target/debug/agentisan runs resume RUN_ID --after-inspection
```

For an uncommitted turn, no-effect reconciliation discards its private staged work and redelivers
its inputs. For a turn that already committed before native completion, it preserves the published
messages, assignment changes, decisions, proposal, and acknowledged inputs, and reconciles only the
unrecorded native effect. This never classifies or replays uncertain external effects automatically.
Other outcomes still require a future reconciliation path.

Agents may stage a scoped `decision_request`, but cannot resolve it. The trusted local CLI requires
the exact scope hash, artifact hash, and offered choice:

```sh
./target/debug/agentisan decisions inspect DECISION_ID
./target/debug/agentisan decisions resolve DECISION_ID \
  --scope-hash SHA256 --artifact-hash SHA256 --choice approve
./target/debug/agentisan decisions invalidate DECISION_ID
```

Resolution records the local OS administrator boundary; native authenticated human-interaction
adapters remain future work.

An exhausted or expired assignment does not block messages for other recipients. If its pending
work is the only remaining work, the run stalls without consuming another provider call. Inspect
the exact assignment, then allocate additional capacity from the unchanged root limits and resume:

```sh
./target/debug/agentisan assignments inspect ASSIGNMENT_ID
./target/debug/agentisan assignments extend ASSIGNMENT_ID \
  --add-turns 1 --deadline-seconds 120 --after-inspection
./target/debug/agentisan runs resume RUN_ID --after-inspection
```

The extension cannot enlarge the run's invocation, message, or wall-clock limit. If the root has
no remaining capacity, the work remains stalled for explicit reconciliation.
Run records, source instructions, raw CLI traces, and account-related metadata stay under
the private data directory; never commit it.

Database schema version 9 records per-agent ownership epochs, short-lived turn leases, complete
input snapshots, claimed turn inputs, staged messages and work operations, assignments, decisions,
durable run proposals, idempotent controller start receipts, and interactive controller capabilities.
Initialization readiness is recorded in
the same transaction as a successful fixture import or team creation. Failed first-time
initialization may leave a schema file, but the service will not treat it as ready. Existing
databases with records are migrated through schema version 9; old delivery receipts remain
readable. New turns use lease-aware delivery and never restore acknowledge-on-read semantics. An
old empty database without readiness evidence needs an explicit valid import or team creation.
No failed import deletes an existing database.

## Repeat the live acceptance test

Ordinary tests use simulated adapters and real local transports. The live test is ignored
unless explicitly selected and enabled. It makes real account model calls, preserves native
sessions, and verifies all six lead/worker/peer routes plus stable, distinct native IDs.
The default acceptance profile uses Claude Sonnet and Codex Luna at low effort. Override
`AGENTISAN_LIVE_CLAUDE_MODEL`, `AGENTISAN_LIVE_CODEX_MODEL`, or `AGENTISAN_LIVE_EFFORT` for a
controlled comparison without editing the test.

The latest committed validation used the default low-effort Sonnet/Luna profile. Both lead
directions completed five native turns, two assignments, eleven persistent messages, all six
checked communication routes, and three distinct native sessions, with no pending messages.
See the [sanitized results](../validation/live-teams.md). Ordinary validation currently comprises
81 tests; CI runs them on Linux, macOS, and Windows without model calls.

```sh
AGENTISAN_LIVE_TESTS=1 \
AGENTISAN_LIVE_STATE_DIR="$PWD/.agentisan/live-validation" \
AGENTISAN_CLAUDE_BIN="/absolute/path/to/claude" \
AGENTISAN_CODEX_BIN="/absolute/path/to/codex" \
cargo test --locked --test live_teams -- --ignored --nocapture
```

The output directory is deliberately retained for inspection. Run this where the CLIs can
access their normal authentication/session stores. A restrictive outer sandbox may prevent
session persistence even when the account itself is valid; fix that host capability instead
of changing provider routes or silently using ephemeral sessions.
