import base64
import hashlib
import hmac
import json
import os
import shutil
import subprocess
import sys
import tempfile
import threading
import time
import unittest
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from urllib.parse import urlsplit, parse_qs

SCRIPT = Path(__file__).with_name("source_github.py")
HAVE_OPENSSL = shutil.which("openssl") is not None
SKIP_MSG = "openssl is not installed; source-github tests need it to make a fake App key"

WEBHOOK_SECRET = "whsec-TESTSECRET-0123456789"
INST_TOKEN = "ghs_FAKEINSTALLATIONTOKEN0001"
RUN_TOKEN = "ghs_FAKERUNTOKEN0002"
PASSED_TOKEN = "ghs_PASSEDINTOKEN0003"
TMP = None
KEY_FILE = None
PUB_FILE = None
KEY_LINES = []


def setUpModule():
    global TMP, KEY_FILE, PUB_FILE, KEY_LINES
    if not HAVE_OPENSSL:
        return
    TMP = tempfile.mkdtemp(prefix="srcgh-")
    KEY_FILE = os.path.join(TMP, "app.pem")
    PUB_FILE = os.path.join(TMP, "app.pub")
    subprocess.run(["openssl", "genrsa", "-out", KEY_FILE, "2048"], check=True, capture_output=True)
    subprocess.run(["openssl", "rsa", "-in", KEY_FILE, "-pubout", "-out", PUB_FILE],
                   check=True, capture_output=True)
    lines = Path(KEY_FILE).read_text().splitlines()
    KEY_LINES = [l for l in lines if l and not l.startswith("-----")][:5]


def tearDownModule():
    if TMP:
        shutil.rmtree(TMP, ignore_errors=True)


class Fake(ThreadingHTTPServer):
    daemon_threads = True

    def __init__(self):
        super().__init__(("127.0.0.1", 0), Handler)
        self.routes = {}
        self.log = []  # dicts: method, path, query, headers, body
        self.thread = threading.Thread(target=self.serve_forever, daemon=True)
        self.thread.start()

    @property
    def url(self):
        return f"http://127.0.0.1:{self.server_address[1]}"

    def seen(self, method, path):
        return [r for r in self.log if r["method"] == method and r["path"] == path]


class Handler(BaseHTTPRequestHandler):
    def log_message(self, *a):
        pass

    def _do(self):
        u = urlsplit(self.path)
        n = int(self.headers.get("Content-Length") or 0)
        raw = self.rfile.read(n) if n else b""
        req = {"method": self.command, "path": u.path, "query": parse_qs(u.query),
               "headers": {k.lower(): v for k, v in self.headers.items()},
               "body": json.loads(raw) if raw else None, "server": self.server}
        self.server.log.append(req)
        route = self.server.routes.get((self.command, u.path))
        if route is None:
            status, headers, body = 404, {}, {"message": "Not Found"}
        else:
            status, headers, body = route(req) if callable(route) else route
        data = b"" if body is None else json.dumps(body).encode()
        self.send_response(status)
        for k, v in headers.items():
            self.send_header(k, v)
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    do_GET = do_POST = do_PUT = do_DELETE = _do


ALL_OUTPUT = []


def leak_check(out, err):
    blob = out + err
    for s in [WEBHOOK_SECRET, INST_TOKEN] + KEY_LINES:
        assert s not in blob, "secret leaked into plug-in output"


def talk(env_extra, *requests):
    env = {"PATH": os.environ["PATH"], "GITHUB_APP_ID": "12345",
           "GITHUB_APP_PRIVATE_KEY_FILE": KEY_FILE, "GITHUB_INSTALLATION_ID": "77",
           "GITHUB_WEBHOOK_SECRET": WEBHOOK_SECRET}
    env.update(env_extra)
    lines = [r if isinstance(r, str) else json.dumps(r) for r in requests]
    p = subprocess.run([sys.executable, "-I", str(SCRIPT)], input="\n".join(lines) + "\n",
                       capture_output=True, text=True, timeout=60, env=env)
    leak_check(p.stdout, p.stderr)
    ALL_OUTPUT.append((p.stdout, p.stderr))
    return [json.loads(l) for l in p.stdout.splitlines()]


