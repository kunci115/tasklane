#!/usr/bin/env python3
"""A generic agent-engine plug-in: runs ANY command-line coding agent (Codex CLI, Gemini CLI, Aider, ...).

It speaks the same plug-in protocol and keeps the same safety rules as engine-claude (it reuses its
prompt blocks, result checks, sandbox and container handling), but the agent program and its flags are
yours. tasklane gives the agent a prompt on stdin (or as an argument) and a workspace; the plug-in turns
what comes back into the result tasklane expects.

Environment (all set by the operator):
  TASKLANE_CLI_COMMAND            the command, split like a shell would, e.g. `codex exec --full-auto -`
                                  (no shell is ever started). Put `{prompt}` as one argument to pass the prompt
                                  as an argument instead of on stdin.
  TASKLANE_CLI_COMMAND_READONLY   optional: the command for the read-only roles (triage, reviewer, ask, duplicate),
                                  for example with the agent's own read-only or sandbox flag. Defaults to the above.
  TASKLANE_CLI_NAME               a label shown in the console (default: the program name)
  TASKLANE_CLI_CREDENTIALS        names of the environment variables the agent needs, e.g. OPENAI_API_KEY. They are
                                  passed to the container by name (never on a command line) and scrubbed from output.
  TASKLANE_SANDBOX=docker, TASKLANE_SANDBOX_IMAGE, TASKLANE_SANDBOX_NETWORK, TASKLANE_SANDBOX_PROXY: as for engine-claude.
                                  The image must contain your agent program (build one `FROM tasklane-sandbox`).

What you give up compared with engine-claude: the agent's own tool list cannot be restricted from here
(the read-only roles get a read-only mount in the Docker sandbox, which is enforced), and token usage is not
reported unless the agent prints nothing we can read, so the per-ticket token budget does not apply.
"""
import json
import os
import re
import shlex
import sys
import tempfile
import time
import shutil
import uuid

sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "engine-claude"))
import engine_claude as base  # noqa: E402  (the shared prompt blocks, result checks and sandbox code)

EngineError = base.EngineError
PROTOCOL_MAJOR = 1
VERSION = "0.1.0"
NAME_RE = re.compile(r"^[A-Z][A-Z0-9_]{2,63}$")


def credentials(env=None):
    env = os.environ if env is None else env
    names = [n.strip() for n in env.get("TASKLANE_CLI_CREDENTIALS", "").split(",") if n.strip()]
    for n in names:
        if not NAME_RE.match(n):
            raise EngineError("permanent", "TASKLANE_CLI_CREDENTIALS must be variable names like OPENAI_API_KEY")
    return tuple(names)


# the shared code scrubs and passes these by name (a bad list is reported on the first run)
try:
    base.CRED_VARS = credentials()
except EngineError:
    base.CRED_VARS = ()


def command(role, env=None):
    env = os.environ if env is None else env
    key = "TASKLANE_CLI_COMMAND"
    if role != "dev" and env.get("TASKLANE_CLI_COMMAND_READONLY", "").strip():
        key = "TASKLANE_CLI_COMMAND_READONLY"
    raw = env.get(key, "").strip()
    if not raw:
        raise EngineError("permanent", "TASKLANE_CLI_COMMAND is not set")
    try:
        argv = shlex.split(raw)
    except ValueError:
        raise EngineError("permanent", "%s cannot be parsed" % key)
    if not argv or not re.fullmatch(r"[A-Za-z0-9_./+-]{1,200}", argv[0]):
        raise EngineError("permanent", "%s must start with a program name" % key)
    return argv


def with_prompt(argv, prompt):
    """(argv, text for stdin): `{prompt}` as a whole argument puts the prompt there; otherwise stdin."""
    if "{prompt}" in argv:
        return [prompt if a == "{prompt}" else a for a in argv], ""
    return argv, prompt


