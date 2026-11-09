#!/usr/bin/env python3
"""Ticket-source plug-in for GitHub (stage 1), authenticated as a GitHub App.

Protocol: one JSON object per line on stdin; one response per line on stdout;
stderr is for logs. Standard library only; RS256 signing shells out to openssl.

Environment:
  GITHUB_API_URL               default https://api.github.com
  GITHUB_APP_ID                numeric App id (JWT issuer)
  GITHUB_APP_PRIVATE_KEY_FILE  path to the App's PEM private key
  GITHUB_INSTALLATION_ID       optional; discovered via /app/installations
  GITHUB_WEBHOOK_SECRET        shared secret for verify_webhook
  GITHUB_ORG                   optional; only this owner's repos are used

Secrets (tokens, key contents, webhook secret) are never logged or returned
in errors; every outgoing message passes through redact().
"""
import base64
import binascii
import calendar
import hashlib
import hmac
import json
import os
import re
import subprocess
import sys
import time
import urllib.error
import urllib.parse
import urllib.request
import http.client
from datetime import datetime, timezone

PROTOCOL_MAJOR = 1
VERSION = "0.1.0"
TIMEOUT = 30
DEFAULT_API = "https://api.github.com"
DEFAULT_PROTECTED = [".github/workflows/", "Dockerfile", "migrations/",
                     "db/migrate/", "terraform/"]
BAD_CHECKS = {"failure", "timed_out", "cancelled", "action_required",
              "startup_failure", "stale"}
OK_CHECKS = {"success", "neutral", "skipped"}

SECRETS = set()  # every secret string seen in this process, for redaction


def remember_secret(s):
    if s and isinstance(s, str) and len(s) >= 6:
        SECRETS.add(s)


def redact(text):
    text = str(text)
    for s in SECRETS:
        text = text.replace(s, "***")
    return re.sub(r"(x-access-token:)[^@\s/]+@", r"\1***@", text)


def log(msg):
    sys.stderr.write(redact(msg) + "\n")
    sys.stderr.flush()


class PluginError(Exception):
    def __init__(self, kind, message, status=None):
        super().__init__(message)
        self.kind, self.message, self.status = kind, message, status


def permanent(msg):
    return PluginError("permanent", msg)


# ---------------------------------------------------------------- config

def api_base():
    return os.environ.get("GITHUB_API_URL", DEFAULT_API).rstrip("/")


def need_env(name):
    v = os.environ.get(name, "")
    if not v:
        raise permanent(f"{name} is not set")
    return v


def org_filter():
    return os.environ.get("GITHUB_ORG", "").strip().lower()


# ---------------------------------------------------------------- params

REPO_RE = re.compile(r"^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$")


def p_str(params, key, required=True):
    v = params.get(key)
    if v is None or v == "":
        if required:
            raise permanent(f"missing param: {key}")
        return None
    if not isinstance(v, str):
        raise permanent(f"param {key} must be a string")
    return v


def p_service(params):
    s = p_str(params, "service")
    if not REPO_RE.match(s) or s.split("/")[0] in (".", "..") or s.split("/")[1] in (".", ".."):
        raise permanent("service must look like owner/name")
    org = org_filter()
    if org and s.split("/")[0].lower() != org:
        raise permanent("service is outside the configured GITHUB_ORG")
    return s


def p_num(params, key):
    v = params.get(key)
    if isinstance(v, int) and not isinstance(v, bool):
        v = str(v)
    if not isinstance(v, str) or not v.isdigit():
        raise permanent(f"param {key} must be a numeric string")
    return v


def p_list(params, key):
    v = params.get(key, [])
    if not isinstance(v, list) or not all(isinstance(x, str) for x in v):
        raise permanent(f"param {key} must be a list of strings")
    return v


# ---------------------------------------------------------------- http

class _NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, *a, **kw):
        return None  # never replay Authorization on another URL


OPENER = urllib.request.build_opener(_NoRedirect)


class Resp:
    def __init__(self, status, headers, data):
        self.status, self.headers, self.data = status, headers, data


def _rate_limit_message(headers):
    now = time.time()
    wait = None
    ra = headers.get("retry-after")
    if ra and ra.isdigit():
        wait = int(ra)
    elif headers.get("x-ratelimit-reset", "").isdigit():
        wait = max(0, int(headers["x-ratelimit-reset"]) - int(now))
    if wait is None:
        return "GitHub rate limit hit; retry later"
    return f"GitHub rate limit hit; retry after {wait} seconds"