def jwt_ok(auth_header):
    """Verify the Bearer JWT is a valid RS256 token signed by the fake key."""
    assert auth_header.startswith("Bearer ")
    head, pay, sig = auth_header[7:].split(".")
    pad = lambda s: s + "=" * (-len(s) % 4)
    assert json.loads(base64.urlsafe_b64decode(pad(head))) == {"alg": "RS256", "typ": "JWT"}
    claims = json.loads(base64.urlsafe_b64decode(pad(pay)))
    assert claims["iss"] == "12345" and claims["exp"] > time.time()
    sigf = os.path.join(TMP, "sig.bin")
    Path(sigf).write_bytes(base64.urlsafe_b64decode(pad(sig)))
    r = subprocess.run(["openssl", "dgst", "-sha256", "-verify", PUB_FILE, "-signature", sigf],
                       input=f"{head}.{pay}".encode(), capture_output=True)
    return r.returncode == 0


def future(secs=3600):
    return time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime(time.time() + secs))


@unittest.skipUnless(HAVE_OPENSSL, SKIP_MSG)
class Base(unittest.TestCase):
    def setUp(self):
        self.fake = Fake()
        self.addCleanup(self.fake.server_close)
        self.addCleanup(self.fake.shutdown)
        self.fake.routes[("POST", "/app/installations/77/access_tokens")] = (
            201, {}, {"token": INST_TOKEN, "expires_at": future()})

    def run1(self, op, params=None, env=None):
        e = {"GITHUB_API_URL": self.fake.url}
        e.update(env or {})
        (r,) = talk(e, {"id": 1, "op": op, "params": params or {}})
        return r

    def ok(self, op, params=None, env=None):
        r = self.run1(op, params, env)
        self.assertTrue(r["ok"], r)
        return r["result"]

    def err(self, op, params=None, env=None):
        r = self.run1(op, params, env)
        self.assertFalse(r["ok"], r)
        return r["error"]

    def auth_of(self, method, path):
        (req,) = self.fake.seen(method, path)
        return req["headers"].get("authorization")


class Protocol(Base):
    def test_describe(self):
        r = self.ok("describe")
        self.assertEqual((r["name"], r["role"], r["protocol_major"]), ("github", "ticket_source", 1))
        self.assertIs(r["configured"], True, "the test environment has an App id and a key file")

    def test_describe_says_when_the_plugin_is_not_configured(self):
        (r,) = talk({"GITHUB_APP_ID": "", "GITHUB_APP_PRIVATE_KEY_FILE": ""}, {"id": 1, "op": "describe"})
        self.assertTrue(r["ok"])
        self.assertIs(r["result"]["configured"], False)

    def test_unknown_op_malformed_lines_and_ids(self):
        out = talk({"GITHUB_API_URL": self.fake.url},
                   "not json", "[1,2]", {"id": 9, "op": "nope"}, {"id": 10, "op": "get_item", "params": []},
                   {"id": 11, "op": "describe"})
        self.assertEqual([o["id"] for o in out], [None, None, 9, 10, 11])
        self.assertEqual([o["ok"] for o in out], [False, False, False, False, True])
        self.assertTrue(all(o["error"]["kind"] == "permanent" for o in out[:4]))

    def test_missing_params_are_permanent(self):
        self.assertEqual(self.err("get_item", {"service": "o/r"})["kind"], "permanent")
        self.assertEqual(self.err("get_item", {"service": "../x", "ref": "1"})["kind"], "permanent")


