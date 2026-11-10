# Agent access to other services (design, not built)

Status: **proposal.** Nothing here is implemented. Today the Claude engine runs with `--strict-mcp-config` and no MCP configuration, a fixed tool list (Read, Grep, Glob; for the developer Edit, Write and a few git commands plus the repository's test command) and an egress proxy that allows only the model API. That is deliberate: an agent that reads untrusted ticket text must not be able to reach anything else.

## The need

Teams want an agent that can look at their other systems while it works: a database schema, an internal API's documentation, a monitoring page, a design system. Each of those is a tool the agent would call.

## Rules any design must keep

1. **Only the owner grants a tool**, in the console, per repository. A ticket, a comment, a pull request or `.tasklane.yml` in the repository can never add or widen one (the file may only tighten, as today).
2. **Read only first.** A server is offered to the triage and reviewer roles and to the developer only when the owner marks it so; no tool that writes outside the working directory in the first version.
3. **Least reach.** Each grant names the server, the tools of it that are allowed, and the network hosts it needs. The egress proxy adds exactly those hosts for that repository and no others.
4. **No secrets in the agent.** The credential for a server stays in the daemon (the same rule as for git): the agent talks to a local proxy that adds it. A server that needs the secret inside the agent process is not supported.
5. **Everything is audited**: which grant a run used and how many calls it made.
6. **Untrusted output is data.** What a tool returns is wrapped in a block the system rules call data, like ticket text.

## Shape

- Table `agent_tools (service_id, name, kind, config jsonb, allowed_tools text[], hosts text[], roles text[])`, written only by console handlers behind the admin session and CSRF check.
- The engine receives the repository's grants in `profile.tools`, writes an MCP config file into the run's private directory, and passes `--mcp-config` with `--strict-mcp-config`; `--allowedTools` lists only the granted tool names.
- A Repositories card section "Tools the agents may use" with an add form, a test button (lists the server's tools) and the audit count.

## Open questions

- Which MCP servers to support first (stdio servers started inside the sandbox, or HTTP servers behind the daemon's proxy). Starting a stdio server inside the sandbox puts its code next to the agent; the proxy route is safer and is the proposal.
- How to show a server's tool descriptions to the owner without trusting them (a malicious server can describe a tool misleadingly).
- A per-run call budget.

Do not implement this before the threat model in `04-threat-model.md` has a section for it and the egress proxy supports per-repository host lists.