def _error_for(status, headers, data, method, path):
    msg = ""
    if isinstance(data, dict):
        msg = str(data.get("message", ""))
    desc = f"GitHub {method} {path} -> {status}" + (f": {msg}" if msg else "")
    h = {k.lower(): v for k, v in headers.items()}
    if status == 429 or (status == 403 and (
            h.get("x-ratelimit-remaining") == "0" or "retry-after" in h)):
        return PluginError("rate_limited", _rate_limit_message(h), status)
    if status in (401, 403):
        return PluginError("unauthorized", desc, status)
    if status >= 500:
        return PluginError("transient", desc, status)
    return PluginError("permanent", desc, status)


def api(method, path, *, auth=None, body=None, headers=None, allow=()):
    """One request. `auth` is a bearer token. Returns Resp for 2xx and for the
    statuses in `allow`; raises PluginError otherwise."""
    base = api_base()
    if path.startswith("http://") or path.startswith("https://"):
        if not (path == base or path.startswith(base + "/")):
            raise permanent("refusing to follow a link outside the API URL")
        url = path
        shown = path[len(base):]
    else:
        url = base + path
        shown = path
    shown = shown.split("?")[0]
    hdrs = {"Accept": "application/vnd.github+json",
            "X-GitHub-Api-Version": "2022-11-28",
            "User-Agent": "tasklane-source-github/" + VERSION}
    if auth:
        remember_secret(auth)
        hdrs["Authorization"] = "Bearer " + auth
    hdrs.update(headers or {})
    data = None
    if body is not None:
        data = json.dumps(body).encode()
        hdrs["Content-Type"] = "application/json"
    req = urllib.request.Request(url, data=data, method=method, headers=hdrs)
    try:
        with OPENER.open(req, timeout=TIMEOUT) as r:
            status, rh, raw = r.status, dict(r.headers.items()), r.read()
    except urllib.error.HTTPError as e:
        status, rh = e.code, dict(e.headers.items()) if e.headers else {}
        try:
            raw = e.read()
        except Exception:
            raw = b""
    except (urllib.error.URLError, OSError, http.client.HTTPException) as e:
        raise PluginError("transient", redact(f"network error on {method} {shown}: {type(e).__name__}"))
    rh = {k.lower(): v for k, v in rh.items()}
    parsed = None
    if raw:
        try:
            parsed = json.loads(raw)
        except ValueError:
            parsed = None
            if 200 <= status < 300:
                raise PluginError("transient", f"non-JSON response on {method} {shown}")
    if 200 <= status < 300 or status in allow:
        return Resp(status, rh, parsed)
    if 300 <= status < 400:
        raise permanent(f"GitHub {method} {shown} -> {status} redirect (not followed)")
    raise _error_for(status, rh, parsed, method, shown)


def next_link(headers):
    for part in headers.get("link", "").split(","):
        m = re.match(r'\s*<([^>]+)>\s*;\s*rel="next"', part)
        if m:
            return m.group(1)
    return None


def paginate(path, *, auth, key=None, max_pages=100):
    out = []
    url = path
    for _ in range(max_pages):
        r = api("GET", url, auth=auth)
        data = r.data
        if key:
            data = (data or {}).get(key, [])
        out.extend(data or [])
        url = next_link(r.headers)
        if not url:
            return out
    raise permanent("too many pages")


# ---------------------------------------------------------------- app auth

def b64url(b):
    return base64.urlsafe_b64encode(b).rstrip(b"=").decode()


def make_jwt():
    app_id = need_env("GITHUB_APP_ID")
    key_file = need_env("GITHUB_APP_PRIVATE_KEY_FILE")
    now = int(time.time())
    head = b64url(json.dumps({"alg": "RS256", "typ": "JWT"}, separators=(",", ":")).encode())
    pay = b64url(json.dumps({"iat": now - 60, "exp": now + 540, "iss": app_id},
                            separators=(",", ":")).encode())
    signing_input = f"{head}.{pay}".encode()
    try:
        p = subprocess.run(["openssl", "dgst", "-sha256", "-sign", key_file],
                           input=signing_input, capture_output=True, timeout=15)
    except FileNotFoundError:
        raise permanent("openssl is not installed")
    except subprocess.TimeoutExpired:
        raise PluginError("transient", "openssl timed out while signing")
    if p.returncode != 0:
        raise permanent("could not sign the App JWT (check GITHUB_APP_PRIVATE_KEY_FILE)")
    token = f"{head}.{pay}.{b64url(p.stdout)}"
    remember_secret(token)
    return token


def parse_time(s):
    return calendar.timegm(time.strptime(s, "%Y-%m-%dT%H:%M:%SZ"))


