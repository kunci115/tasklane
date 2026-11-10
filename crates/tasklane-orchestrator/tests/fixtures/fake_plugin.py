#!/usr/bin/env python3
"""Fake source + engine plug-in for tests. Behaviour is chosen by the ticket ref and by files in FAKE_STATE_DIR."""
import json
import os
import subprocess
import sys
import time

STATE = os.environ.get("FAKE_STATE_DIR", "")
REMOTE = os.environ.get("FAKE_REMOTE_ROOT", "")


def flag(name):
    return os.path.exists(os.path.join(STATE, name))


def once(name):
    """True the first time it is called for a marker that the test armed as `arm_<name>`."""
    arm, done = os.path.join(STATE, "arm_" + name), os.path.join(STATE, "done_" + name)
    if os.path.exists(arm) and not os.path.exists(done):
        open(done, "w").close()
        return True
    return False


class UsedUp(Exception):
    """Every model account is used up (what engine-claude reports when its pool is empty)."""


def remote_head(service, branch):
    out = subprocess.run(["git", f"--git-dir={REMOTE}/{service}.git", "rev-parse", f"refs/heads/{branch}"],
                         capture_output=True, text=True)
    return out.stdout.strip()


def read_json(name, default):
    try:
        return json.load(open(os.path.join(STATE, name)))
    except FileNotFoundError:
        return default


def item(ref):
    comments = read_json("comments_" + ref + ".json", [])
    body = "The button colour is wrong."
    if "risky" in ref:
        body = "Needs a database migration and a password reset flow."
    out = {"title": f"Fix {ref}", "body": body, "comments": comments, "state": "open", "is_pull_request": False}
    if "credit" in ref:
        out["author"] = "nabil"
    if "stranger" in ref:
        out["author_trusted"] = False
    if ref.startswith("pr-"):
        out["is_pull_request"] = True
    if ref.startswith("closed-"):
        out["state"] = "closed"
    return out


def run(params):
    role = params.get("role", "triage")
    with open(os.path.join(STATE, "profile.log"), "a") as f:
        f.write(json.dumps({"role": role, "system": (params.get("profile") or {}).get("system", "")}) + "\n")
    ref = params["ticket"]["ref"]
    if role == "duplicate":
        with open(os.path.join(STATE, "dup_calls.log"), "a") as f:
            f.write(ref + "\n")
        if "modeldup" in ref:
            return {"duplicate_of": params["candidates"][0]["ref"], "reason": "same"}
        if "hallucinate" in ref:
            return {"duplicate_of": "999", "reason": "invented"}
        return {"duplicate_of": None, "reason": ""}
    if role == "triage":
        ws = params.get("workspace")
        with open(os.path.join(STATE, "triage_ws.log"), "a") as f:
            f.write(json.dumps({"ref": ref, "ws": ws, "readme": bool(ws) and os.path.exists(os.path.join(ws, "README"))}) + "\n")
        if "slow" in ref:
            time.sleep(2.5)
        if "usedup" in ref and once("used_up"):
            raise UsedUp()
        if "crash" in ref:
            os._exit(1)
        if "badjson" in ref:
            return {"nonsense": True}
        level = 3 if "l3" in ref else 1
        questions = ["Which page?"] if "ask" in ref else []
        out = {"level": level, "plan": f"plan for {ref}", "questions": questions}
        if "costly" in ref:
            out["usage"] = {"tokens_in": 900, "tokens_out": 200}
        return out
    if role == "dev":
        ws = params["workspace"]
        if flag("feedback_log") is False:
            pass
        with open(os.path.join(STATE, "dev_calls.log"), "a") as f:
            f.write(json.dumps({"ref": ref, "feedback": params.get("feedback")}) + "\n")
        if "nochange" in ref:
            return {"summary": "did nothing"}
        if "protected" in ref:
            os.makedirs(os.path.join(ws, ".github", "workflows"), exist_ok=True)
            open(os.path.join(ws, ".github", "workflows", "ci.yml"), "w").write(f"changed {time.time()}\n")
        if "unicode" in ref:
            os.makedirs(os.path.join(ws, ".github", "workflows"), exist_ok=True)
            open(os.path.join(ws, ".github", "workflows", "\u00e9.yml"), "w").write("on: push\n")
        if "moveaway" in ref:
            os.rename(os.path.join(ws, ".github", "workflows", "ci.yml"), os.path.join(ws, "docs-ci.txt"))
        if "nested" in ref:
            os.makedirs(os.path.join(ws, "apps", "x", "migrations"), exist_ok=True)
            open(os.path.join(ws, "apps", "x", "migrations", "0001.sql"), "w").write("select 1;\n")
        if "big" in ref:
            open(os.path.join(ws, "big.txt"), "w").write("".join(f"line {i}\n" for i in range(900)))
        open(os.path.join(ws, "fix.txt"), "w").write(f"fix for {ref} at {time.time()}\n")
        return {"summary": f"fixed {ref}"}
    if role == "ask":
        if flag("ask_fails"):
            raise ValueError("model down")
        with open(os.path.join(STATE, "ask_calls.log"), "a") as f:
            f.write(json.dumps({"q": params.get("question"), "ws": params.get("workspace")}) + "\n")
        return {"answer": "The answer is 42."}
    if role == "reviewer":
        with open(os.path.join(STATE, "review_diffs.log"), "a") as f:
            f.write(params.get("diff", "") + "\n")
        if "changes" in ref and once("changes"):
            return {"verdict": "changes", "notes": "please add a test"}
        if "escalate" in ref and once("escalate"):
            return {"verdict": "escalate", "notes": "touches billing"}
        return {"verdict": "approve", "notes": "looks fine", "complexity": "O(n) in the changed lines"}
    raise ValueError(role)


