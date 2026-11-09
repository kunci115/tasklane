#!/usr/bin/env python3
"""Ticket-source plug-in for GitLab (experimental): issues and merge requests, authenticated with an access token.

Protocol: one JSON object per line on stdin; one response per line on stdout; stderr is for logs.
Standard library only.

Environment:
  GITLAB_URL             default https://gitlab.com (the plug-in never follows links to another host)
  GITLAB_TOKEN           a group or project access token (or a bot user's token) with the `api` scope and the
                         Developer role. Use a dedicated bot, not a person: tasklane uses this token to read issues,
                         comment, push branches and merge. GitLab cannot mint a token that lives only minutes, so
                         `mint_run_token` hands out this same token; keep its reach as small as you can.
  GITLAB_WEBHOOK_SECRET  the secret token set on the webhook (`X-Gitlab-Token`)
  GITLAB_GROUP           optional; only projects under this group path are used

Secrets are never logged or returned in errors; every outgoing message passes through redact().
Differences from GitHub: a "pull request" is a merge request, a "ref" is the issue's iid, checks come from the
merge request's pipeline, and trust in an author means the Developer role or higher on the project.
"""
import hmac
import json
import os
import re
import sys
import time
import urllib.error
import urllib.parse
import urllib.request
import http.client
from datetime import datetime, timezone

sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "source-github"))
from source_github import parse_overrides  # noqa: E402  (one strict parser for the repository rules file)

PROTOCOL_MAJOR = 1
VERSION = "0.1.0"
TIMEOUT = 30
DEFAULT_URL = "https://gitlab.com"
DEVELOPER = 30
SECRETS = set()


def remember_secret(s):
    if s and isinstance(s, str) and len(s) >= 6:
        SECRETS.add(s)


def redact(text):
    text = str(text)
    for s in SECRETS:
        text = text.replace(s, "***")
    return re.sub(r"(oauth2:)[^@\s/]+@", r"\1***@", text)


class PluginError(Exception):
    def __init__(self, kind, message, status=None):
        super().__init__(message)
        self.kind, self.message, self.status = kind, redact(message), status


def permanent(msg, status=None):
    return PluginError("permanent", msg, status)


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, *a, **k):
        return None


OPENER = urllib.request.build_opener(NoRedirect)


def base_url():
    return os.environ.get("GITLAB_URL", DEFAULT_URL).rstrip("/")


def token():
    t = os.environ.get("GITLAB_TOKEN", "").strip()
    if not t:
        raise permanent("GITLAB_TOKEN is not set")
    remember_secret(t)
    return t


class Resp:
    def __init__(self, status, headers, data):
        self.status, self.headers, self.data = status, headers, data


def api(method, path, *, body=None, allow=(), params=None):
    url = base_url() + "/api/v4" + path
    if params:
        url += ("&" if "?" in url else "?") + urllib.parse.urlencode(params)
    hdrs = {"PRIVATE-TOKEN": token(), "Accept": "application/json", "User-Agent": "tasklane-source-gitlab/" + VERSION}
    data = None
    if body is not None:
        data = json.dumps(body).encode()
        hdrs["Content-Type"] = "application/json"
    req = urllib.request.Request(url, data=data, method=method, headers=hdrs)
    shown = path.split("?")[0]
    try:
        with OPENER.open(req, timeout=TIMEOUT) as r:
            status, rh, raw = r.status, {k.lower(): v for k, v in r.headers.items()}, r.read()
    except urllib.error.HTTPError as e:
        status, rh = e.code, {k.lower(): v for k, v in (e.headers.items() if e.headers else [])}
        try:
            raw = e.read()
        except Exception:
            raw = b""
    except (urllib.error.URLError, OSError, http.client.HTTPException) as e:
        raise PluginError("transient", "network error on %s %s: %s" % (method, shown, type(e).__name__))
    parsed = None
    if raw:
        try:
            parsed = json.loads(raw)
        except ValueError:
            parsed = raw.decode("utf-8", "replace")
    if 200 <= status < 300 or status in allow:
        return Resp(status, rh, parsed)
    msg = parsed.get("message") if isinstance(parsed, dict) else parsed
    if isinstance(msg, (dict, list)):
        msg = json.dumps(msg)
    desc = "GitLab %s %s -> %s: %s" % (method, shown, status, str(msg or "")[:200])
    if status == 429:
        raise PluginError("rate_limited", desc, status)
    if status in (401, 403):
        raise PluginError("unauthorized", desc, status)
    if status >= 500:
        raise PluginError("transient", desc, status)
    raise PluginError("permanent", desc, status)


