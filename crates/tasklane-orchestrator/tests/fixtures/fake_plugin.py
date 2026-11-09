#!/usr/bin/env python3
"""Fake source + engine plug-in for tests. Behaviour is chosen by the ticket ref."""
import json
import os
import sys
import time


def item(ref):
    body = "The button colour is wrong."
    if "risky" in ref:
        body = "Needs a database migration and a password reset flow."
    if "secret" in ref:
        body = "Rotate the api key."
    return {"title": f"Fix {ref}", "body": body}


def run(params):
    ref = params["ref"]
    if "slow" in ref:
        time.sleep(2.5)
    if "crash" in ref:
        os._exit(1)
    if "badjson" in ref:
        return {"nonsense": True}
    level = 3 if "l3" in ref else 1
    questions = ["Which page?"] if "ask" in ref else []
    return {"level": level, "plan": f"plan for {ref}", "questions": questions}


for line in sys.stdin:
    req = json.loads(line)
    rid, op, params = req["id"], req["op"], req.get("params") or {}
    if op == "get_item":
        resp = {"id": rid, "ok": True, "result": item(params["ref"])}
    elif op == "run":
        resp = {"id": rid, "ok": True, "result": run(params)}
    elif op == "echo":
        # out-of-order test: answer after `delay` seconds
        time.sleep(params.get("delay", 0))
        resp = {"id": rid, "ok": True, "result": params}
    else:
        resp = {"id": rid, "ok": False, "error": {"kind": "permanent", "message": f"unknown op {op}"}}
    sys.stdout.write(json.dumps(resp) + "\n")
    sys.stdout.flush()