class Reading(Base):
    def repos_route(self, req):
        page = req["query"].get("page", ["1"])[0]
        if page == "1":
            return 200, {"Link": f'<{self.fake.url}/installation/repositories?per_page=100&page=2>; rel="next"'}, {
                "repositories": [{"full_name": "acme/one", "default_branch": "main", "archived": False,
                                  "private": True, "owner": {"login": "acme"}}]}
        return 200, {}, {"repositories": [{"full_name": "acme/two", "default_branch": "dev",
                                           "archived": True, "private": False, "owner": {"login": "acme"}}]}

    def test_list_services_two_pages_and_jwt_and_cache(self):
        self.fake.routes[("GET", "/installation/repositories")] = self.repos_route
        out = talk({"GITHUB_API_URL": self.fake.url}, {"id": 1, "op": "list_services", "params": {}},
                   {"id": 2, "op": "list_services", "params": {}})
        self.assertEqual(out[0]["result"], {"services": [
            {"external_id": "acme/one", "default_branch": "main", "archived": False, "private": True},
            {"external_id": "acme/two", "default_branch": "dev", "archived": True, "private": False}]})
        self.assertEqual(out[1]["result"], out[0]["result"])
        mints = self.fake.seen("POST", "/app/installations/77/access_tokens")
        self.assertEqual(len(mints), 1, "installation token must be cached")
        self.assertTrue(jwt_ok(mints[0]["headers"]["authorization"]))
        for r in self.fake.seen("GET", "/installation/repositories"):
            self.assertEqual(r["headers"]["authorization"], "Bearer " + INST_TOKEN)

    def test_org_filter(self):
        self.fake.routes[("GET", "/installation/repositories")] = self.repos_route
        res = self.ok("list_services", env={"GITHUB_ORG": "other"})
        self.assertEqual(res["services"], [])

    def test_installation_discovery(self):
        self.fake.routes[("GET", "/app/installations")] = (200, {}, [{"id": 77, "account": {"login": "acme"}}])
        self.fake.routes[("GET", "/installation/repositories")] = (200, {}, {"repositories": []})
        talk({"GITHUB_API_URL": self.fake.url, "GITHUB_INSTALLATION_ID": ""},
             {"id": 1, "op": "list_services", "params": {}})
        self.assertEqual(len(self.fake.seen("GET", "/app/installations")), 1)
        self.assertEqual(len(self.fake.seen("POST", "/app/installations/77/access_tokens")), 1)

    def test_get_item(self):
        self.fake.routes[("GET", "/repos/acme/one/issues/5")] = (200, {}, {
            "title": "T", "body": "B", "user": {"login": "ann"}, "state": "closed",
            "labels": [{"name": "bug"}, {"name": "p1"}]})
        self.fake.routes[("GET", "/repos/acme/one/issues/5/comments")] = (200, {}, [
            {"user": {"login": "bob"}, "body": "hi", "created_at": "2026-01-01T00:00:00Z"}])
        self.assertEqual(self.ok("get_item", {"service": "acme/one", "ref": "5"}), {
            "title": "T", "body": "B", "author": "ann", "labels": ["bug", "p1"], "state": "closed",
            "is_pull_request": False, "author_trusted": False,
            "comments": [{"author": "bob", "body": "hi", "at": "2026-01-01T00:00:00Z", "mine": False}]})

    def issue(self, **kw):
        base = {"title": "T", "body": "B", "user": {"login": "ann"}, "state": "open", "labels": []}
        base.update(kw)
        self.fake.routes[("GET", "/repos/acme/one/issues/5")] = (200, {}, base)
        self.fake.routes[("GET", "/repos/acme/one/issues/5/comments")] = (200, {}, [])
        return self.ok("get_item", {"service": "acme/one", "ref": "5"})

    def test_collaborators_are_trusted_strangers_are_not(self):
        for assoc, want in [("OWNER", True), ("MEMBER", True), ("COLLABORATOR", True),
                            ("CONTRIBUTOR", False), ("FIRST_TIME_CONTRIBUTOR", False), ("NONE", False)]:
            self.fake.routes[("GET", "/app")] = (200, {}, {"slug": "tasklane-test"})
            self.assertEqual(self.issue(author_association=assoc)["author_trusted"], want, assoc)

    def test_the_apps_own_issues_are_trusted_another_bot_is_not(self):
        self.fake.routes[("GET", "/app")] = (200, {}, {"slug": "tasklane-test"})
        self.assertTrue(self.issue(user={"login": "tasklane-test[bot]"}, author_association="NONE")["author_trusted"])
        self.assertFalse(self.issue(user={"login": "evil-app[bot]"}, author_association="NONE")["author_trusted"])

    def test_the_apps_own_comments_are_flagged_by_author_not_by_a_hidden_marker(self):
        self.fake.routes[("GET", "/app")] = (200, {}, {"slug": "tasklane-test"})
        self.fake.routes[("GET", "/repos/acme/one/issues/5")] = (200, {}, {"title": "T", "body": "B", "user": {"login": "ann"}, "state": "open", "labels": []})
        self.fake.routes[("GET", "/repos/acme/one/issues/5/comments")] = (200, {}, [
            {"user": {"login": "tasklane-test[bot]"}, "body": "question? (no marker: GitHub stripped it)", "created_at": "t1"},
            {"user": {"login": "ann"}, "body": "answer", "created_at": "t2"}])
        got = self.ok("get_item", {"service": "acme/one", "ref": "5"})
        self.assertEqual([c["mine"] for c in got["comments"]], [True, False])

    def test_pull_requests_are_flagged(self):
        self.fake.routes[("GET", "/app")] = (200, {}, {"slug": "t"})
        self.assertTrue(self.issue(pull_request={"url": "x"})["is_pull_request"])
        self.assertFalse(self.issue()["is_pull_request"])

    def test_get_checks(self):
        self.fake.routes[("GET", "/repos/acme/one/pulls/3")] = (200, {}, {"head": {"sha": "abc"}})
        self.fake.routes[("GET", "/repos/acme/one/commits/abc/check-runs")] = (200, {}, {"check_runs": [
            {"name": "ci", "status": "completed", "conclusion": "success"},
            {"name": "lint", "status": "in_progress", "conclusion": None}]})
        self.fake.routes[("GET", "/repos/acme/one/commits/abc/status")] = (200, {}, {"statuses": []})
        r = self.ok("get_checks", {"service": "acme/one", "pr": "3"})
        self.assertEqual((r["head_sha"], r["state"]), ("abc", "pending"))
        self.assertEqual(r["checks"], [{"name": "ci", "state": "success"}, {"name": "lint", "state": "pending"}])

    def test_detect_facts(self):
        import base64 as b
        R = self.fake.routes
        R[("GET", "/repos/acme/one/languages")] = (200, {}, {"Python": 900, "Shell": 10})
        R[("GET", "/repos/acme/one/contents/")] = (200, {}, [{"name": "pyproject.toml"}, {"name": "README.md"}])
        R[("GET", "/repos/acme/one/environments")] = (200, {}, {"environments": [{"name": "staging"}, {"name": "prod"}]})
        R[("GET", "/repos/acme/one/contents/.github/CODEOWNERS")] = (200, {}, {
            "encoding": "base64", "content": b.b64encode(b"# c\n/infra/ @team\n*.sql @dba\n").decode()})
        r = self.ok("detect_facts", {"service": "acme/one"})
        self.assertEqual((r["language"], r["test_command"]), ("Python", "pytest"))
        self.assertEqual(r["environments"], ["staging", "prod"])
        self.assertEqual(r["protected_paths"][:5], [".github/workflows/", "Dockerfile", "migrations/",
                                                     "db/migrate/", "terraform/"])
        self.assertIn("infra/", r["protected_paths"])
        self.assertIn("*.sql", r["protected_paths"])

    def test_detect_facts_makefile_and_missing_envs(self):
        import base64 as b
        R = self.fake.routes
        R[("GET", "/repos/acme/one/languages")] = (200, {}, {})
        R[("GET", "/repos/acme/one/contents/")] = (200, {}, [{"name": "Makefile"}])
        R[("GET", "/repos/acme/one/contents/Makefile")] = (200, {}, {
            "encoding": "base64", "content": b.b64encode(b"all:\n\ntest:\n\tgo\n").decode()})
        r = self.ok("detect_facts", {"service": "acme/one"})  # environments 404 -> []
        self.assertEqual((r["test_command"], r["environments"]), ("make test", []))
        self.assertNotIn("language", r)