def paginate(path, params=None, max_pages=50):
    out, page = [], 1
    for _ in range(max_pages):
        p = dict(params or {})
        p.update({"per_page": 100, "page": page})
        r = api("GET", path, params=p)
        out.extend(r.data if isinstance(r.data, list) else [])
        nxt = r.headers.get("x-next-page", "")
        if not nxt:
            return out
        page = int(nxt)
    raise permanent("too many pages")


def enc(svc):
    return urllib.parse.quote(svc, safe="")


def p_str(params, key):
    v = params.get(key)
    if not isinstance(v, str) or not v:
        raise permanent("missing param: %s" % key)
    return v


def p_service(params):
    s = p_str(params, "service")
    if not re.fullmatch(r"[A-Za-z0-9_.-]+(/[A-Za-z0-9_.-]+)+", s):
        raise permanent("service must look like group/project")
    return s


def p_num(params, key):
    v = params.get(key)
    if isinstance(v, int) and not isinstance(v, bool):
        v = str(v)
    if not isinstance(v, str) or not v.isdigit():
        raise permanent("param %s must be a numeric string" % key)
    return v


def now_iso():
    return datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


_cache = {"me": None, "access": {}}


def me():
    if _cache["me"] is None:
        _cache["me"] = (api("GET", "/user").data or {}).get("id")
    return _cache["me"]


def access_level(svc, user_id):
    """The user's role on the project (inherited roles included); 0 if they are not a member."""
    key = (svc, user_id)
    if key not in _cache["access"]:
        r = api("GET", "/projects/%s/members/all/%s" % (enc(svc), user_id), allow=(404,))
        _cache["access"][key] = int((r.data or {}).get("access_level", 0)) if r.status == 200 and isinstance(r.data, dict) else 0
    return _cache["access"][key]


def trusted(svc, user):
    """People with the Developer role or higher, and tasklane's own account."""
    if not isinstance(user, dict) or not user.get("id"):
        return False
    return user.get("id") == me() or access_level(svc, user["id"]) >= DEVELOPER


# ---------------------------------------------------------------- ops

def op_describe(params):
    return {"name": "gitlab", "role": "ticket_source", "protocol_major": PROTOCOL_MAJOR, "version": VERSION,
            "configured": bool(os.environ.get("GITLAB_TOKEN", "").strip())}


def visible_projects():
    projects = paginate("/projects", {"membership": "true", "min_access_level": DEVELOPER, "simple": "true"})
    group = os.environ.get("GITLAB_GROUP", "").strip().strip("/").lower()
    if group:
        projects = [p for p in projects if str(p.get("path_with_namespace", "")).lower().startswith(group + "/")]
    return projects


def op_list_services(params):
    return {"services": [{"external_id": p["path_with_namespace"], "default_branch": p.get("default_branch") or "main",
                          "archived": bool(p.get("archived")), "private": p.get("visibility") != "public"}
                         for p in visible_projects()]}


