#!/usr/bin/env python3
"""A minimal ticket-source plug-in: tickets live in one JSON file.

Copy this directory to start your own plug-in (see docs/plugins.md). It speaks the tasklane plug-in
protocol: one JSON object per line on stdin (requests), one per line on stdout (responses), logs on stderr.
It implements only the operations needed to see tickets and comment on them, so tasklane can read and plan
(`triage` mode). A real source also implements repo_url, mint_run_token, open_pr, get_checks and merge_pr
(docs/design/06-plugin-operations.md).

Environment: FILE_SOURCE_PATH = the JSON file, shaped like
  {"repos": {"acme/shop": {"default_branch": "main", "issues": {"1": {"title": "...", "body": "...",
   "author": "ann", "state": "open", "labels": [], "comments": [], "updated_at": "2026-01-01T00:00:00Z"}}}}}
"""
import json
import os
import sys

PROTOCOL_MAJOR = 1
VERSION = "0.1.0"


class PluginError(Exception):
    def __init__(self, kind, message):
        super().__init__(message)
        self.kind, self.message = kind, message


def load():
    path = os.environ.get("FILE_SOURCE_PATH", "")
    if not path:
        raise PluginError("permanent", "FILE_SOURCE_PATH is not set")
    try:
        with open(path) as f:
            return json.load(f)
    except (OSError, ValueError) as e:
        raise PluginError("permanent", "cannot read %s: %s" % (path, type(e).__name__))


def save(data):
    path = os.environ["FILE_SOURCE_PATH"]
    tmp = path + ".tmp"
    with open(tmp, "w") as f:
        json.dump(data, f)
    os.replace(tmp, path)  # atomic, so a crash never leaves half a file


def need(params, key):
    v = params.get(key)
    if not isinstance(v, str) or not v:
        raise PluginError("permanent", "params.%s is required" % key)
    return v


def issue(data, params):
    repo = data.get("repos", {}).get(need(params, "service"))
    if repo is None:
        raise PluginError("permanent", "unknown service")
    item = repo.get("issues", {}).get(need(params, "ref"))
    if item is None:
        raise PluginError("permanent", "unknown ticket")
    return item


def op_describe(params):
    return {"name": "file", "role": "ticket_source", "protocol_major": PROTOCOL_MAJOR,
            "version": VERSION, "configured": bool(os.environ.get("FILE_SOURCE_PATH"))}


def op_list_services(params):
    repos = load().get("repos", {})
    return {"services": [{"external_id": name, "default_branch": r.get("default_branch", "main"),
                          "archived": bool(r.get("archived")), "private": True}
                         for name, r in sorted(repos.items())]}


def op_list_changes(params):
    """The cursor is the newest `updated_at` seen; the first call only sets it (no history import)."""
    cursor = params.get("cursor")
    items, newest = [], cursor or ""
    for name, repo in load().get("repos", {}).items():
        for ref, it in repo.get("issues", {}).items():
            ts = it.get("updated_at", "")
            newest = max(newest, ts)
            if cursor and ts > cursor:
                items.append({"service": name, "ref": ref, "kind": "issue", "updated_at": ts})
    return {"items": sorted(items, key=lambda i: i["updated_at"]), "cursor": newest}


def op_get_item(params):
    it = issue(load(), params)
    return {"title": it.get("title", ""), "body": it.get("body", ""), "author": it.get("author", ""),
            "labels": it.get("labels", []), "state": it.get("state", "open"),
            "is_pull_request": False, "author_trusted": bool(it.get("author_trusted", False)),
            "comments": [{"author": c.get("author", ""), "body": c.get("body", ""), "at": c.get("at", ""),
                          "mine": bool(c.get("mine")), "trusted": bool(c.get("trusted", False))}
                         for c in it.get("comments", [])]}


def op_comment(params):
    data = load()
    it = issue(data, params)
    it.setdefault("comments", []).append({"author": "tasklane", "body": need(params, "body"), "mine": True,
                                          "trusted": True, "at": ""})
    save(data)
    return {"id": str(len(it["comments"]))}


OPS = {"describe": op_describe, "list_services": op_list_services, "list_changes": op_list_changes,
       "get_item": op_get_item, "comment": op_comment}


def handle(req):
    rid = req.get("id") if isinstance(req, dict) else None
    try:
        if not isinstance(req, dict) or req.get("op") not in OPS:
            raise PluginError("permanent", "unknown op: %s" % (req.get("op") if isinstance(req, dict) else None))
        params = req.get("params") if isinstance(req.get("params"), dict) else {}
        return {"id": rid, "ok": True, "result": OPS[req["op"]](params)}
    except PluginError as e:
        return {"id": rid, "ok": False, "error": {"kind": e.kind, "message": e.message}}
    except Exception as e:  # never crash the loop: the core would see a dead plug-in
        return {"id": rid, "ok": False, "error": {"kind": "permanent", "message": "internal error: %s" % type(e).__name__}}


def main():
    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        try:
            req = json.loads(line)
        except ValueError:
            continue  # a line that is not JSON is ignored; the caller times out
        sys.stdout.write(json.dumps(handle(req)) + "\n")
        sys.stdout.flush()


if __name__ == "__main__":
    main()
