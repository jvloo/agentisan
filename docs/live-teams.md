# Live CLI teams

Agentisan can run reusable teams with a Claude lead and Codex workers, or a Codex lead and
Claude workers. Each member is a separate native CLI conversation. Members use real MCP
tool calls to receive assignments, send peer messages, and report to the lead. These are
Agentisan-managed worker sessions, not the providers' built-in subagent objects.

The current profiles support consultation, planning, and review of supplied material. They
disable shell, file-editing, native delegation, external apps, and unrelated MCP servers.
Arbitrary coding tools, direct model-API workers, Windows CLI supervision, and verified
human approvals are not implemented. Registry/inspection remains usable on Windows.

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

`teams create` registers one lead and up to seven workers. It does not call a model. `teams run`
requires `--live`, returns a run ID immediately, and queues work for the separate service.
The service needs `--enable-cli-workers` to execute it. The initial objective is addressed to
the lead; the lead decides assignments and sends them through MCP. The scheduler currently
executes one native CLI turn at a time, preserving separate contexts and allowing peer
messages. Concurrent native execution is a future optimization, not a current claim.

Creating an existing managed team fails rather than silently replacing profiles or credentials.
New objectives on a team get fresh native sessions; old native IDs remain in the turn history.
Only one active run per team is allowed. Client-led coordination through a natively bound
external main agent remains a separate future adapter; current managed leads run in the service.

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
```

The existing stdio MCP connector exposes the same inspection operations. New tools are
`runs_inspect`, `messages_list`, `messages_receive`, `messages_send`, and `runs_complete`.
The three mutating tools require the credential-bound agent's active turn in that run.
They cannot register teams, expand limits, or confer human approval. Read access follows
the credential's group grants; message recipients must belong to the same team.

Native session/thread IDs appear as soon as the native CLI reports them, and every resumed
turn must return the same ID. The native CLI stores remain the normal user stores. Native
history can be opened in the provider's own client after Agentisan releases the session;
Codex app readback was checked after completed runs. Live inspection through Agentisan MCP
does not acquire a native writer. Do not start a competing native resume while a run is
active. Automatic native deep-link opening and cross-client writer handoff are not shipped.

## Delivery and completion

Messages are committed before an acceptance receipt is returned. Send keys are scoped to
run and sender; an identical resend returns the original receipt, while changed payloads
under the same key fail. Replies must address the original sender in the same run.
`messages_receive` acknowledges delivery, which is distinct from finishing the assignment.
The lead can propose completion only after pending messages are received, other active turns
settle, and each worker has reported to it. Native turn completion is verified separately.
The final run result still says `not_independently_verified`: it is an agent proposal, not
proof of correctness or a human approval.

Agents end a turn while waiting for peers. New pending messages cause the scheduler to resume
the exact native session. There are no automatic acknowledgement chains or hidden model
calls for routing. An agent that ends without receiving pending input stalls the run.

## Limits and recovery

Limits cover native CLI invocations, accepted messages, per-invocation elapsed time, output
size, and the run deadline. A native CLI invocation may contain multiple model/tool calls;
`max-turns` is not a per-model-request token cap. Claude also has a per-invocation USD 0.50
stopping threshold. Codex has no universal per-task dollar cap. Missing usage remains unknown,
and provider-side work or charges may continue after local cancellation.

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
rejects exhausted deadlines, failed/unknown turns, and runs without unread messages. General
reconciliation of interrupted model/tool work and durable human decisions remain future work.
Run records, source instructions, raw CLI traces, and account-related metadata stay under
the private data directory; never commit it.

## Repeat the live acceptance test

Ordinary tests use simulated adapters and real local transports. The live test is ignored
unless explicitly selected and enabled. It makes real account model calls, preserves native
sessions, and verifies all six lead/worker/peer routes plus stable, distinct native IDs.

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