_state = {"installation_id": None, "token": None, "token_exp": 0}


def installation_id():
    env = os.environ.get("GITHUB_INSTALLATION_ID", "").strip()
    if env:
        return env
    if _state["installation_id"]:
        return _state["installation_id"]
    insts = paginate("/app/installations?per_page=100", auth=make_jwt())
    org = org_filter()
    if org:
        insts = [i for i in insts if str((i.get("account") or {}).get("login", "")).lower() == org]
    if not insts:
        raise PluginError("unauthorized", "the GitHub App has no matching installation")
    _state["installation_id"] = str(insts[0]["id"])
    return _state["installation_id"]


def mint(body=None):
    r = api("POST", f"/app/installations/{installation_id()}/access_tokens",
            auth=make_jwt(), body=body)
    d = r.data or {}
    if not d.get("token"):
        raise PluginError("transient", "installation token response had no token")
    remember_secret(d["token"])
    return d["token"], d.get("expires_at", "")


def app_token():
    """Installation token with default permissions, cached until 60 s before expiry."""
    if _state["token"] and time.time() < _state["token_exp"] - 60:
        return _state["token"]
    tok, exp = mint()
    try:
        _state["token_exp"] = parse_time(exp)
    except ValueError:
        _state["token_exp"] = time.time() + 300
    _state["token"] = tok
    return tok


def app_api(method, path, **kw):
    """Request with the app installation token; one retry after a 401."""
    try:
        return api(method, path, auth=app_token(), **kw)
    except PluginError as e:
        if e.status == 401:
            _state["token"] = None
            return api(method, path, auth=app_token(), **kw)
        raise


def app_paginate(path, key=None):
    try:
        return paginate(path, auth=app_token(), key=key)
    except PluginError as e:
        if e.status == 401:
            _state["token"] = None
            return paginate(path, auth=app_token(), key=key)
        raise


def q(s):
    return urllib.parse.quote(s, safe="")


# ---------------------------------------------------------------- ops

def op_describe(params):
    key = os.environ.get("GITHUB_APP_PRIVATE_KEY_FILE", "")
    configured = bool(os.environ.get("GITHUB_APP_ID") and key and os.path.isfile(key))
    return {"name": "github", "role": "ticket_source",
            "protocol_major": PROTOCOL_MAJOR, "version": VERSION, "configured": configured}


MANIFEST_CODE = re.compile(r"^[A-Za-z0-9_-]{8,80}$")


def op_app_manifest_convert(params):
    """Exchange the one-time code GitHub hands back after the user created an App from a manifest
    for the App's credentials. The call needs no authentication. The result holds secrets (the
    private key and the webhook secret) for the core to store; they are never logged."""
    code = p_str(params, "code")
    if not MANIFEST_CODE.match(code):
        raise permanent("that is not a valid manifest code")
    d = api("POST", f"/app-manifests/{code}/conversions").data or {}
    for k in ("pem", "webhook_secret", "client_secret"):
        remember_secret(d.get(k))
    if not d.get("id") or not d.get("pem"):
        raise permanent("GitHub did not return the App credentials (the code may have been used already)")
    return {"app_id": str(d["id"]), "slug": d.get("slug", ""), "name": d.get("name", ""),
            "owner": (d.get("owner") or {}).get("login", ""),
            "owner_type": (d.get("owner") or {}).get("type", ""), "html_url": d.get("html_url", ""),
            "client_id": d.get("client_id", ""), "pem": d["pem"],
            "webhook_secret": d.get("webhook_secret") or ""}


def op_list_installations(params):
    """Where the App is installed (for the setup page): id, account, and whether all or selected repositories."""
    items = paginate("/app/installations?per_page=100", auth=make_jwt())
    app = api("GET", "/app", auth=make_jwt()).data or {}
    return {"installations": [{"id": str(i["id"]), "account": (i.get("account") or {}).get("login", ""),
                               "repository_selection": i.get("repository_selection", "")} for i in items],
            "owner": (app.get("owner") or {}).get("login", ""), "owner_type": (app.get("owner") or {}).get("type", "")}


# ---------------------------------------------------------------- repository ownership
# Two tasklane installs on the same repository would both answer every issue. Each install that is
# allowed to act on a repository holds a ref `refs/tasklane/owner/<epoch>-<instance>`. Creating a ref is
# atomic on GitHub, the oldest claim wins, and every install computes the same winner, so no server is needed.
OWNER_PREFIX = "tasklane/owner/"
INSTANCE_RE = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._-]{2,50}$")


