# Milestone 1: registry and inspection

Implemented scope: a single Rust executable, an explicit fixture importer, a persistent
SQLite registry, a loopback HTTP inspection service, a CLI client, and a stdio MCP connector.
The example describes simulated agents; none of them executes a model or a tool.

## Data and identity

Groups contain teams; agents reference a team and optionally a parent in that same team.
Newtypes distinguish canonical group, team, agent, and principal IDs. Native bindings retain
adapter, host, namespace, session, thread, and subagent identifiers separately. The complete
native binding is unique. The same native session string can appear under another namespace
without merging two canonical agents.

Fixtures are complete registration sets with `schema_version: 1` and adapter `fake`. Import
validates names, IDs, references, grants, parent cycles, and duplicate bindings. The import
transaction inserts missing records, accepts equal records, and rejects changes to existing
records. A conflict rolls back that transaction. Import does not delete records omitted from
a later fixture. Rebinding, revocation, incremental registration, and real lifecycle hooks
will need separate administrative contracts.

`init` generates a random opaque credential per fixture principal and stores only its SHA256
digest in SQLite. On repeated import it reuses the credential file. Credential files may be
created before the database transaction; a process crash at that point can leave an unused
file, which a later identical import can reuse. Handled failures remove newly created files.
This bootstrap process is not a durable job-execution implementation.

The adapter credential determines the principal and grants. Query parameters cannot select
another principal. A bound fixture principal is evidence of possession of a provisioned
fixture credential, not proof of native process or conversation identity. An MCP connection
may serve multiple conversations: this connector does not distinguish them. Do not provision
a shared connector with access those conversations should not share.

Anonymous callers can request `whoami` only. Authenticated unbound observers can inspect
their explicitly granted groups. Invalid credentials fail even for `whoami`. An inaccessible
resource and a nonexistent resource produce the same public not-found error.

## Interfaces

The loopback service provides `GET /health` with version metadata and `POST /v1/inspect` with
a strict JSON query and optional `Authorization: Bearer ...` header. `whoami` and `groups_list`
take no identity arguments. `teams_list`, `agents_list`, and `agents_inspect` take an exact
`group_id`, `team_id`, or `agent_id`. There is no public registration endpoint.

CLI and MCP forward the same queries to that service. Five MCP tools are declared read-only;
the actual service paths perform reads. The MCP process never opens SQLite. Its stdout is
reserved for protocol messages; service readiness and CLI results are JSON. MCP initialization
and tool calls are handled by RMCP, with wire-level integration tests. Client compatibility
beyond the exercised protocol remains to be verified.

CLI clients accept numeric loopback HTTP origins only, disable proxies and redirects, and
use request deadlines. The HTTP service binds loopback only, limits request bodies, rejects
browser Origin headers, and returns generic storage errors. It has no browser interface,
message delivery, worker dispatch, deep-link handler, or native resume capability.

Inspection returns registered metadata with `source: fixture`, `connection: unverified`,
and `activity: unobserved`. It does not manufacture live status. `native_open`, `native_resume`,
and `execute` capabilities are false. Possessing an ID alone grants no access.

## Operating boundary

One trusted OS account owns and administers the state directory. On Unix, the CLI creates
owner-only state directories and credential files and rejects permissive or symlinked paths
at the checked locations. Windows relies on the directory's inherited OS ACL; automated
ACL provisioning and equivalent adversarial filesystem protection are not implemented.
Local administrators and processes with access to that account's files are outside the
credential-isolation claim. Credentials are development capabilities, not provider tokens.

The daemon remains available after its connector exits. Database records survive daemon
restart. Sleeping or powering off its host stops progress. This milestone has no jobs whose
effects need recovery, and does not establish cancellation or budget enforcement for workers.
The SQLite database and credential files should be backed up together using a consistent
database backup or while the service is stopped.

## Validation boundary

Tests use fake metadata, real SQLite databases, and actual CLI/service/MCP subprocesses.
They check persistence, credential scoping, missing identity, namespace collisions, conflicting
registration rollback, invalid parentage, and strict query inputs. These tests make no model
calls. They establish registry and transport behavior, not agent effectiveness or native
application integration. Future execution work must add fault tests around effects, ownership,
permissions, approvals, and shared budgets.
