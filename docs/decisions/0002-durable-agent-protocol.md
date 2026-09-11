# ADR 0002: Durable agent protocol with connector-owned leases

## Status

Accepted for the core-v2 implementation.

## Context

The first managed-team runtime used one long-lived credential per agent. Reading an inbox marked
messages delivered immediately, and the lead's completion proposal lived on the run row. Those
shortcuts proved native communication, but they do not survive lost tool responses, stale native
processes, or a lead process failure after proposing a valid result.

MCP also served inspection and mutation from one broad tool catalog. That made the connector easy
to bootstrap, but exposed full-run data to worker credentials and did not distinguish a model,
observer, connector, or administrator trust boundary.

## Decision

Agentisan separates four surfaces:

1. The core owns durable messages, claimed inputs, assignments, proposals, decisions, assignment
   budget slices, turn leases, and acceptance. A unified event/cursor store, provider usage
   reservations, and connector epochs extend the same boundary in later migrations.
2. Connectors claim delivery and receive a short-lived turn credential bound to one agent, run,
   turn, and ownership epoch. A newer epoch fences every older writer.
3. Agent MCP tools operate only within that lease. The first read stores the complete message and
   work snapshot; later reads return it unchanged and do not acknowledge processing. `turn_commit`
   is the atomic boundary that advances the inbox cursor and publishes staged work.
4. Observer MCP tools are read-only. Administrative cancellation, reconciliation, verification,
   human decisions, and binding changes remain outside the model-facing MCP surface.

Native sessions remain owned by their native clients. A Desktop companion may prevent competing
writers, but it may act as an exact agent only when its host provides verifiable per-conversation
identity. Otherwise it is an observer or a human-mediated lead. Working directory, process ID,
connection identity, recency, and native session ID alone never grant authority.

## State boundaries

- Delivery acceptance, connector dispatch, model receipt, turn commit, native process completion,
  result proposal, deterministic verification, and human acceptance are distinct records.
- An uncertain native effect is never replayed automatically. An administrator reconciles it with
  evidence before the run continues.
- Restart reconciliation distinguishes an uncommitted turn, whose private staged work is discarded,
  from a committed turn, whose published effects remain authoritative.
- A result proposal is immutable and remains inspectable even when the proposing native turn later
  fails or the service restarts.
- Assignment slices cannot be expanded by an agent or child. Future provider-usage reservations
  must remain held when usage is missing rather than treating it as free.
- Deep links navigate to inspectable work; they never send, resume, approve, or transfer ownership.

## Consequences

The protocol requires more explicit state than the initial message table, but crash windows become
auditable and stale connectors can be rejected deterministically. Role-specific MCP catalogs reduce
context and authority. Provider and Desktop integrations remain adapters, so unsupported liveness,
identity, cancellation, or writer behavior is reported as a capability gap rather than inferred.

The migration keeps historical runs readable. Compatibility tools may translate old calls during a
bounded transition, but the new agent runtime must not restore acknowledge-on-read or team-lifetime
mutation authority.