def p_instance(params):
    v = p_str(params, "instance")
    if not INSTANCE_RE.match(v):
        raise permanent("instance must be 3 to 51 letters, digits, dots, dashes or underscores")
    return v


def owner_claims(svc):
    """The claims on the repository, oldest first: [(epoch, instance, ref_name)]."""
    r = app_api("GET", f"/repos/{svc}/git/matching-refs/{OWNER_PREFIX}", allow=(404,))
    out = []
    for x in (r.data if isinstance(r.data, list) else []):
        name = str(x.get("ref", ""))[len("refs/" + OWNER_PREFIX):]
        m = re.match(r"^(\d{1,12})-(.+)$", name)
        if str(x.get("ref", "")).startswith("refs/" + OWNER_PREFIX) and m:
            out.append((int(m.group(1)), m.group(2), name))
    return sorted(out)


def owner_report(claims, me):
    if not claims:
        return {"owner": "", "mine": False, "others": [], "since": 0, "guard": "on"}
    win = claims[0]
    return {"owner": win[1], "mine": win[1] == me, "since": win[0], "guard": "on",
            "others": [c[1] for c in claims if c[1] != win[1]]}


def delete_claim(svc, name):
    app_api("DELETE", f"/repos/{svc}/git/refs/{OWNER_PREFIX}{q(name)}", allow=(404, 422))


def op_claim_repo(params):
    """Make sure this install holds a claim unless an older one exists. Safe to repeat."""
    svc, me = p_service(params), p_instance(params)
    claims = owner_claims(svc)
    mine = [c for c in claims if c[1] == me]
    foreign_first = bool(claims) and claims[0][1] != me
    if not mine and not foreign_first:
        repo = app_api("GET", f"/repos/{svc}").data or {}
        branch = repo.get("default_branch") or "main"
        head = app_api("GET", f"/repos/{svc}/git/ref/heads/{q(branch)}", allow=(404, 409))
        sha = ((head.data or {}).get("object") or {}).get("sha") if head.status == 200 else None
        if not sha:  # an empty repository has nothing to point a ref at: no guard is possible yet
            return {"owner": me, "mine": True, "others": [], "since": 0, "guard": "unavailable"}
        # created_at is the name's first part: it decides who is oldest
        stamp = int(time.time())
        app_api("POST", f"/repos/{svc}/git/refs", body={"ref": f"refs/{OWNER_PREFIX}{stamp}-{me}", "sha": sha}, allow=(422,))
        claims = owner_claims(svc)
        mine = [c for c in claims if c[1] == me]
    # two installs that claimed at the same moment: the oldest stays, the other gives its claim back
    if mine and claims and claims[0][1] != me:
        for c in mine:
            delete_claim(svc, c[2])
        claims = [c for c in claims if c[1] != me]
    return owner_report(claims, me)


def op_release_repo(params):
    svc, me = p_service(params), p_instance(params)
    for c in owner_claims(svc):
        if c[1] == me:
            delete_claim(svc, c[2])
    return {}


def op_takeover_repo(params):
    """A person decided this install takes the repository over: remove the other claims, then claim."""
    svc, me = p_service(params), p_instance(params)
    for c in owner_claims(svc):
        if c[1] != me:
            delete_claim(svc, c[2])
    return op_claim_repo(params)



def visible_repos():
    repos = app_paginate("/installation/repositories?per_page=100", key="repositories")
    org = org_filter()
    if org:
        repos = [r for r in repos
                 if str((r.get("owner") or {}).get("login", "")).lower() == org
                 or str(r.get("full_name", "")).split("/")[0].lower() == org]
    return repos


def op_list_services(params):
    return {"services": [
        {"external_id": r["full_name"], "default_branch": r.get("default_branch") or "main",
         "archived": bool(r.get("archived")), "private": bool(r.get("private"))}
        for r in visible_repos()]}


def now_iso():
    return datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


