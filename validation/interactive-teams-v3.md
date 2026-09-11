# Interactive agent teams v3 Phase 1 validation

Date: 2026-09-12. This report is sanitized; native session IDs, credentials, control handles, raw
provider traces, and private prompts remain in the ignored local state directory.

## Scenario

One Agentisan operator MCP connection represented the external main chat. The configured team
contained a Codex lead identity and two Claude Code workers using Sonnet at low effort. The operator
sent distinct architecture and validation work directly to the Claude workers. The workers had to
exchange findings in both directions before each reported to the controller mailbox. The operator
accepted both reports and finished the run.

## Observed result

- Run mode and final state: `interactive`, `completed`.
- Native turns: three worker turns and zero turns for the configured Codex lead.
- Native continuity: two distinct persistent Claude sessions, one per worker.
- Durable messages: six total — two controller assignments, two peer messages, and two reports.
- Checked routes: controller to each worker, each worker to the other, and each worker to controller.
- Both committed reports were explicitly accepted before controller completion.
- SQLite stored only the control-handle hash; no plaintext daemon master key was present.
- The two released Claude sessions were opened successfully in Codex Desktop native terminal panels.

## Limits of this evidence

The run validates real Claude execution, the operator MCP protocol, durable peer messaging, controller
completion, native-session continuity, and absence of a managed-lead model call. The operator client
for this acceptance was a deterministic MCP harness. Loading the new operator profile directly in a
fresh Codex Desktop chat remains a separate host-integration check after installation. The final
worker content was not independently verified, and the run does not establish a provider billing cap,
coding-workspace isolation, trusted native human approval, or exactly-once external effects.
