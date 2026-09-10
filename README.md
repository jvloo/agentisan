# Agentisan

**Agent teams. Your tools. Your control.**

Agentisan (agent + artisan) is a proposed local service for coordinating teams of AI agents from the CLI or Desktop clients you already use — no new mandatory UI, no lock-in to a single vendor's app.

> **Status: design bootstrap.** No runtime, CLI, MCP server, or installer exists yet. This repository currently contains design documents only. Every command, URI, and guarantee described below is a proposal, not a shipped feature.

## The problem

Developers using several agent tools often relay context, assignments, and review feedback by hand. Switching clients makes it harder to follow who owns an assignment, what a worker changed, and whether another attempt is worth its cost. Agentisan proposes a shared record of that work, with explicit execution limits and human decisions.

Agentisan aims to give one main agent (which you talk to normally, in your existing CLI or Desktop client) the ability to delegate scoped work to specialist agents, while a local service — not the model — owns the registry, message history, budgets, permissions, and job state. The service is deterministic where it can be: scheduling and policy don't require a model call.

## Concept

- You talk to **one main agent** per objective, in the client you already use.
- The main agent **delegates** to specialist workers, which may exchange bounded, scoped peer messages.
- **Groups** contain **teams**, which contain **agents** — organizational and policy namespaces, not automatic extra coordinators.
- A local **service** persists everything: registry, messages, decisions, artifacts, budgets, job attempts.
- Every agent is **inspectable** — activity, messages, tool actions, artifacts, usage — via both a CLI and MCP, with no private reasoning ever exposed.
- **Human decisions** are recorded with evidence and a scope; only a trusted approval path counts as consent.

Client-led coordination is the intended starting experience: accepted service-owned workers can keep going while you're disconnected, but the client-owned main agent must reconnect to continue coordinating. Control and usage coverage depend on each adapter; unrelated activity in the main agent's client is outside the service's budget enforcement.

## Current state

This repository contains architecture and roadmap documents only. There is no code, no package to install, and no service to run. See [docs/architecture.md](docs/architecture.md) for the design and [ROADMAP.md](ROADMAP.md) for what's planned and in what order.

## Navigation

- [docs/architecture.md](docs/architecture.md) — design decisions, execution boundaries, what the service does and doesn't own.
- [ROADMAP.md](ROADMAP.md) — milestones and acceptance criteria.
- [LICENSE](LICENSE) — MIT.

## Hypothetical usage (proposed, not implemented)

Once a runtime exists, a session might look like this:

```
$ agentisan whoami
unbound — no verified caller for this session

$ agentisan teams list --group inventory
TEAM        AGENTS  ACTIVE JOBS
restock     3       1

$ agentisan agents inspect worker-7f2a
status: running · budget: 3/10 calls reserved
last message: "checked warehouse-3 stock, discrepancy found"
artifacts: 1 (report.md, unreviewed)

$ agentisan messages follow --team restock
[main-agent -> worker-7f2a] job assigned: reconcile warehouse-3
[worker-7f2a -> main-agent] evidence: stock_count.csv attached
```

Nothing above executes today. It illustrates the intended shape of read-only inspection and bounded messaging, not a promised interface.
