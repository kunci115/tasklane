"""A list of model credentials for engine-claude, so that when one account runs out of usage (or is
refused) the next one takes over. Standard library only.

The list lives in a JSON file that only the operator can read (`TASKLANE_CLAUDE_POOL_FILE`, mode 0600).
Order is priority: the credential in the plug-in's own environment is tried first, then the accounts in
the file, top to bottom. A credential that hit its usage limit sleeps for a while (`cooldown`), one the
provider refused sleeps for a day; both come back by themselves. Tokens are never returned by `status`
and never written to a command line: the engine passes them to the CLI (or, by name, to Docker) as an
environment variable for one run.

  python3 credential_pool.py list | add LABEL KIND | enable LABEL | disable LABEL | remove LABEL | reset LABEL
  (add reads the token from standard input, so it never shows up in a process list or shell history)
"""
import fcntl
import json
import math
import os
import re
import sys
import tempfile
import time

KIND_VAR = {"oauth": "CLAUDE_CODE_OAUTH_TOKEN", "api_key": "ANTHROPIC_API_KEY"}
LABEL = re.compile(r"^[A-Za-z0-9][A-Za-z0-9 ._-]{0,39}$")
ENV_LABEL = "environment"
DAY = 86400


class PoolError(Exception):
    pass


def path_from(env=None):
    p = (os.environ if env is None else env).get("TASKLANE_CLAUDE_POOL_FILE", "").strip()
    return p or None


def _read(path):
    try:
        with open(path, encoding="utf-8") as f:
            doc = json.load(f)
    except FileNotFoundError:
        return {"accounts": [], "environment": {}}
    except ValueError as e:
        raise PoolError("the credential file is not valid JSON: %s" % e)
    if not isinstance(doc, dict) or not isinstance(doc.get("accounts", []), list):
        raise PoolError("the credential file has the wrong shape")
    doc.setdefault("accounts", [])
    doc.setdefault("environment", {})
    return doc


class locked:
    """Read-modify-write under a lock, because the daemon's plug-in and an operator's command can meet."""

    def __init__(self, path):
        self.path = path

    def __enter__(self):
        d = os.path.dirname(os.path.abspath(self.path))
        os.makedirs(d, mode=0o700, exist_ok=True)
        self.lock = open(self.path + ".lock", "a")
        os.chmod(self.path + ".lock", 0o600)
        fcntl.flock(self.lock, fcntl.LOCK_EX)
        self.doc = _read(self.path)
        return self.doc

    def __exit__(self, exc_type, exc, tb):
        try:
            if exc_type is None:
                fd, tmp = tempfile.mkstemp(dir=os.path.dirname(os.path.abspath(self.path)), prefix=".pool-")
                with os.fdopen(fd, "w", encoding="utf-8") as f:
                    json.dump(self.doc, f, indent=2)
                os.chmod(tmp, 0o600)
                os.replace(tmp, self.path)
        finally:
            fcntl.flock(self.lock, fcntl.LOCK_UN)
            self.lock.close()


def all_tokens(path):
    """Every token in the file, for scrubbing output."""
    if not path:
        return []
    try:
        return [a["token"] for a in _read(path)["accounts"] if isinstance(a.get("token"), str) and a["token"]]
    except (PoolError, OSError):
        return []


def _find(doc, label):
    for a in doc["accounts"]:
        if a.get("label") == label:
            return a
    raise PoolError("no account called %r" % label)


def add(path, label, kind, token, now=None):
    if not LABEL.match(label or ""):
        raise PoolError("the label is 1-40 letters, digits, space, dot, dash or underscore")
    if label == ENV_LABEL:
        raise PoolError("%r is reserved for the credential in the environment" % ENV_LABEL)
    if kind not in KIND_VAR:
        raise PoolError("the kind is oauth (a token from `claude setup-token`) or api_key")
    token = (token or "").strip()
    if len(token) < 20 or re.search(r"\s", token):
        raise PoolError("that does not look like a token")
    with locked(path) as doc:
        if any(a.get("label") == label for a in doc["accounts"]):
            raise PoolError("an account called %r already exists" % label)
        doc["accounts"].append({"label": label, "kind": kind, "token": token, "enabled": True,
                                "cooldown_until": 0, "last_error": "", "last_used": 0,
                                "added": int(now or time.time())})


def remove(path, label):
    with locked(path) as doc:
        _find(doc, label)
        doc["accounts"] = [a for a in doc["accounts"] if a.get("label") != label]


def set_enabled(path, label, enabled):
    with locked(path) as doc:
        a = _find(doc, label)
        a["enabled"] = bool(enabled)
        if enabled:
            a["cooldown_until"] = 0
            a["last_error"] = ""


def reset(path, label):
    """Forget a cooldown now (the person knows the limit is over)."""
    with locked(path) as doc:
        if label == ENV_LABEL:
            doc["environment"] = {}
            return
        a = _find(doc, label)
        a["cooldown_until"] = 0
        a["last_error"] = ""