def agent_name(env=None):
    env = os.environ if env is None else env
    name = env.get("TASKLANE_CLI_NAME", "").strip()
    if name:
        return re.sub(r"[^A-Za-z0-9 ._-]", "", name)[:40] or "cli"
    raw = env.get("TASKLANE_CLI_COMMAND", "").split()
    return os.path.basename(raw[0])[:40] if raw else "cli"


def run(params):
    base.require_sandbox_or_opt_in()
    role = params.get("role")
    if role not in base.ROLES:
        raise EngineError("permanent", "unknown role: %s" % (role,))
    if not isinstance(params.get("ticket"), dict):
        raise EngineError("permanent", "params.ticket is required")
    if role == "reviewer" and not isinstance(params.get("diff"), str):
        raise EngineError("permanent", "params.diff is required for reviewer")
    if role == "reviewer" and len(params["diff"]) > base.MAX_TEXT:
        return {"verdict": "escalate", "notes": "the diff is too large to review in full (%d characters); a person must look at it" % len(params["diff"]),
                "usage": {"tokens_in": 0, "tokens_out": 0, "secs": 0}, "exit_reason": "ok"}
    if role == "ask" and not isinstance(params.get("question"), str):
        raise EngineError("permanent", "params.question is required for ask")
    workspace = params.get("workspace")
    if role == "dev" and not (isinstance(workspace, str) and os.path.isdir(workspace)):
        raise EngineError("permanent", "params.workspace must be an existing directory for dev")
    if workspace is not None and not (isinstance(workspace, str) and os.path.isdir(workspace)):
        raise EngineError("permanent", "params.workspace is not a directory")
    budget = params.get("budget") if isinstance(params.get("budget"), dict) else {}
    max_secs = budget.get("max_secs", base.DEFAULT_MAX_SECS)
    if isinstance(max_secs, bool) or not isinstance(max_secs, (int, float)) or max_secs <= 0:
        max_secs = base.DEFAULT_MAX_SECS

    argv_template = command(role)
    system = base.build_system(role, params.get("profile"))
    # these CLIs have no common system-prompt flag, so the rules lead the prompt
    prompt = system + "\n\n" + base.build_prompt(role, params)
    cli_argv, stdin_text = with_prompt(argv_template, prompt)
    home = tempfile.mkdtemp(prefix="tasklane-cli-home-")
    scratch = None
    container = None
    creds = credentials()
    try:
        if base.sandbox_mode() == "docker":
            container = "tasklane-%s" % uuid.uuid4().hex[:12]
            denv = dict(os.environ, TASKLANE_SANDBOX_CLAUDE_BIN=cli_argv[0])
            argv = base.docker_command(role, cli_argv[1:], workspace, container, denv)
            cwd = None
            env = {"PATH": os.environ.get("PATH", "/usr/local/bin:/usr/bin:/bin"), "HOME": home}
            for v in creds + ("DOCKER_HOST",):
                if os.environ.get(v):
                    env[v] = os.environ[v]
        else:
            argv = cli_argv
            env = {"PATH": os.environ.get("PATH", "/usr/local/bin:/usr/bin:/bin"), "HOME": home}
            for v in creds:
                if os.environ.get(v):
                    env[v] = os.environ[v]
            cwd = workspace
            if not cwd:
                scratch = tempfile.mkdtemp(prefix="tasklane-cli-cwd-")
                cwd = scratch
        t0 = time.monotonic()
        rc, out, err, timed_out = base.execute(argv, cwd, env, stdin_text, max_secs, container,
                                               os.environ.get("TASKLANE_DOCKER_BIN", "docker"))
        secs = time.monotonic() - t0
    finally:
        shutil.rmtree(home, ignore_errors=True)
        if scratch:
            shutil.rmtree(scratch, ignore_errors=True)

    if timed_out:
        raise EngineError("transient", "budget exhausted: killed after %ss" % max_secs, exit_reason="budget")
    if rc != 0:
        detail = base.scrub((err or out or "").strip())[-300:]
        raise EngineError(base.classify_failure(detail), "%s exited with code %s: %s" % (agent_name(), rc, detail))
    usage = {"tokens_in": 0, "tokens_out": 0, "secs": round(secs, 2)}
    if role == "dev":
        result = {"summary": base.scrub(out.strip()[-4000:]) or "The agent finished and wrote no summary."}
    else:
        result = None
        for obj in reversed(base.find_json_objects(out)):
            result = base.validate(role, obj)
            if result:
                break
        if not result:
            raise EngineError("permanent", "unusable model output")
        for key in ("answer", "notes", "plan", "reason"):
            if isinstance(result.get(key), str):
                result[key] = base.scrub(result[key])
        if isinstance(result.get("questions"), list):
            result["questions"] = [base.scrub(q) if isinstance(q, str) else q for q in result["questions"]]
    result.update({"usage": usage, "exit_reason": "ok"})
    return result