def op_list_changes(params):
    raw = params.get("cursor")
    if not raw:
        return {"items": [], "cursor": json.dumps({"base": now_iso(), "repos": {}}, sort_keys=True)}
    try:
        cur = json.loads(raw)
        assert isinstance(cur, dict) and isinstance(cur.get("repos", {}), dict) and isinstance(cur["base"], str)
    except (ValueError, AssertionError, KeyError, TypeError):
        raise permanent("invalid cursor")
    items, repos = [], {}
    for p in visible_projects():
        name = p["path_with_namespace"]
        if p.get("archived"):
            continue
        since = (cur.get("repos", {}).get(name) or {}).get("since") or cur["base"]
        try:
            rows = paginate("/projects/%s/issues" % enc(name), {"updated_after": since, "order_by": "updated_at", "sort": "asc"}, max_pages=20)
        except PluginError as e:
            if e.status in (403, 404, 410):  # issues disabled, or this project is off limits: skip it
                continue
            raise
        newest = since
        for it in rows:
            ts = it.get("updated_at", "")
            items.append({"service": name, "ref": str(it["iid"]), "kind": "issue", "updated_at": ts})
            newest = max(newest, ts)
        repos[name] = {"since": newest}
    items.sort(key=lambda i: i["updated_at"])
    return {"items": items, "cursor": json.dumps({"base": cur["base"], "repos": repos}, sort_keys=True)}


def op_get_item(params):
    svc, ref = p_service(params), p_num(params, "ref")
    it = api("GET", "/projects/%s/issues/%s" % (enc(svc), ref)).data or {}
    notes = paginate("/projects/%s/issues/%s/notes" % (enc(svc), ref), {"sort": "asc"})
    comments = []
    for n in notes:
        if n.get("system"):
            continue
        a = n.get("author") or {}
        comments.append({"author": a.get("username", ""), "body": n.get("body") or "", "at": n.get("created_at", ""),
                         "mine": a.get("id") == me(), "trusted": trusted(svc, a)})
    author = it.get("author") or {}
    return {"title": it.get("title", ""), "body": it.get("description") or "", "author": author.get("username", ""),
            "labels": it.get("labels", []), "state": "closed" if it.get("state") == "closed" else "open",
            "is_pull_request": False, "author_trusted": trusted(svc, author), "comments": comments}


def op_create_item(params):
    svc = p_service(params)
    labels = params.get("labels") if isinstance(params.get("labels"), list) else []
    d = api("POST", "/projects/%s/issues" % enc(svc),
            body={"title": p_str(params, "title"), "description": params.get("body") or "", "labels": ",".join(map(str, labels))}).data or {}
    return {"ref": str(d["iid"])}


def op_comment(params):
    svc, ref = p_service(params), p_num(params, "ref")
    d = api("POST", "/projects/%s/issues/%s/notes" % (enc(svc), ref), body={"body": p_str(params, "body")}).data or {}
    return {"id": str(d.get("id", ""))}


def op_set_labels(params):
    svc, ref = p_service(params), p_num(params, "ref")
    add = ",".join(map(str, params.get("add") or []))
    remove = ",".join(map(str, params.get("remove") or []))
    body = {k: v for k, v in (("add_labels", add), ("remove_labels", remove)) if v}
    if body:
        api("PUT", "/projects/%s/issues/%s" % (enc(svc), ref), body=body)
    return {}


def default_branch(svc):
    return (api("GET", "/projects/%s" % enc(svc)).data or {}).get("default_branch") or "main"


def op_detect_facts(params):
    svc = p_service(params)
    out = {"environments": [], "protected_paths": [".gitlab-ci.yml", "Dockerfile", "migrations/", "db/migrate/", "terraform/"]}
    langs = api("GET", "/projects/%s/languages" % enc(svc), allow=(404,)).data
    if isinstance(langs, dict) and langs:
        out["language"] = max(langs, key=lambda k: langs[k])
    tree = api("GET", "/projects/%s/repository/tree" % enc(svc), params={"per_page": 100}, allow=(404,)).data
    names = {t.get("name") for t in tree} if isinstance(tree, list) else set()
    for marker, cmd in (("pytest.ini", "pytest"), ("package.json", "npm test"), ("Cargo.toml", "cargo test"),
                        ("go.mod", "go test ./..."), ("Makefile", "make test")):
        if marker in names:
            out["test_command"] = cmd
            break
    return out


