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
    return {"title": f"Fix {ref}", "body": body, "comments": comments}


def run(params):
    role = params.get("role", "triage")
    ref = params["ticket"]["ref"]
    if role == "triage":
        if "slow" in ref:
            time.sleep(2.5)
        if "crash" in ref:
            os._exit(1)
        if "badjson" in ref:
            return {"nonsense": True}
        level = 3 if "l3" in ref else 1
        questions = ["Which page?"] if "ask" in ref else []
        return {"level": level, "plan": f"plan for {ref}", "questions": questions}
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
        if "big" in ref:
            open(os.path.join(ws, "big.txt"), "w").write("".join(f"line {i}\n" for i in range(900)))
        open(os.path.join(ws, "fix.txt"), "w").write(f"fix for {ref} at {time.time()}\n")
        return {"summary": f"fixed {ref}"}
    if role == "reviewer":
        if "changes" in ref and once("changes"):
            return {"verdict": "changes", "notes": "please add a test"}
        if "escalate" in ref and once("escalate"):
            return {"verdict": "escalate", "notes": "touches billing"}
        return {"verdict": "approve", "notes": "looks fine"}
    raise ValueError(role)


def handle(op, params):
    if op == "get_item":
        return item(params["ref"])
    if op == "run":
        return run(params)
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
        open(os.path.join(STATE, "pr_branch"), "w").write(params["branch"])
        return {"pr": "7"}
    if op == "get_checks":
        branch = open(os.path.join(STATE, "pr_branch")).read()
        head = remote_head(params["service"], branch)
        if once("ci_pending"):
            return {"head_sha": head, "state": "pending", "checks": []}
        if once("ci_red") or flag("ci_always_red"):
            return {"head_sha": head, "state": "failure", "checks": [{"name": "tests", "state": "failure"}]}
        return {"head_sha": head, "state": "success", "checks": [{"name": "tests", "state": "success"}]}
    if op == "merge_pr":
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
    sys.stdout.write(json.dumps(resp) + "\n")
    sys.stdout.flush()