class ListChanges(Base):
    def setUp(self):
        super().setUp()
        self.fake.routes[("GET", "/installation/repositories")] = (200, {}, {"repositories": [
            {"full_name": "acme/one", "default_branch": "main", "archived": False, "private": False,
             "owner": {"login": "acme"}}]})

    def test_baseline_then_change_then_304(self):
        base = self.ok("list_changes", {})
        self.assertEqual(base["items"], [])
        self.assertEqual(len(self.fake.seen("GET", "/repos/acme/one/issues")), 0)

        def issues(req):
            if req["headers"].get("if-none-match") == '"e2"':
                return 304, {}, None
            return 200, {"ETag": '"e2"'}, [
                {"number": 4, "updated_at": "2030-05-01T10:00:00Z"},
                {"number": 5, "updated_at": "2030-05-01T11:00:00Z", "pull_request": {}}]
        self.fake.routes[("GET", "/repos/acme/one/issues")] = issues
        first = self.ok("list_changes", {"cursor": base["cursor"]})
        self.assertEqual([(i["ref"], i["kind"]) for i in first["items"]], [("4", "issue"), ("5", "pr")])
        q = self.fake.log[-1]["query"] if self.fake.log[-1]["path"].endswith("/issues") else None
        self.assertEqual(q["state"], ["all"])
        self.assertEqual(q["sort"], ["updated"])
        # second poll uses the advanced since; no etag known yet -> 200 again, etag learned
        second = self.ok("list_changes", {"cursor": first["cursor"]})
        self.assertEqual(json.loads(second["cursor"])["repos"]["acme/one"]["since"], "2030-05-01T11:00:00Z")
        # third poll: conditional request answered with 304
        third = self.ok("list_changes", {"cursor": second["cursor"]})
        self.assertEqual(third["items"], [])
        self.assertEqual(third["cursor"], second["cursor"])
        last = [r for r in self.fake.log if r["path"] == "/repos/acme/one/issues"][-1]
        self.assertEqual(last["headers"]["if-none-match"], '"e2"')

    def test_bad_cursor_is_permanent(self):
        self.assertEqual(self.err("list_changes", {"cursor": "garbage"})["kind"], "permanent")