def owner_call(op, params):
    """The repository-ownership guard: a test arms `owner.json` with {"mine": false, "owner": "other_1234abcd"}."""
    with open(os.path.join(STATE, "owner_calls.log"), "a") as f:
        f.write(op + " " + params.get("service", "") + "\n")
    if op == "release_repo":
        return {}
    o = read_json("owner.json", {"mine": True, "owner": params.get("instance", "")})
    if op == "takeover_repo":
        o = {"mine": True, "owner": params.get("instance", "")}
        json.dump(o, open(os.path.join(STATE, "owner.json"), "w"))
    return {"owner": o.get("owner", ""), "mine": o.get("mine", True), "others": [], "since": 0, "guard": "on"}


def accounts_file():
    return os.path.join(STATE, "accounts.json")


def accounts_op(op, params):
    """A list of model accounts like engine-claude keeps (without any real secret handling)."""
    rows = json.load(open(accounts_file())) if os.path.exists(accounts_file()) else []
    if op == "accounts":
        return {"configured": True, "rows": [dict(r, token="\u2026" + r["token"][-4:]) for r in rows]}
    act, label = params.get("action"), params.get("label")
    i = next((n for n, r in enumerate(rows) if r["label"] == label), None)
    if act == "add":
        if i is not None:
            raise ValueError(f"an account called {label!r} already exists")
        if len(params.get("token") or "") < 20:
            raise ValueError("that does not look like a token")
        rows.append({"label": label, "kind": params["kind"], "token": params["token"], "enabled": True,
                     "cooldown_secs": 0, "last_error": "", "removable": True})
    elif i is None:
        raise ValueError(f"no account called {label!r}")
    elif act == "remove":
        rows.pop(i)
    elif act in ("enable", "disable"):
        rows[i]["enabled"] = act == "enable"
    elif act in ("up", "down"):
        j = i + (-1 if act == "up" else 1)
        if 0 <= j < len(rows):
            rows[i], rows[j] = rows[j], rows[i]
    json.dump(rows, open(accounts_file(), "w"))
    return {"ok": True}