def op_read_overrides(params):
    svc = p_service(params)
    r = api("GET", "/projects/%s/repository/files/%s/raw" % (enc(svc), enc(".tasklane.yml")),
            params={"ref": default_branch(svc)}, allow=(404,))
    if r.status == 404 or not isinstance(r.data, str):
        return {"overrides": {}, "found": False}
    out, ignored = parse_overrides(r.data)
    return {"overrides": out, "found": True, "ignored": ignored}


def op_repo_url(params):
    svc = p_service(params)
    u = urllib.parse.urlsplit(base_url())
    tok = params.get("token")
    if tok:
        if not isinstance(tok, str):
            raise permanent("token must be a string")
        remember_secret(tok)
        netloc = "oauth2:%s@%s" % (urllib.parse.quote(tok, safe=""), u.netloc)
    else:
        netloc = u.netloc
    return {"url": urllib.parse.urlunsplit((u.scheme, netloc, u.path.rstrip("/") + "/" + svc + ".git", "", ""))}


def op_mint_run_token(params):
    p_service(params)
    ttl = params.get("ttl_secs") if isinstance(params.get("ttl_secs"), int) else 3600
    # GitLab cannot mint a token that lives for minutes: this is the configured token (see the module notes)
    return {"token": token(), "expires_at": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime(time.time() + max(60, ttl)))}


def op_open_pr(params):
    svc = p_service(params)
    branch = p_str(params, "branch")
    body = {"source_branch": branch, "target_branch": p_str(params, "base"), "title": p_str(params, "title"),
            "description": params.get("body") or "", "remove_source_branch": False}
    try:
        d = api("POST", "/projects/%s/merge_requests" % enc(svc), body=body).data or {}
    except PluginError as e:
        if e.status != 409:
            raise
        found = api("GET", "/projects/%s/merge_requests" % enc(svc), params={"source_branch": branch, "state": "opened"}).data or []
        if not found:
            raise
        d = found[0]
    return {"pr": str(d["iid"])}


PENDING = {"created", "waiting_for_resource", "preparing", "pending", "running", "scheduled", "manual"}
GOOD = {"success", "skipped"}


def check_state(status):
    return "success" if status in GOOD else "pending" if status in PENDING else "failure"


def op_get_checks(params):
    svc, pr = p_service(params), p_num(params, "pr")
    mr = api("GET", "/projects/%s/merge_requests/%s" % (enc(svc), pr)).data or {}
    sha = mr.get("sha")
    if not sha:
        raise PluginError("transient", "merge request has no head sha")
    pipes = api("GET", "/projects/%s/merge_requests/%s/pipelines" % (enc(svc), pr)).data or []
    pipes = [p for p in pipes if p.get("sha") == sha]
    checks = []
    if pipes:
        pid = max(pipes, key=lambda p: p.get("id", 0))["id"]
        for j in paginate("/projects/%s/pipelines/%s/jobs" % (enc(svc), pid)):
            checks.append({"name": j.get("name", ""), "state": check_state(j.get("status"))})
    states = {c["state"] for c in checks}
    state = "failure" if "failure" in states else "pending" if "pending" in states else "success"
    return {"head_sha": sha, "state": state, "checks": checks}


def op_merge_pr(params):
    svc, pr, expected = p_service(params), p_num(params, "pr"), p_str(params, "expected_head_sha")
    path = "/projects/%s/merge_requests/%s" % (enc(svc), pr)
    mr = api("GET", path).data or {}
    if mr.get("sha") != expected:
        return {"merged": False, "reason": "head_moved"}
    if mr.get("state") == "merged":
        out = {"merged": True}
        if mr.get("merge_commit_sha"):
            out["sha"] = mr["merge_commit_sha"]
        return out
    if mr.get("state") == "closed":
        return {"merged": False, "reason": "not_mergeable"}
    try:
        r = api("PUT", path + "/merge", body={"sha": expected, "should_remove_source_branch": False}).data or {}
    except PluginError as e:
        if e.status == 409:
            return {"merged": False, "reason": "head_moved"}
        if e.status in (405, 406):
            low = e.message.lower()
            reason = "conflict" if "conflict" in low or "cannot be merged" in low else \
                "checks_failing" if "pipeline" in low or "check" in low else "not_mergeable"
            return {"merged": False, "reason": reason}
        raise
    out = {"merged": r.get("state") == "merged"}
    if r.get("merge_commit_sha"):
        out["sha"] = r["merge_commit_sha"]
    return out


