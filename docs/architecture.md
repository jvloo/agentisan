# Architecture

This document describes Agentisan's broader runtime design. The Rust implementation now includes
the fixture registry, managed native CLI teams with persistent MCP messages, bounded invocations,
native session continuation, authoritative live inspection, and administrator-selected deterministic
verification of exact proposed results. See the [live-team contract](live-teams.md) for what is
implemented and its limits. Human approvals, general effect reconciliation, direct API adapters,
and native deep-link opening remain proposed. Rust is selected for the core
([decision](decisions/0001-rust-core.md)); a general durable-execution engine remains undecided.

## Goals and non-goals

Agentisan would expose **one local service** through two front ends — a **CLI toolkit** and an **MCP server** — that call the same underlying behavior. The runtime must not depend on any particular vendor's CLI or Desktop app to function. Native integrations (e.g., linking into an existing client's session) are optional adapters with explicit capabilities. Direct provider API workers are an intended path, with their own credentials and billing, independent of any native app.

Agentisan does **not** aim to be a browser UI, a new Desktop app, or a recursive multi-coordinator swarm. It starts with one active coordination owner per objective, 1-to-N delegation, and bounded peer messaging between workers. Recursive N-to-N delegation and cross-team automation are explicitly deferred.

## Ownership model

- **Groups** are organizational/policy namespaces. They do not spawn automatic coordinating agents.
- **Teams** live inside groups; **agents** live inside teams.
- Exactly **one main agent owns coordination** for a given objective/job at any time. Ownership transfer is explicit and stale owners are rejected — the system must never let two main agents drive the same objective concurrently.
- The implemented CLI-team path is **service-led**: the service hosts the lead's native CLI turns and resumes members when messages arrive. Coordination continues while the service host is available. A natively bound **client-led** adapter remains planned; in that mode, a disconnected client-owned main agent would need to return for coordination. The two modes must never create competing owners of the same objective.

## What the service owns

The local service — not any model — owns:

- The **registry**: canonical group/team/agent/job/attempt IDs, parentage, and separately-tracked exact native adapter/host/session/thread/agent bindings, version/capabilities, and connection freshness.
- **Message and event persistence**, including idempotency keys and replayable cursors.
- **Permissions and budgets**, enforced with atomic reservations.
- **Job scheduling and attempts**, deterministically where possible — scheduling and policy decisions do not require a model call.
- **Human decisions and artifacts**.

The main agent's job is to propose decomposition and integrate results; workers contribute evidence. Worker success is not the same as acceptance — acceptance is a separate, ideally deterministic, check plus proportionate human or automated review. No automatic review chains are assumed.

## Identity and binding

A native session ID, thread ID, or subagent ID are **not interchangeable** — a native ID does not by itself grant authority to act. An MCP connection, client name, or working directory does not identify the calling conversation on its own; a trusted per-call or host binding is required. When binding is unavailable, the registry must mark it **unbound** rather than guess by inferring the newest matching session. Lifecycle callbacks (connect/disconnect/resume) can fire more than once, so registration and binding updates must be **idempotent**. A turn ending in a client is not proof that an agent process exited.

## Messaging and turn leases

Authenticated messages carry an idempotency/message ID, exact recipient, job/attempt ID, kind, correlation/reply-to reference, and artifact revision references. The sender is derived from the verified binding, never claimed by the caller. Each native turn receives a short-lived lease credential bound to one agent, run, turn, and ownership epoch; a newer epoch fences stale writers. `inbox_read` returns the stable inputs claimed for that lease without acknowledging them. `message_send` stages an outbound message, and `turn_commit` atomically acknowledges claimed inputs and publishes staged messages. A successful lead `result_propose` commits its inputs and durable proposal atomically. The service distinguishes accepted, delivered, processed, job-completed, and result-accepted states separately. Workers receive only job-scoped context. There is no universal exactly-once guarantee for external side effects; unknown delivery or completion status triggers reconciliation, and retries are bounded to cases where they are safe.

