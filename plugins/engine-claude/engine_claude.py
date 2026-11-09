#!/usr/bin/env python3
"""Agent engine plug-in for Claude (stage 1). Skeleton: speaks the protocol,
answers `describe` and `capabilities`, and refuses `run` until implemented.

Protocol: one JSON object per line on stdin; one response per line on stdout.
"""
import json
import sys

PROTOCOL_MAJOR = 1


def handle(req: dict) -> dict:
    rid, op = req.get("id"), req.get("op")
    if op == "describe":
        return {"id": rid, "ok": True, "result": {
            "name": "claude", "role": "agent_engine",
            "protocol_major": PROTOCOL_MAJOR, "version": "0.0.1"}}
    if op == "capabilities":
        return {"id": rid, "ok": True, "result": {
            "tool_use": True, "structured_output": True, "read_only_mode": True}}
    if op == "run":
        return {"id": rid, "ok": False, "error": {
            "kind": "permanent", "message": "run is not implemented yet"}}
    return {"id": rid, "ok": False, "error": {
        "kind": "permanent", "message": f"unknown op: {op}"}}


def main() -> None:
    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        try:
            resp = handle(json.loads(line))
        except json.JSONDecodeError as e:
            resp = {"id": None, "ok": False, "error": {"kind": "permanent", "message": f"bad json: {e}"}}
        sys.stdout.write(json.dumps(resp) + "\n")
        sys.stdout.flush()


if __name__ == "__main__":
    main()