def op_list_changes(params):
    raw = params.get("cursor")
    if raw:
        try:
            cur = json.loads(raw)
            assert isinstance(cur, dict) and isinstance(cur.get("repos", {}), dict)
            assert isinstance(cur["base"], str)
        except (ValueError, AssertionError, KeyError, TypeError):
            raise permanent("invalid cursor")
    else:
        # First call: nothing to diff against. Baseline "now"; no history import.
        return {"items": [], "cursor": json.dumps({"base": now_iso(), "repos": {}}, sort_keys=True)}
    new_repos, items, changed = {}, [], False
    for repo in visible_repos():
        name = repo["full_name"]
        if repo.get("archived"):
            continue
        st = cur.get("repos", {}).get(name, {})
        since = st.get("since") or cur["base"]
        hdrs = {"If-None-Match": st["etag"]} if st.get("etag") else {}
        path = (f"/repos/{name}/issues?state=all&sort=updated&direction=asc"
                f"&per_page=100&since={q(since)}")
        try:
            r = app_api("GET", path, headers=hdrs, allow=(304,))
            if r.status == 304:
                new_repos[name] = {"since": since, "etag": st.get("etag")}
                continue
            rows = list(r.data or [])
            url = next_link(r.headers)
            pages = 1
            while url and pages < 100:
                nxt = app_api("GET", url)
                rows.extend(nxt.data or [])
                url, pages = next_link(nxt.headers), pages + 1
        except PluginError as e:
            if e.status in (404, 410):  # issues disabled or repo gone
                continue
            if e.kind == "unauthorized" and e.status in (403, 451):
                # blocked or restricted repository: skip it, do not stop the sweep for all the others
                print(f"list_changes: skipping {name}: {e.message}", file=sys.stderr)
                continue
            raise
        etag = r.headers.get("etag")
        newest = since
        for it in rows:
            ts = it.get("updated_at", "")
            items.append({"service": name, "ref": str(it["number"]),
                          "kind": "pr" if "pull_request" in it else "issue",
                          "updated_at": ts})
            if ts > newest:
                newest = ts
        # An ETag is only valid for the exact query (same `since`).
        new_repos[name] = {"since": newest, "etag": etag if newest == since else None}
        changed = changed or bool(rows)
    for v in new_repos.values():
        if v.get("etag") is None:
            v.pop("etag", None)
    if not changed and new_repos == cur.get("repos", {}):
        return {"items": [], "cursor": raw}
    items.sort(key=lambda i: i["updated_at"])
    return {"items": items,
            "cursor": json.dumps({"base": cur["base"], "repos": new_repos}, sort_keys=True)}


_app_login = {"value": None}


def own_bot_login():
    """Login of this App's bot user (issues created through tasklane are authored by it)."""
    if _app_login["value"] is None:
        slug = (api("GET", "/app", auth=make_jwt()).data or {}).get("slug", "")
        _app_login["value"] = f"{slug}[bot]" if slug else ""
    return _app_login["value"]


def is_own_comment(c):
    """Written by this App's bot user. (GitHub strips hidden HTML markers from stored comments,
    so the author is the reliable signal.)"""
    try:
        mine = own_bot_login()
    except PluginError:
        return False
    return bool(mine) and (c.get("user") or {}).get("login") == mine


TRUSTED_ASSOCIATIONS = ("OWNER", "MEMBER", "COLLABORATOR")


def author_trusted(issue):
    """True for people with a real relationship to the repository and for tasklane's own
    issues (created from the report button or chat, where the gateway already checked the reporter)."""
    user = issue.get("user") or {}
    if issue.get("author_association") in TRUSTED_ASSOCIATIONS:
        return True
    try:
        mine = own_bot_login()
    except PluginError:
        return False
    return bool(mine) and user.get("login") == mine


def op_get_item(params):
    svc, ref = p_service(params), p_num(params, "ref")
    issue = app_api("GET", f"/repos/{svc}/issues/{ref}").data or {}
    comments = app_paginate(f"/repos/{svc}/issues/{ref}/comments?per_page=100")
    return {"title": issue.get("title", ""), "body": issue.get("body") or "",
            "author": (issue.get("user") or {}).get("login", ""),
            "labels": [l["name"] if isinstance(l, dict) else str(l) for l in issue.get("labels", [])],
            "state": "closed" if issue.get("state") == "closed" else "open",
            "is_pull_request": "pull_request" in issue,
            "author_trusted": author_trusted(issue),
            "comments": [{"author": (c.get("user") or {}).get("login", ""),
                          "body": c.get("body") or "", "at": c.get("created_at", ""),
                          "mine": is_own_comment(c), "trusted": author_trusted(c)}
                         for c in comments]}


def op_create_item(params):
    svc = p_service(params)
    body = {"title": p_str(params, "title"), "body": params.get("body") or "",
            "labels": p_list(params, "labels")}
    d = app_api("POST", f"/repos/{svc}/issues", body=body).data or {}
    return {"ref": str(d["number"])}


def op_comment(params):
    svc, ref = p_service(params), p_num(params, "ref")
    d = app_api("POST", f"/repos/{svc}/issues/{ref}/comments",
                body={"body": p_str(params, "body")}).data or {}
    return {"id": str(d["id"])}