class Writing(Base):
    def test_create_comment_labels(self):
        R = self.fake.routes
        R[("POST", "/repos/acme/one/issues")] = (201, {}, {"number": 12})
        R[("POST", "/repos/acme/one/issues/12/comments")] = (201, {}, {"id": 999})
        R[("POST", "/repos/acme/one/issues/12/labels")] = (200, {}, [])
        R[("DELETE", "/repos/acme/one/issues/12/labels/old%20one")] = (200, {}, [])
        R[("DELETE", "/repos/acme/one/issues/12/labels/gone")] = (404, {}, {"message": "Label does not exist"})
        self.assertEqual(self.ok("create_item", {"service": "acme/one", "title": "t", "body": "b",
                                                 "labels": ["x"]}), {"ref": "12"})
        self.assertEqual(self.fake.seen("POST", "/repos/acme/one/issues")[0]["body"],
                         {"title": "t", "body": "b", "labels": ["x"]})
        self.assertEqual(self.ok("comment", {"service": "acme/one", "ref": "12", "body": "yo"}), {"id": "999"})
        self.assertEqual(self.ok("set_labels", {"service": "acme/one", "ref": "12",
                                                "add": ["a", "b"], "remove": ["old one", "gone"]}), {})
        self.assertEqual(self.fake.seen("POST", "/repos/acme/one/issues/12/labels")[0]["body"],
                         {"labels": ["a", "b"]})
        self.assertEqual(len(self.fake.seen("DELETE", "/repos/acme/one/issues/12/labels/old%20one")), 1)