def move(path, label, delta):
    """Change priority: -1 earlier, +1 later."""
    with locked(path) as doc:
        a = _find(doc, label)
        i = doc["accounts"].index(a)
        j = i + (-1 if delta < 0 else 1)
        if 0 <= j < len(doc["accounts"]):
            doc["accounts"][i], doc["accounts"][j] = doc["accounts"][j], doc["accounts"][i]


def candidates(path, env=None, now=None):
    """Credentials to try, best first: [{label, kind, var, token}]. Sleeping and disabled ones are left out."""
    env = os.environ if env is None else env
    now = time.time() if now is None else now
    doc = _read(path)
    out = []
    envstate = doc.get("environment") or {}
    if now >= envstate.get("cooldown_until", 0):
        for kind in ("oauth", "api_key"):
            v = KIND_VAR[kind]
            if env.get(v):
                out.append({"label": ENV_LABEL, "kind": kind, "var": v, "token": env[v]})
                break
    for a in doc["accounts"]:
        if a.get("enabled", True) and now >= a.get("cooldown_until", 0) and a.get("kind") in KIND_VAR and a.get("token"):
            out.append({"label": a["label"], "kind": a["kind"], "var": KIND_VAR[a["kind"]], "token": a["token"]})
    return out


def used(path, label, now=None):
    with locked(path) as doc:
        t = int(now or time.time())
        if label == ENV_LABEL:
            doc["environment"]["last_used"] = t
        else:
            try:
                _find(doc, label)["last_used"] = t
            except PoolError:
                pass


def sleep_for(path, label, reason, seconds, now=None):
    """Put a credential to sleep after a failure."""
    until = int((now or time.time()) + seconds)
    with locked(path) as doc:
        target = doc["environment"] if label == ENV_LABEL else None
        if target is None:
            try:
                target = _find(doc, label)
            except PoolError:
                return
        target["cooldown_until"] = until
        target["last_error"] = reason[:200]


def earliest_wake(path, now=None):
    """Seconds until the first sleeping credential is usable again (None if nothing sleeps)."""
    now = time.time() if now is None else now
    doc = _read(path)
    ups = [a["cooldown_until"] for a in doc["accounts"] if a.get("enabled", True) and a.get("cooldown_until", 0) > now]
    ups += [doc["environment"]["cooldown_until"]] if doc["environment"].get("cooldown_until", 0) > now else []
    return max(0, math.ceil(min(ups) - now)) if ups else None


def mask(token):
    return "…" + token[-4:] if isinstance(token, str) and len(token) >= 12 else "…"


def status(path, env=None, now=None):
    """What a person may see: no tokens, only the last four characters."""
    env = os.environ if env is None else env
    now = time.time() if now is None else now
    doc = _read(path)
    rows = []
    have_env = any(env.get(v) for v in KIND_VAR.values())
    if have_env:
        e = doc["environment"]
        rows.append({"label": ENV_LABEL, "kind": "environment", "token": "", "enabled": True,
                     "cooldown_secs": max(0, int(e.get("cooldown_until", 0) - now)), "last_error": e.get("last_error", ""),
                     "last_used": e.get("last_used", 0), "removable": False})
    for a in doc["accounts"]:
        rows.append({"label": a.get("label", ""), "kind": a.get("kind", ""), "token": mask(a.get("token")),
                     "enabled": a.get("enabled", True), "cooldown_secs": max(0, int(a.get("cooldown_until", 0) - now)),
                     "last_error": a.get("last_error", ""), "last_used": a.get("last_used", 0), "removable": True})
    return rows


def main(argv):
    path = path_from()
    if not path:
        print("set TASKLANE_CLAUDE_POOL_FILE to the credential file first", file=sys.stderr)
        return 2
    cmd, args = (argv[1] if len(argv) > 1 else "list"), argv[2:]
    try:
        if cmd == "list":
            for r in status(path):
                state = "disabled" if not r["enabled"] else ("sleeping %ds" % r["cooldown_secs"] if r["cooldown_secs"] else "ready")
                print("%-20s %-12s %-8s %s %s" % (r["label"], r["kind"], r["token"], state, r["last_error"]))
        elif cmd == "add" and len(args) == 2:
            add(path, args[0], args[1], sys.stdin.readline())
        elif cmd == "remove" and len(args) == 1:
            remove(path, args[0])
        elif cmd == "enable" and len(args) == 1:
            set_enabled(path, args[0], True)
        elif cmd == "disable" and len(args) == 1:
            set_enabled(path, args[0], False)
        elif cmd == "reset" and len(args) == 1:
            reset(path, args[0])
        else:
            print(__doc__, file=sys.stderr)
            return 2
    except PoolError as e:
        print("error: %s" % e, file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