def op_set_labels(params):
    svc, ref = p_service(params), p_num(params, "ref")
    add, remove = p_list(params, "add"), p_list(params, "remove")
    if add:
        app_api("POST", f"/repos/{svc}/issues/{ref}/labels", body={"labels": add})
    for name in remove:
        try:
            app_api("DELETE", f"/repos/{svc}/issues/{ref}/labels/{q(name)}")
        except PluginError as e:
            if e.status != 404:  # label not on the issue: already as wanted
                raise
    return {}


def get_file(svc, path):
    """Decoded text of a file, or None if absent."""
    try:
        d = app_api("GET", f"/repos/{svc}/contents/{path}").data
    except PluginError as e:
        if e.kind == "permanent":
            return None
        raise
    if not isinstance(d, dict) or d.get("encoding") != "base64":
        return None
    try:
        return base64.b64decode(d.get("content", "")).decode("utf-8", "replace")
    except (binascii.Error, ValueError):
        return None


OVERRIDE_FILE = ".tasklane.yml"
MODE_ORDER = ("watch", "triage", "full")


def parse_overrides(text):
    """Read the small subset of YAML `.tasklane.yml` uses: `key: value` lines and `- item` lists.
    The file can only make tasklane STRICTER in that repository; anything else is ignored:
      protected_paths: [list]   extra paths that always need approval of the exact diff
      max_diff_lines: 200       a smaller diff limit than the install's
      mode_cap: triage          the most this repository may be set to (watch, triage or full)
      always_ask: true          every ticket needs a person's approval"""
    out, ignored, key = {}, [], None
    for raw in (text or "").splitlines()[:500]:
        line = raw.split(" #")[0].rstrip() if not raw.lstrip().startswith("#") else ""
        if not line.strip():
            continue
        item = line.strip()
        if item.startswith("- ") and key == "protected_paths":
            v = item[2:].strip().strip("\"'")
            if v and len(v) <= 200 and "\x00" not in v and ".." not in v.split("/"):
                out.setdefault("protected_paths", [])
                if len(out["protected_paths"]) < 50:
                    out["protected_paths"].append(v.lstrip("/"))
            continue
        k, sep, v = line.partition(":")
        k, v = k.strip(), v.strip().strip("\"'")
        if not sep or line[0] in " \t":
            continue
        key = k
        if k == "protected_paths":
            continue
        if k == "max_diff_lines" and v.isdigit() and 10 <= int(v) <= 100000:
            out[k] = int(v)
        elif k == "mode_cap" and v in MODE_ORDER:
            out[k] = v
        elif k == "always_ask" and v.lower() in ("true", "false"):
            out[k] = v.lower() == "true"
        else:
            ignored.append(k)
    return out, ignored


def op_read_overrides(params):
    svc = p_service(params)
    text = get_file(svc, OVERRIDE_FILE)
    if text is None:
        return {"overrides": {}, "found": False}
    out, ignored = parse_overrides(text)
    return {"overrides": out, "found": True, "ignored": ignored}


def op_detect_facts(params):
    svc = p_service(params)
    langs = app_api("GET", f"/repos/{svc}/languages").data or {}
    language = max(langs, key=langs.get) if langs else None
    try:
        root = app_api("GET", f"/repos/{svc}/contents/").data
    except PluginError as e:
        if e.kind != "permanent":
            raise
        root = []  # e.g. empty repository
    names = {f.get("name") for f in root if isinstance(f, dict)} if isinstance(root, list) else set()
    test_command = None
    if names & {"pyproject.toml", "pytest.ini", "tox.ini", "setup.py"}:
        test_command = "pytest"
    elif "package.json" in names:
        test_command = "npm test"
    elif "Cargo.toml" in names:
        test_command = "cargo test"
    elif "go.mod" in names:
        test_command = "go test ./..."
    elif "Makefile" in names:
        mk = get_file(svc, "Makefile") or ""
        if re.search(r"^test\s*:", mk, re.M):
            test_command = "make test"
    try:
        envs = (app_api("GET", f"/repos/{svc}/environments").data or {}).get("environments", [])
    except PluginError as e:
        if e.kind not in ("permanent", "unauthorized"):
            raise
        envs = []  # not visible with this App's permissions
    protected = list(DEFAULT_PROTECTED)
    for path in ("CODEOWNERS", ".github/CODEOWNERS", "docs/CODEOWNERS"):
        text = get_file(svc, path)
        if text is None:
            continue
        for line in text.splitlines():
            line = line.split("#")[0].strip()
            if not line:
                continue
            pat = line.split()[0].lstrip("/")
            if pat and pat not in protected:
                protected.append(pat)
        break
    out = {"environments": [e["name"] for e in envs if isinstance(e, dict) and "name" in e],
           "protected_paths": protected}
    if language:
        out["language"] = language
    if test_command:
        out["test_command"] = test_command
    return out