def op_list_pr_feedback(params):
    """What people with the Developer role or higher said on a merge request since `since`."""
    svc, pr = p_service(params), p_num(params, "pr")
    since = params.get("since") or ""
    if since and not isinstance(since, str):
        raise permanent("since must be an ISO time")
    items = []
    for n in paginate("/projects/%s/merge_requests/%s/notes" % (enc(svc), pr), {"sort": "asc"}):
        a = n.get("author") or {}
        body, at = (n.get("body") or "").strip(), n.get("created_at", "")
        if n.get("system") or not body or not at or at <= since or a.get("id") == me() or not trusted(svc, a):
            continue
        items.append({"author": a.get("username", ""), "body": body[:2000], "at": at,
                      "kind": "line" if n.get("type") == "DiffNote" else "comment"})
    items.sort(key=lambda i: i["at"])
    items = items[-20:]
    return {"items": items, "cursor": items[-1]["at"] if items else since}


def op_verify_webhook(params):
    secret = os.environ.get("GITLAB_WEBHOOK_SECRET", "")
    if not secret:
        raise permanent("GITLAB_WEBHOOK_SECRET is not set")
    remember_secret(secret)
    headers = params.get("headers")
    if not isinstance(headers, dict):
        raise permanent("headers must be an object")
    h = {str(k).lower(): str(v) for k, v in headers.items()}
    if not hmac.compare_digest(secret.encode(), h.get("x-gitlab-token", "").encode()):
        raise permanent("webhook token mismatch")
    import base64
    try:
        payload = json.loads(base64.b64decode(p_str(params, "body_b64"), validate=True))
        assert isinstance(payload, dict)
    except Exception:
        raise permanent("webhook body is not a JSON object")
    service = (payload.get("project") or {}).get("path_with_namespace")
    if not service:
        raise permanent("webhook event has no project")
    event = h.get("x-gitlab-event", "")
    attrs = payload.get("object_attributes") or {}
    if event == "Issue Hook":
        ref, kind = attrs.get("iid"), "issue"
    elif event == "Note Hook":
        if attrs.get("noteable_type") == "Issue":
            ref, kind = (payload.get("issue") or {}).get("iid"), "comment"
        elif attrs.get("noteable_type") == "MergeRequest":
            ref, kind = (payload.get("merge_request") or {}).get("iid"), "pr"
        else:
            raise permanent("note on something that is not a ticket")
    elif event == "Merge Request Hook":
        ref, kind = attrs.get("iid"), "pr"
    elif event == "Pipeline Hook":
        ref, kind = (payload.get("merge_request") or {}).get("iid"), "pr"
    else:
        raise permanent("unsupported event: %s" % event[:40])
    if ref is None:
        raise permanent("event has no ticket")
    return {"event": {"service": service, "ref": str(ref), "kind": kind}}


# ---- two installs on one project: an atomic branch claim, oldest wins (same idea as the GitHub plug-in)

OWNER_PREFIX = "tasklane-owner-"
INSTANCE_RE = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._-]{2,50}$")


def p_instance(params):
    v = p_str(params, "instance")
    if not INSTANCE_RE.match(v):
        raise permanent("instance must be 3 to 51 letters, digits, dots, dashes or underscores")
    return v