## MCP profiles

The model-facing MCP surface is role-scoped. The **agent** profile exposes
`agent_context_get`, `inbox_read`, `message_send`, `turn_commit`, and `result_propose` to a
lead. The **observer** profile exposes bounded read-only inspection tools and cannot send,
resume, approve, or execute. Connector and administrator operations remain outside model MCP:
connectors claim delivery and report native state through a separate authenticated interface;
administrators create teams, reconcile uncertain work, select verifiers, and record human
decisions. First-class assignments, decisions, and budget reservations are design targets, not
yet shipped agent tools.

## Budgets

A root budget is shared across the main agent, workers, retries, review, and recovery for one objective. Reservations are atomic and made **before** a model call, so concurrent workers cannot overspend the same allowance; usage that comes back unknown stays reserved rather than assumed free. The service tracks max calls, wall-clock time, concurrency, message exchange counts, child delegation depth, and stagnation, so a budget cannot expand itself. Token accounting and cache hits are tracked separately from dollar cost. Enforcement of cancellation and usage caps depends on what each provider adapter actually supports — the service will not claim an exact bill cap or symmetric control over opaque native workers it cannot fully observe.

This enforcement covers calls admitted by the service. A client-owned main agent may make calls outside that boundary: record usage when the host reports it and label missing coverage explicitly. Do not advertise a whole-workflow spending limit when the main agent or a worker can spend outside the service's control. Strict jobs must use adapters that supply their required controls or be rejected before dispatch. Reserve a bounded call allowance with appropriate headroom; a reservation alone is not a provider billing cap.

## Execution boundaries

Each job gets its own permissions and an isolated writer workspace; a plain worktree is **not** treated as a sandbox. Enforceable action boundaries require the runtime itself to own constrained tool execution, not just trust a model's stated intentions. An appropriate isolated executor is required before arbitrary code execution is enabled for any agent. Where an external adapter's guarantees (containment, cancellation, observability) are unclear, the service marks the behavior unsupported or unobservable rather than assuming it.

## Human decisions

Every human decision is persisted with the evidence considered, the proposed action, its scope and artifact revision, the principal eligible to decide, and exactly one resolution. A trusted human interaction in the existing host or an authenticated decision mechanism can supply approval; a model calling an "approve" tool is not human consent. Existing authorizations are honored across restarts while their scope and validity remain applicable; a missing decision blocks only the dependent work, and silence is never treated as approval. A material change to the proposed action invalidates a prior approval. Cancellation distinguishes a cancellation *request* from a *confirmed* stop, and retains its receipt and any existing artifacts.

## Inspection

Every agent is inspectable through both the CLI and MCP: activity, sent/received messages, visible tool actions, artifacts, usage where the adapter exposes it, and explicit uncertainty markers. Private chain-of-thought is never exposed. A proposed `agentisan://agents/<agent-id>` link (and equivalent decision links) navigates to an inspection view after authorization — it never sends a message, grants approval, or resumes a session by being opened. Native deep links and native session resume are offered only where the underlying client actually supports them; API-backed workers get no invented native conversation identity. No secret or prompt text is placed in a URI.

## Infrastructure (initial)

The initial deployment target is a single local background service, SQLite for persistence, and filesystem-based artifacts, alongside an isolated executor added before any code execution capability ships. An MCP subprocess connector is a thin transport and must not own the lifecycle of durable jobs — jobs must survive the connector process exiting. A powered-off or sleeping host cannot make progress; persisting data does not, by itself, make retrying an arbitrary external side effect safe.

The worker scheduler is authoritative only while healthy. A scheduler failure fences active work
and fails closed rather than leaving an HTTP process reporting stale activity. Verifier attempts
are reserved durably; startup and lock recovery reconcile abandoned `running` reservations to an
error state before another verification can proceed. A durable proposal remains inspectable and
independently verifiable after native process failure or service restart.
