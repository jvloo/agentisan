# Working on Agentisan

Read README.md for current status and docs/architecture.md for the proposed contracts.
ROADMAP.md records implementation milestones. Keep shipped behavior distinct from design
intent. At bootstrap this repository contains documentation only; there is no CLI,
MCP server, test suite, or selected implementation stack.

## Product constraints

- Keep existing CLI and Desktop applications as the primary human interfaces. A separate
  browser UI is optional, not an MVP dependency.
- Give CLI and MCP access to the same registry and runtime behavior. Groups contain teams;
  agents have stable identities and explicit native bindings.
- Preserve one active coordination owner per objective. Peer messages do not grant authority.
- Use exact verified identities. An MCP connection, directory, PID, or latest timestamp is
  insufficient to identify and authorize a native conversation.
- Deep links navigate to inspectable work; opening a link must not send a message, grant
  approval, or resume execution.
- Declare adapter capabilities and limits. Do not imply native resume, spending control,
  containment, or live observability where an adapter cannot establish them.

## Implementation discipline

- Evaluate existing execution components before committing to a runtime engine. Keep
  provider and native-client details behind adapters.
- Separate job acceptance, message delivery, turn completion, and result acceptance.
- Persist operation identity and receipts. Reconcile uncertain effects before retrying;
  never claim exactly-once behavior for arbitrary external operations.
- Bound all owned work and account for children, retries, verification, and missing usage.
  Concurrent workers share the root budget; they cannot expand it themselves.
- Isolate concurrent writers. Worktrees alone do not isolate processes or credentials.
- Record human decisions against concrete scope and artifact revisions. Agent-generated
  claims of approval are not human consent. Preserve authority already granted by the user.
- Keep credentials, native session records, raw model transcripts, and runtime artifacts
  outside tracked files. Use synthetic identifiers in examples.
- Add proportionate checks for implemented behavior, especially interruption, duplicate
  delivery, stale ownership, permissions, and budget races. Report fake-adapter tests and
  live provider validation separately. Include failed calls in usage accounting.
- Update status documentation when behavior ships. Do not add installation instructions,
  passing badges, or performance claims before they are verified.
