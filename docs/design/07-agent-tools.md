# Agent access to other services

Status: **built in two steps**: reference documents (the daemon hands the text over once) and read-only tools (the agent asks while it works, through a broker). Servers that speak MCP themselves, writing tools and database drivers are still not supported.

## Built: reference documents

The owner lists documents on the Knowledge page (an internal wiki page, an API description, a runbook), for every repository or one. The daemon fetches each through the engine plug-in (the only part with network access): http(s) only, no redirects, text-like content only, a size limit, a lifetime (default 10 minutes) after which it is fetched again, and the last good text is used if the address fails. A credential goes in a header and is kept as a file `ref_<id>` in the secrets directory; the agent never sees it, and the fetcher removes it from anything it returns. The text reaches triage, development and review as a `<reference>` block that the system rules call data. Nothing the agent does can change the list. It needs no network for the agent and no new egress rule.

Limits: up to 8 documents (300 000 characters) per run; the agent reads them as background and cannot query a service. What a document says is untrusted: point only at pages your own people write.

## Built: read-only tools through a broker

On the Knowledge page an owner grants a tool: a name, a base address, an optional credential header, the agents allowed to use it (triage and reviewer by default, the developer only if ticked), a repository or all, and a call limit. For each run the engine plug-in (on the host) starts a broker listening on a Unix socket in a private directory and writes an MCP configuration that points the agent's CLI at `tl_mcp_shim.js`, a small MCP server that runs next to the agent (in the sandbox, the script and the socket directory are mounted read-only; no network is added) and forwards each call to the broker. The broker checks the per-run token, then does a GET below the base address with the credential, and returns text. The shim has no credential and no network. The per-run token sits in a file in the broker's private directory (never on a command line); the directory and socket are private to the daemon's user when the sandbox runs as that user (the normal case), and otherwise can be entered but not listed. Use is counted per tool and shown on the page.

What the agent can do: call the tools granted for this run, with a `path`; nothing else. The rules in the list above hold: only the owner writes grants; per repository and role; read only; no secret in the agent; counted; answers are data.

Unsupported: tools that write, drivers (a database is reached through an HTTP service that you put in front of it), MCP servers started by the repository, and anything that needs a network inside the sandbox.

## Original proposal (kept for the reasoning)

Status of what follows: superseded by the broker above. Today the Claude engine runs with `--strict-mcp-config` and no MCP configuration, a fixed tool list (Read, Grep, Glob; for the developer Edit, Write and a few git commands plus the repository's test command) and an egress proxy that allows only the model API. That is deliberate: an agent that reads untrusted ticket text must not be able to reach anything else.

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