class RunTokens(Base):
    def test_mint_run_token_restricted(self):
        self.fake.routes[("POST", "/app/installations/77/access_tokens")] = (
            201, {}, {"token": RUN_TOKEN, "expires_at": "2030-01-01T00:00:00Z"})
        r = self.ok("mint_run_token", {"service": "acme/one",
                                       "permissions": ["contents:write", "pull_requests:write"], "ttl_secs": 600})
        self.assertEqual(r, {"token": RUN_TOKEN, "expires_at": "2030-01-01T00:00:00Z"})
        (req,) = self.fake.seen("POST", "/app/installations/77/access_tokens")
        self.assertEqual(req["body"], {"repositories": ["one"],
                                       "permissions": {"contents": "write", "pull_requests": "write"}})
        self.assertTrue(jwt_ok(req["headers"]["authorization"]))

    def test_mint_rejects_bad_permission(self):
        self.assertEqual(self.err("mint_run_token", {"service": "acme/one", "permissions": ["x"]})["kind"], "permanent")

    def test_repo_url(self):
        self.assertEqual(self.ok("repo_url", {"service": "acme/one"}), {"url": "https://github.com/acme/one.git"})
        self.assertEqual(self.ok("repo_url", {"service": "acme/one", "token": "T0K"})["url"],
                         "https://x-access-token:T0K@github.com/acme/one.git")

    def test_open_pr_uses_passed_token(self):
        self.fake.routes[("POST", "/repos/acme/one/pulls")] = (201, {}, {"number": 8})
        r = self.ok("open_pr", {"service": "acme/one", "branch": "feat", "base": "main",
                                "title": "T", "body": "B", "token": PASSED_TOKEN})
        self.assertEqual(r, {"pr": "8"})
        self.assertEqual(self.auth_of("POST", "/repos/acme/one/pulls"), "Bearer " + PASSED_TOKEN)
        self.assertEqual(self.fake.seen("POST", "/repos/acme/one/pulls")[0]["body"],
                         {"title": "T", "head": "feat", "base": "main", "body": "B"})
        self.assertEqual(self.fake.seen("POST", "/app/installations/77/access_tokens"), [])

    def test_open_pr_without_token_never_falls_back(self):
        e = self.err("open_pr", {"service": "acme/one", "branch": "f", "base": "m", "title": "t"})
        self.assertEqual(e["kind"], "permanent")
        self.assertEqual(self.fake.log, [])

    def pr_route(self, sha):
        self.fake.routes[("GET", "/repos/acme/one/pulls/8")] = (200, {}, {"head": {"sha": sha}, "state": "open"})

    def test_merge_pr_success(self):
        self.pr_route("aaa")
        self.fake.routes[("PUT", "/repos/acme/one/pulls/8/merge")] = (200, {}, {"merged": True, "sha": "mmm"})
        r = self.ok("merge_pr", {"service": "acme/one", "pr": "8", "expected_head_sha": "aaa", "token": PASSED_TOKEN})
        self.assertEqual(r, {"merged": True, "sha": "mmm"})
        (put,) = self.fake.seen("PUT", "/repos/acme/one/pulls/8/merge")
        self.assertEqual(put["body"], {"sha": "aaa"})
        self.assertEqual(put["headers"]["authorization"], "Bearer " + PASSED_TOKEN)
        self.assertEqual(self.auth_of("GET", "/repos/acme/one/pulls/8"), "Bearer " + PASSED_TOKEN)

    def test_open_pr_returns_the_existing_pull_request_instead_of_failing(self):
        R = self.fake.routes
        R[("POST", "/repos/acme/one/pulls")] = (422, {}, {"message": "A pull request already exists for acme:feat."})
        R[("GET", "/repos/acme/one/pulls")] = (200, {}, [{"number": 12}])
        r = self.ok("open_pr", {"service": "acme/one", "branch": "feat", "base": "main", "title": "T", "token": PASSED_TOKEN})
        self.assertEqual(r, {"pr": "12"})
        # a different 422 is still an error
        R[("POST", "/repos/acme/one/pulls")] = (422, {}, {"message": "Validation Failed"})
        self.assertEqual(self.err("open_pr", {"service": "acme/one", "branch": "feat", "base": "main", "title": "T", "token": PASSED_TOKEN})["kind"], "permanent")

    def test_merge_pr_is_idempotent_after_a_crash(self):
        self.fake.routes[("GET", "/repos/acme/one/pulls/8")] = (200, {}, {"head": {"sha": "aaa"}, "state": "closed", "merged": True, "merge_commit_sha": "mmm"})
        r = self.ok("merge_pr", {"service": "acme/one", "pr": "8", "expected_head_sha": "aaa", "token": PASSED_TOKEN})
        self.assertEqual(r, {"merged": True, "sha": "mmm"})
        self.assertEqual(self.fake.seen("PUT", "/repos/acme/one/pulls/8/merge"), [])
        # closed without a merge is not mergeable
        self.fake.routes[("GET", "/repos/acme/one/pulls/8")] = (200, {}, {"head": {"sha": "aaa"}, "state": "closed", "merged": False})
        r = self.ok("merge_pr", {"service": "acme/one", "pr": "8", "expected_head_sha": "aaa", "token": PASSED_TOKEN})
        self.assertEqual(r, {"merged": False, "reason": "not_mergeable"})

    def test_merge_pr_head_moved_does_not_merge(self):
        self.pr_route("bbb")
        r = self.ok("merge_pr", {"service": "acme/one", "pr": "8", "expected_head_sha": "aaa", "token": PASSED_TOKEN})
        self.assertEqual(r, {"merged": False, "reason": "head_moved"})
        self.assertEqual(self.fake.seen("PUT", "/repos/acme/one/pulls/8/merge"), [])

    def test_merge_pr_405_409_mapping(self):
        self.pr_route("aaa")
        cases = [(409, "Head branch was modified", "head_moved"),
                 (405, "Pull Request is not mergeable", "not_mergeable"),
                 (405, "Merge conflict", "conflict"),
                 (405, "3 of 3 required status checks are expected", "checks_failing")]
        for status, msg, reason in cases:
            self.fake.routes[("PUT", "/repos/acme/one/pulls/8/merge")] = (status, {}, {"message": msg})
            r = self.ok("merge_pr", {"service": "acme/one", "pr": "8", "expected_head_sha": "aaa",
                                     "token": PASSED_TOKEN})
            self.assertEqual(r, {"merged": False, "reason": reason}, msg)