def op_repo_url(params):
    svc = p_service(params)
    token = params.get("token")
    if token:
        if not isinstance(token, str):
            raise permanent("token must be a string")
        remember_secret(token)
        return {"url": f"https://x-access-token:{token}@github.com/{svc}.git"}
    return {"url": f"https://github.com/{svc}.git"}


def op_mint_run_token(params):
    svc = p_service(params)
    perms = params.get("permissions")
    if not isinstance(perms, list) or not perms:
        raise permanent("permissions must be a non-empty list like contents:write")
    granted = {}
    for item in perms:
        m = re.match(r"^([a-z_]+):(read|write)$", item) if isinstance(item, str) else None
        if not m:
            raise permanent("bad permission, expected name:read|write")
        if granted.get(m.group(1)) != "write":
            granted[m.group(1)] = m.group(2)
    # GitHub fixes installation token lifetime at one hour; ttl_secs cannot extend it.
    tok, exp = mint({"repositories": [svc.split("/")[1]], "permissions": granted})
    return {"token": tok, "expires_at": exp}


def need_token(params):
    t = params.get("token")
    if not t or not isinstance(t, str):
        raise permanent("missing param: token")
    remember_secret(t)
    return t


def op_open_pr(params):
    svc, tok = p_service(params), need_token(params)
    body = {"title": p_str(params, "title"), "head": p_str(params, "branch"),
            "base": p_str(params, "base"), "body": params.get("body") or ""}
    try:
        d = api("POST", f"/repos/{svc}/pulls", auth=tok, body=body).data or {}
    except PluginError as e:
        if e.status != 422 or "already exists" not in e.message.lower():
            raise
        owner = svc.split("/")[0]
        found = api("GET", f"/repos/{svc}/pulls?state=open&head={q(owner)}:{q(body['head'])}", auth=tok).data or []
        if not found:
            raise
        d = found[0]
    return {"pr": str(d["number"])}


def op_get_checks(params):
    svc, pr = p_service(params), p_num(params, "pr")
    sha = ((app_api("GET", f"/repos/{svc}/pulls/{pr}").data or {}).get("head") or {}).get("sha")
    if not sha:
        raise PluginError("transient", "pull request has no head sha")
    runs = app_paginate(f"/repos/{svc}/commits/{sha}/check-runs?per_page=100", key="check_runs")
    checks = []
    for c in runs:
        if c.get("status") != "completed":
            st = "pending"
        elif c.get("conclusion") in OK_CHECKS:
            st = "success"
        else:
            st = "failure"
        checks.append({"name": c.get("name", ""), "state": st})
    comb = app_api("GET", f"/repos/{svc}/commits/{sha}/status").data or {}
    for s in comb.get("statuses", []):
        st = {"success": "success", "pending": "pending"}.get(s.get("state"), "failure")
        checks.append({"name": s.get("context", ""), "state": st})
    states = {c["state"] for c in checks}
    state = "failure" if "failure" in states else "pending" if "pending" in states else "success"
    return {"head_sha": sha, "state": state, "checks": checks}


def op_merge_pr(params):
    svc, pr, tok = p_service(params), p_num(params, "pr"), need_token(params)
    expected = p_str(params, "expected_head_sha")
    d = api("GET", f"/repos/{svc}/pulls/{pr}", auth=tok).data or {}
    head = (d.get("head") or {}).get("sha")
    if head != expected:
        return {"merged": False, "reason": "head_moved"}
    if d.get("merged"):
        out = {"merged": True}
        if d.get("merge_commit_sha"):
            out["sha"] = d["merge_commit_sha"]
        return out
    if d.get("state") == "closed":
        return {"merged": False, "reason": "not_mergeable"}
    try:
        r = api("PUT", f"/repos/{svc}/pulls/{pr}/merge", auth=tok,
                body={"sha": expected}).data or {}
    except PluginError as e:
        if e.status == 409:
            return {"merged": False, "reason": "head_moved"}
        if e.status == 405:
            low = e.message.lower()
            if "merge queue" in low:
                reason = "merge_queue"
            elif "conflict" in low:
                reason = "conflict"
            elif "check" in low or "status" in low:
                reason = "checks_failing"
            else:
                reason = "not_mergeable"
            return {"merged": False, "reason": reason}
        raise
    out = {"merged": bool(r.get("merged", True))}
    if r.get("sha"):
        out["sha"] = r["sha"]
    return out