def handle(op, params):
    if op in ("accounts", "account"):
        return accounts_op(op, params)
    if op in ("claim_repo", "takeover_repo", "release_repo"):
        return owner_call(op, params)
    if op == "describe":
        # like the real plug-ins: report what the environment they were started with says
        out = {"name": "fake", "version": "0.0.0", "role": "test",
                "sandbox": os.environ.get("TASKLANE_SANDBOX", "none"),
                "unsandboxed_allowed": os.environ.get("TASKLANE_ALLOW_UNSANDBOXED") == "1",
                "credential": bool(os.environ.get("ANTHROPIC_API_KEY")),
                "configured": bool(os.environ.get("GITHUB_APP_ID"))}
        if os.environ.get("FAKE_PROTOCOL_MAJOR"):
            out["protocol_major"] = int(os.environ["FAKE_PROTOCOL_MAJOR"])
        return out
    if op == "app_manifest_convert":
        if params["code"].startswith("bad"):
            raise ValueError("code refused")
        return {"app_id": "4242", "slug": "tasklane-test", "name": "tasklane-test", "owner": "someone", "owner_type": "User",
                "html_url": "https://github.com/apps/tasklane-test", "client_id": "Iv1.fake",
                "pem": "-----BEGIN RSA PRIVATE KEY-----\nFAKEPEMDATA\n-----END RSA PRIVATE KEY-----\n", "webhook_secret": "fake-webhook-secret-value"}
    if op == "list_installations":
        return {"installations": read_json("installations.json", []), "owner": "someone", "owner_type": "User"}
    if op == "check":
        return read_json("check.json", {"ok": True, "answer": "ok", "secs": 1.2, "sandbox": "none"})
    if op == "list_pr_feedback":
        fb = read_json("feedback.json", {"items": []})
        since = params.get("since") or ""
        items = [i for i in fb.get("items", []) if i["at"] > since]
        return {"items": items, "cursor": items[-1]["at"] if items else since}
    if op == "read_overrides":
        return {"overrides": read_json("overrides.json", {}), "found": True}
    if op == "get_item":
        return item(params["ref"])
    if op == "get_user":
        ids = {"nabil": 4242, "rino": 777}
        if params["login"] not in ids or flag("no_user_lookup"):
            raise ValueError("not a user account")
        return {"id": ids[params["login"]], "login": params["login"]}
    if op == "run":
        return run(params)
    if op == "receive":
        events = read_json("events.json", [])
        if events:
            os.rename(os.path.join(STATE, "events.json"), os.path.join(STATE, "events_done.json"))
        return {"events": events}
    if op == "send":
        sent = read_json("sent.json", []) + [params]
        json.dump(sent, open(os.path.join(STATE, "sent.json"), "w"))
        return {"message_id": str(len(sent))}
    if op == "leave":
        left = read_json("left.json", []) + [params["group"]]
        json.dump(left, open(os.path.join(STATE, "left.json"), "w"))
        return {}
    if op == "create_item":
        n = len(read_json("created.json", [])) + 100
        made = read_json("created.json", []) + [{"title": params["title"], "body": params["body"], "labels": params.get("labels")}]
        json.dump(made, open(os.path.join(STATE, "created.json"), "w"))
        return {"ref": str(n)}
    if op == "verify_webhook":
        if params["headers"].get("x-sig") != "good":
            raise KeyError("bad signature")
        import base64
        return {"event": json.loads(base64.b64decode(params["body_b64"]))}
    if op == "list_services":
        return {"services": read_json("services.json", [])}
    if op == "create_repo":
        if flag("create_denied"):
            raise ValueError("the GitHub App may not create repositories: give it the Administration permission")
        full = params["owner"] + "/" + params["name"]
        with open(os.path.join(STATE, "created_repo.json"), "w") as f:
            f.write(json.dumps(params))
        return {"external_id": full, "html_url": "https://github.com/" + full, "default_branch": "main",
                "private": params.get("private", True), "seeded": ["README.md", ".tasklane.yml"], "seed_error": ""}
    if op == "get_pull":
        return read_json("pull_" + str(params["pr"]) + ".json", {"base": "main", "state": "open"})
    if op == "list_pulls":
        return {"pulls": read_json("pulls_" + params.get("service", "").replace("/", "_") + ".json", []), "truncated": False}
    if op == "list_changes":
        items = read_json("changes.json", [])
        return {"items": items, "cursor": "c" + str(len(items))}
    if op == "detect_facts":
        return {"language": "python", "test_command": "pytest", "environments": [], "protected_paths": [".github/workflows/"]}
    if op == "echo":
        time.sleep(params.get("delay", 0))
        return params
    if op == "mint_run_token":
        return {"token": "tok-" + "-".join(params["permissions"]), "expires_at": "later"}
    if op == "repo_url":
        return {"url": f"file://{REMOTE}/{params['service']}.git"}
    if op == "comment":
        with open(os.path.join(STATE, "comments.log"), "a") as f:
            f.write(json.dumps({"ref": params["ref"], "body": params["body"]}) + "\n")
        return {"id": "1"}
    if op == "open_pr":
        # like GitHub: a second pull request for the same branch is refused
        if flag("pr_branch"):
            raise ValueError("A pull request already exists")
        open(os.path.join(STATE, "pr_branch"), "w").write(params["branch"])
        open(os.path.join(STATE, "pr_body"), "w").write(params.get("body") or "")
        return {"pr": "7"}
    if op == "get_checks":
        branch = open(os.path.join(STATE, "pr_branch")).read()
        head = remote_head(params["service"], branch)
        if once("ci_pending"):
            return {"head_sha": head, "state": "pending", "checks": []}
        if once("ci_red") or flag("ci_always_red"):
            return {"head_sha": head, "state": "failure", "checks": [{"name": "tests", "state": "failure", "output": "FAILED test_login: expected 200, got 500"}]}
        return {"head_sha": head, "state": "success", "checks": [{"name": "tests", "state": "success"}]}
    if op == "merge_pr":
        if not os.path.exists(os.path.join(STATE, "pr_branch")):
            # a person's pull request merged from the console: no ticket branch to compare
            if flag("merge_refuse"):
                return {"merged": False, "reason": "checks_failing"}
            open(os.path.join(STATE, "merged"), "w").write(params["token"])
            return {"merged": True, "sha": "abc"}
        branch = open(os.path.join(STATE, "pr_branch")).read()
        if once("head_moves") or remote_head(params["service"], branch) != params["expected_head_sha"]:
            return {"merged": False, "reason": "head_moved"}
        open(os.path.join(STATE, "merged"), "w").write(params["token"])
        return {"merged": True, "sha": "abc"}
    raise KeyError(op)


for line in sys.stdin:
    req = json.loads(line)
    try:
        resp = {"id": req["id"], "ok": True, "result": handle(req["op"], req.get("params") or {})}
    except KeyError as e:
        resp = {"id": req["id"], "ok": False, "error": {"kind": "permanent", "message": f"unknown op {e}"}}
    except UsedUp:
        resp = {"id": req["id"], "ok": False, "error": {"kind": "rate_limited", "message": "every model account is used up", "retry_after_secs": 60}}
    except ValueError as e:
        resp = {"id": req["id"], "ok": False, "error": {"kind": "permanent", "message": str(e)}}
    sys.stdout.write(json.dumps(resp) + "\n")
    sys.stdout.flush()