class Webhooks(Base):
    def send(self, event, payload, secret=WEBHOOK_SECRET, sig=None):
        body = json.dumps(payload).encode()
        sig = sig or "sha256=" + hmac.new(secret.encode(), body, hashlib.sha256).hexdigest()
        return self.run1("verify_webhook", {"headers": {"X-Hub-Signature-256": sig, "X-GitHub-Event": event},
                                            "body_b64": base64.b64encode(body).decode()})

    def test_good_signatures_and_event_mapping(self):
        repo = {"full_name": "acme/one"}
        cases = [("issues", {"action": "opened", "issue": {"number": 3}, "repository": repo}, "issue"),
                 ("issues", {"action": "labeled", "issue": {"number": 3}, "repository": repo}, "label"),
                 ("issue_comment", {"issue": {"number": 3}, "repository": repo}, "comment"),
                 ("pull_request", {"action": "opened", "pull_request": {"number": 9}, "repository": repo}, "pr"),
                 ("check_suite", {"check_suite": {"pull_requests": [{"number": 9}]}, "repository": repo}, "pr")]
        for event, payload, kind in cases:
            r = self.send(event, payload)
            self.assertTrue(r["ok"], r)
            ref = "9" if kind == "pr" else "3"
            self.assertEqual(r["result"], {"event": {"service": "acme/one", "ref": ref, "kind": kind}})

    def test_bad_signatures(self):
        payload = {"issue": {"number": 3}, "repository": {"full_name": "acme/one"}}
        for r in (self.send("issues", payload, secret="wrong"),
                  self.send("issues", payload, sig="sha256=00"),
                  self.send("issues", payload, sig="nonsense")):
            self.assertFalse(r["ok"])
            self.assertEqual(r["error"]["kind"], "permanent")

    def test_unsupported_event(self):
        r = self.send("ping", {"zen": "x", "repository": {"full_name": "acme/one"}})
        self.assertFalse(r["ok"])
        self.assertEqual(r["error"]["kind"], "permanent")


class ConnectingAnApp(Base):
    CODE = "abcdef0123456789abcdef"

    def test_the_manifest_code_is_exchanged_for_the_apps_credentials_without_authentication(self):
        self.fake.routes[("POST", f"/app-manifests/{self.CODE}/conversions")] = (201, {}, {
            "id": 5677544, "slug": "my-tasklane", "name": "My tasklane", "owner": {"login": "someone"},
            "html_url": "https://github.com/apps/my-tasklane", "client_id": "Iv1.abc", "client_secret": "cs-secret-value",
            "pem": "-----BEGIN RSA PRIVATE KEY-----\nKEYDATA\n-----END RSA PRIVATE KEY-----\n", "webhook_secret": "whsec-value-1234"})
        r = self.ok("app_manifest_convert", {"code": self.CODE})
        self.assertEqual((r["app_id"], r["slug"], r["owner"]), ("5677544", "my-tasklane", "someone"))
        self.assertIn("KEYDATA", r["pem"])
        self.assertEqual(r["webhook_secret"], "whsec-value-1234")
        self.assertNotIn("client_secret", r)
        (req,) = self.fake.seen("POST", f"/app-manifests/{self.CODE}/conversions")
        self.assertNotIn("authorization", req["headers"], "the exchange is unauthenticated")

    def test_a_bad_code_never_reaches_github_and_a_used_code_is_a_clear_error(self):
        for bad in ["", "short", "has space in it 123", "../../etc/passwd", "a" * 200]:
            self.assertEqual(self.err("app_manifest_convert", {"code": bad})["kind"], "permanent", bad)
        self.assertEqual(self.fake.log, [])
        self.fake.routes[("POST", f"/app-manifests/{self.CODE}/conversions")] = (404, {}, {"message": "Not Found"})
        self.assertEqual(self.err("app_manifest_convert", {"code": self.CODE})["kind"], "permanent")

    def test_the_credentials_never_appear_in_the_plugins_logs(self):
        self.fake.routes[("POST", f"/app-manifests/{self.CODE}/conversions")] = (201, {}, {
            "id": 1, "slug": "s", "owner": {"login": "o"}, "pem": "PEMSECRETVALUE-1234567", "webhook_secret": "WHSECRETVALUE-1234", "client_secret": "CSSECRETVALUE-1234"})
        out = subprocess.run([sys.executable, "-I", str(SCRIPT)], input=json.dumps({"id": 1, "op": "app_manifest_convert", "params": {"code": self.CODE}}) + "\n",
                             env={"PATH": os.environ["PATH"], "GITHUB_API_URL": self.fake.url}, capture_output=True, text=True, timeout=30)
        self.assertNotIn("CSSECRETVALUE", out.stderr + out.stdout)
        self.assertNotIn("WHSECRETVALUE", out.stderr)
        self.assertNotIn("PEMSECRETVALUE", out.stderr)

    def test_installations_are_listed_for_the_setup_page(self):
        self.fake.routes[("GET", "/app/installations")] = (200, {}, [
            {"id": 180094190, "account": {"login": "someone"}, "repository_selection": "selected"}])
        r = self.ok("list_installations")
        self.assertEqual(r["installations"], [{"id": "180094190", "account": "someone", "repository_selection": "selected"}])