def check():
    params = {"role": "ask", "ticket": {"service": "check/check", "ref": "0", "title": "", "body": "", "comments": []},
              "question": 'Reply with exactly this JSON and nothing else: {"answer": "ok"}', "budget": {"max_secs": 90}}
    t0 = time.monotonic()
    try:
        res = run(params)
    except EngineError as e:
        return {"ok": False, "kind": e.kind, "message": base.scrub(str(e))[:300], "secs": round(time.monotonic() - t0, 1)}
    return {"ok": True, "answer": base.scrub(str(res.get("answer", "")))[:80], "secs": round(time.monotonic() - t0, 1),
            "sandbox": base.sandbox_mode()}


def handle(req):
    rid = req.get("id") if isinstance(req, dict) else None
    try:
        if not isinstance(req, dict):
            raise EngineError("permanent", "request must be a JSON object")
        op = req.get("op")
        if op == "describe":
            res = {"name": agent_name(), "role": "agent_engine", "protocol_major": PROTOCOL_MAJOR, "version": VERSION,
                   "sandbox": base.sandbox_mode(),
                   "unsandboxed_allowed": os.environ.get("TASKLANE_ALLOW_UNSANDBOXED", "").strip() == "1",
                   "credential": all(os.environ.get(v) for v in credentials()) if credentials() else True,
                   "configured": bool(os.environ.get("TASKLANE_CLI_COMMAND", "").strip())}
        elif op == "capabilities":
            res = {"tool_use": True, "structured_output": False, "read_only_mode": base.sandbox_mode() == "docker"}
        elif op == "check":
            res = check()
        elif op == "sweep":
            older = (req.get("params") or {}).get("older_than_secs", 7200) if isinstance(req.get("params"), dict) else 7200
            res = base.sweep(older if isinstance(older, int) and not isinstance(older, bool) and older >= 60 else 7200)
        elif op == "run":
            params = req.get("params")
            if not isinstance(params, dict):
                raise EngineError("permanent", "params must be an object")
            res = run(params)
        else:
            raise EngineError("permanent", "unknown op: %s" % (op,))
        return {"id": rid, "ok": True, "result": res}
    except EngineError as e:
        err = {"kind": e.kind, "message": e.message}
        err.update(e.extra)
        return {"id": rid, "ok": False, "error": err}
    except Exception as e:  # never crash the loop
        return {"id": rid, "ok": False, "error": {"kind": "permanent", "message": "internal error: %s" % type(e).__name__}}


def main():
    if os.environ.get("TASKLANE_SANDBOX", "").strip().lower() != "docker":
        sys.stderr.write("engine-cli: the agent is NOT sandboxed (set TASKLANE_SANDBOX=docker)\n")
        sys.stderr.flush()
    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        try:
            resp = handle(json.loads(line))
        except ValueError as e:
            resp = {"id": None, "ok": False, "error": {"kind": "permanent", "message": "bad json: %s" % e}}
        sys.stdout.write(base.scrub(json.dumps(resp)) + "\n")
        sys.stdout.flush()


if __name__ == "__main__":
    main()