def owner_claims(svc):
    rows = api("GET", "/projects/%s/repository/branches" % enc(svc), params={"search": "^" + OWNER_PREFIX, "per_page": 100}, allow=(404,)).data
    out = []
    for b in rows if isinstance(rows, list) else []:
        m = re.match(r"^%s(\d{1,12})-(.+)$" % re.escape(OWNER_PREFIX), str(b.get("name", "")))
        if m:
            out.append((int(m.group(1)), m.group(2), b["name"]))
    return sorted(out)


def owner_report(claims, mine):
    if not claims:
        return {"owner": "", "mine": False, "others": [], "since": 0, "guard": "on"}
    win = claims[0]
    return {"owner": win[1], "mine": win[1] == mine, "since": win[0], "guard": "on",
            "others": [c[1] for c in claims if c[1] != win[1]]}


def delete_claim(svc, name):
    api("DELETE", "/projects/%s/repository/branches/%s" % (enc(svc), enc(name)), allow=(404,))


def op_claim_repo(params):
    svc, mine = p_service(params), p_instance(params)
    claims = owner_claims(svc)
    have = [c for c in claims if c[1] == mine]
    foreign_first = bool(claims) and claims[0][1] != mine
    if not have and not foreign_first:
        branch = default_branch(svc)
        head = api("GET", "/projects/%s/repository/branches/%s" % (enc(svc), enc(branch)), allow=(404,))
        if head.status != 200:  # an empty project has no commit to branch from: no guard is possible yet
            return {"owner": mine, "mine": True, "others": [], "since": 0, "guard": "unavailable"}
        api("POST", "/projects/%s/repository/branches" % enc(svc), allow=(400,),
            params={"branch": "%s%d-%s" % (OWNER_PREFIX, int(time.time()), mine), "ref": branch})
        claims = owner_claims(svc)
        have = [c for c in claims if c[1] == mine]
    if have and claims and claims[0][1] != mine:
        for c in have:
            delete_claim(svc, c[2])
        claims = [c for c in claims if c[1] != mine]
    return owner_report(claims, mine)


def op_release_repo(params):
    svc, mine = p_service(params), p_instance(params)
    for c in owner_claims(svc):
        if c[1] == mine:
            delete_claim(svc, c[2])
    return {}


def op_takeover_repo(params):
    svc, mine = p_service(params), p_instance(params)
    for c in owner_claims(svc):
        if c[1] != mine:
            delete_claim(svc, c[2])
    return op_claim_repo(params)


OPS = {"describe": op_describe, "list_services": op_list_services, "list_changes": op_list_changes,
       "get_item": op_get_item, "create_item": op_create_item, "comment": op_comment, "set_labels": op_set_labels,
       "detect_facts": op_detect_facts, "read_overrides": op_read_overrides, "repo_url": op_repo_url,
       "mint_run_token": op_mint_run_token, "open_pr": op_open_pr, "get_checks": op_get_checks,
       "merge_pr": op_merge_pr, "list_pr_feedback": op_list_pr_feedback, "verify_webhook": op_verify_webhook,
       "claim_repo": op_claim_repo, "release_repo": op_release_repo, "takeover_repo": op_takeover_repo}


def handle(req):
    rid = req.get("id") if isinstance(req, dict) else None
    try:
        if not isinstance(req, dict) or req.get("op") not in OPS:
            raise permanent("unknown op: %s" % (req.get("op") if isinstance(req, dict) else None))
        params = req.get("params") if isinstance(req.get("params"), dict) else {}
        return {"id": rid, "ok": True, "result": OPS[req["op"]](params)}
    except PluginError as e:
        return {"id": rid, "ok": False, "error": {"kind": e.kind, "message": e.message}}
    except Exception as e:  # never crash the loop
        return {"id": rid, "ok": False, "error": {"kind": "permanent", "message": "internal error: %s" % type(e).__name__}}


def main():
    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        try:
            resp = handle(json.loads(line))
        except ValueError:
            continue
        sys.stdout.write(json.dumps(resp) + "\n")  # errors are redacted when they are made; repo_url and mint_run_token carry a token on purpose
        sys.stdout.flush()


if __name__ == "__main__":
    main()