class WebhookPullRequests(Webhooks):
    def test_comments_and_issue_events_on_pull_requests_are_not_ticket_events(self):
        repo = {"full_name": "acme/one"}
        for event, payload in [
            ("issue_comment", {"repository": repo, "action": "created", "issue": {"number": 4, "pull_request": {"url": "x"}}}),
            ("issues", {"repository": repo, "action": "labeled", "issue": {"number": 4, "pull_request": {"url": "x"}}}),
        ]:
            r = self.send(event, payload)
            self.assertFalse(r["ok"], event)
            self.assertEqual(r["error"]["kind"], "permanent")
        ok = self.send("issue_comment", {"repository": repo, "action": "created", "issue": {"number": 4}})
        self.assertTrue(ok["ok"])


class ErrorMapping(Base):
    def get_item(self):
        return self.err("get_item", {"service": "acme/one", "ref": "5"})

    def route(self, resp):
        self.fake.routes[("GET", "/repos/acme/one/issues/5")] = resp

    def test_kinds(self):
        reset = str(int(time.time()) + 120)
        table = [
            ((401, {}, {"message": "Bad credentials"}), "unauthorized"),
            ((403, {}, {"message": "Resource not accessible"}), "unauthorized"),
            ((403, {"X-RateLimit-Remaining": "0", "X-RateLimit-Reset": reset}, {"message": "rate"}), "rate_limited"),
            ((429, {"Retry-After": "42"}, {"message": "slow down"}), "rate_limited"),
            ((502, {}, {"message": "bad gateway"}), "transient"),
            ((404, {}, {"message": "Not Found"}), "permanent"),
            ((422, {}, {"message": "Validation Failed"}), "permanent"),
        ]
        for resp, kind in table:
            self.route(resp)
            self.assertEqual(self.get_item()["kind"], kind, resp)

    def test_rate_limit_message_says_when(self):
        self.route((429, {"Retry-After": "42"}, {"message": "x"}))
        self.assertIn("42 seconds", self.get_item()["message"])
        reset = str(int(time.time()) + 120)
        self.route((403, {"X-RateLimit-Remaining": "0", "X-RateLimit-Reset": reset}, {"message": "x"}))
        self.assertRegex(self.get_item()["message"], r"retry after (1[12]\d) seconds")

    def test_network_error_is_transient(self):
        e = self.err("get_item", {"service": "acme/one", "ref": "5"}, env={"GITHUB_API_URL": "http://127.0.0.1:1"})
        self.assertEqual(e["kind"], "transient")

    def test_bad_key_file_is_permanent(self):
        e = self.err("list_services", env={"GITHUB_APP_PRIVATE_KEY_FILE": os.path.join(TMP, "missing.pem")})
        self.assertEqual(e["kind"], "permanent")

    def test_link_to_foreign_host_is_refused(self):
        self.fake.routes[("GET", "/installation/repositories")] = (
            200, {"Link": '<http://evil.invalid/x>; rel="next"'}, {"repositories": []})
        self.assertEqual(self.err("list_services")["kind"], "permanent")

    def test_no_secret_in_errors(self):
        self.fake.routes[("POST", "/repos/acme/one/pulls")] = (
            422, {}, {"message": f"echo {PASSED_TOKEN}"})
        out = talk({"GITHUB_API_URL": self.fake.url},
                   {"id": 1, "op": "open_pr", "params": {"service": "acme/one", "branch": "f", "base": "m",
                                                         "title": "t", "token": PASSED_TOKEN}},
                   {"id": 2, "op": "repo_url", "params": {"service": "acme/one", "token": PASSED_TOKEN}})
        self.assertNotIn(PASSED_TOKEN, json.dumps(out[0]))
        self.assertFalse(out[0]["ok"])
        self.assertEqual(out[0]["error"]["kind"], "permanent")


@unittest.skipUnless(HAVE_OPENSSL, SKIP_MSG)
class ZNoSecrets(unittest.TestCase):
    def test_no_secret_in_any_stdout_or_stderr(self):
        self.assertTrue(ALL_OUTPUT)
        for out, err in ALL_OUTPUT:
            for s in [WEBHOOK_SECRET, INST_TOKEN, PASSED_TOKEN] + KEY_LINES:
                if s == PASSED_TOKEN and "x-access-token" in out:
                    continue  # repo_url legitimately returns the token it was given
                self.assertNotIn(s, err)
                if s != PASSED_TOKEN:
                    self.assertNotIn(s, out)


if __name__ == "__main__":
    unittest.main()
