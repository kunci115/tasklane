# 05. Plug-in protocol

Status: draft 1. Plug-ins are separate programs, in any language. The core starts each plug-in as a child process and talks to it over **JSON lines**: one JSON object per line on stdin (requests) and stdout (responses). stderr is for logs and is never parsed.

```
-> {"id": 1, "op": "describe"}
<- {"id": 1, "ok": true, "result": {"name": "claude", "role": "agent_engine", "protocol_major": 1, "version": "0.0.1"}}
-> {"id": 2, "op": "run", "params": {...}}
<- {"id": 2, "ok": false, "error": {"kind": "permanent", "message": "..."}}
```

Rules:

1. Every request has a unique `id`; the response echoes it. Responses may arrive out of order.
2. `describe` is always supported and returns `name`, `role` (`ticket_source`, `agent_engine`, `notifier`, `chat_channel`, `device_adapter`, `memory`), `protocol_major` and `version`. The core refuses a plug-in whose `protocol_major` it does not know.
3. Errors have a `kind` of `transient`, `permanent`, `unauthorized` or `rate_limited`; the core decides about retries.
4. The operations for each role are listed in `03-plugin-interfaces.md`.
5. The core passes a plug-in only the environment variables and secrets that role needs. Plug-ins do not get network access they do not need (enforced by the container or namespace the core starts them in).
6. A plug-in that stops answering is killed after the call's timeout; its in-flight calls fail as `transient`.

Reference implementations: the Rust types in `crates/tasklane-plugin`, and `plugins/engine-claude` (Python, skeleton).