def op_verify_webhook(params):
    secret = os.environ.get("GITHUB_WEBHOOK_SECRET", "")
    if not secret:
        raise permanent("GITHUB_WEBHOOK_SECRET is not set")
    remember_secret(secret)
    headers = params.get("headers")
    if not isinstance(headers, dict):
        raise permanent("headers must be an object")
    h = {str(k).lower(): str(v) for k, v in headers.items()}
    try:
        body = base64.b64decode(p_str(params, "body_b64"), validate=True)
    except (binascii.Error, ValueError):
        raise permanent("body_b64 is not valid base64")
    want = "sha256=" + hmac.new(secret.encode(), body, hashlib.sha256).hexdigest()
    if not hmac.compare_digest(want.encode(), h.get("x-hub-signature-256", "").encode()):
        raise permanent("webhook signature mismatch")
    event = h.get("x-github-event", "")
    try:
        payload = json.loads(body)
        assert isinstance(payload, dict)
    except (ValueError, AssertionError):
        raise permanent("webhook body is not a JSON object")
    service = (payload.get("repository") or {}).get("full_name")
    if not service:
        raise permanent(f"webhook event '{event}' has no repository")
    action = payload.get("action")
    if event == "issues":
        if "pull_request" in (payload.get("issue") or {}):
            raise permanent("pull request is not a ticket event")
        ref = payload.get("issue", {}).get("number")
        kind = "label" if action in ("labeled", "unlabeled") else "issue"
    elif event == "issue_comment":
        if "pull_request" in (payload.get("issue") or {}):
            raise permanent("comment on a pull request is not a ticket event")
        ref, kind = payload.get("issue", {}).get("number"), "comment"
    elif event == "pull_request":
        ref = (payload.get("pull_request") or {}).get("number") or payload.get("number")
        kind = "label" if action in ("labeled", "unlabeled") else "pr"
    elif event == "check_suite":
        prs = (payload.get("check_suite") or {}).get("pull_requests") or []
        ref = prs[0].get("number") if prs and isinstance(prs[0], dict) else None
        kind = "pr"
        if ref is None:
            raise permanent("check_suite event is not tied to a pull request")
    else:
        raise permanent(f"unsupported webhook event: {event or 'none'}")
    if ref is None:
        raise permanent(f"webhook event '{event}' has no item number")
    return {"event": {"service": service, "ref": str(ref), "kind": kind}}


OPS = {
    "describe": op_describe, "list_services": op_list_services,
    "list_changes": op_list_changes, "get_item": op_get_item,
    "create_item": op_create_item, "comment": op_comment,
    "set_labels": op_set_labels, "detect_facts": op_detect_facts, "read_overrides": op_read_overrides,
    "repo_url": op_repo_url, "mint_run_token": op_mint_run_token,
    "open_pr": op_open_pr, "get_checks": op_get_checks,
    "merge_pr": op_merge_pr, "verify_webhook": op_verify_webhook,
    "app_manifest_convert": op_app_manifest_convert, "list_installations": op_list_installations,
    "claim_repo": op_claim_repo, "release_repo": op_release_repo, "takeover_repo": op_takeover_repo,
}


def handle(req):
    rid = req.get("id") if isinstance(req, dict) else None
    try:
        if not isinstance(req, dict):
            raise permanent("request must be a JSON object")
        op = req.get("op")
        fn = OPS.get(op) if isinstance(op, str) else None
        if fn is None:
            raise permanent(f"unknown op: {op}")
        params = req.get("params") or {}
        if not isinstance(params, dict):
            raise permanent("params must be an object")
        log(f"op {op} id {rid}")
        return {"id": rid, "ok": True, "result": fn(params)}
    except PluginError as e:
        log(f"op failed id {rid}: {e.kind}: {e.message}")
        return {"id": rid, "ok": False, "error": {"kind": e.kind, "message": redact(e.message)}}
    except Exception as e:  # never crash the loop
        log(f"internal error id {rid}: {type(e).__name__}")
        return {"id": rid, "ok": False, "error": {
            "kind": "permanent", "message": redact(f"internal error: {type(e).__name__}")}}


def main():
    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        try:
            req = json.loads(line)
        except ValueError:
            resp = {"id": None, "ok": False, "error": {"kind": "permanent", "message": "bad json"}}
        else:
            resp = handle(req)
        sys.stdout.write(json.dumps(resp) + "\n")
        sys.stdout.flush()


if __name__ == "__main__":
    main()
