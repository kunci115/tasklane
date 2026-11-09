import base64
import json
import os
import subprocess
import sys
import threading
import unittest
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from urllib.parse import urlsplit, parse_qs

SCRIPT = Path(__file__).with_name("source_gitlab.py")
TOKEN = "glpat-FAKEACCESSTOKEN000111"
HOOK = "hook-SECRET-0123456789"
PID = "acme%2Fshop"  # how the path appears in /projects/<id>


class Fake(ThreadingHTTPServer):
    daemon_threads = True

    def __init__(self):
        super().__init__(("127.0.0.1", 0), Handler)
        self.routes, self.log = {}, []
        threading.Thread(target=self.serve_forever, daemon=True).start()

    @property
    def url(self):
        return "http://127.0.0.1:%d" % self.server_address[1]

    def seen(self, method, path):
        return [r for r in self.log if r["method"] == method and r["path"] == path]


class Handler(BaseHTTPRequestHandler):
    def log_message(self, *a):
        pass

    def _do(self):
        u = urlsplit(self.path)
        n = int(self.headers.get("Content-Length") or 0)
        raw = self.rfile.read(n) if n else b""
        req = {"method": self.command, "path": urlsplit(self.path).path, "raw_path": u.path, "query": parse_qs(u.query),
               "headers": {k.lower(): v for k, v in self.headers.items()}, "body": json.loads(raw) if raw else None}
        self.server.log.append(req)
        route = self.server.routes.get((self.command, u.path))
        status, headers, body = (route(req) if callable(route) else route) if route else (404, {}, {"message": "404 Not Found"})
        data = b"" if body is None else (body.encode() if isinstance(body, str) else json.dumps(body).encode())
        self.send_response(status)
        for k, v in headers.items():
            self.send_header(k, v)
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    do_GET = do_POST = do_PUT = do_DELETE = _do


def talk(env_extra, *requests):
    env = {"PATH": os.environ["PATH"], "GITLAB_TOKEN": TOKEN, "GITLAB_WEBHOOK_SECRET": HOOK}
    env.update(env_extra)
    p = subprocess.run([sys.executable, "-I", str(SCRIPT)], input="\n".join(json.dumps(r) for r in requests) + "\n",
                       capture_output=True, text=True, timeout=60, env=env)
    wants_token = any(r.get("op") in ("mint_run_token", "repo_url") for r in requests)
    assert (wants_token or TOKEN not in p.stdout) and TOKEN not in p.stderr and HOOK not in p.stdout + p.stderr, "a secret leaked"
    return [json.loads(l) for l in p.stdout.splitlines()]


class Base(unittest.TestCase):
    def setUp(self):
        self.fake = Fake()
        self.addCleanup(self.fake.server_close)
        self.addCleanup(self.fake.shutdown)
        self.R = self.fake.routes
        self.R[("GET", "/api/v4/user")] = (200, {}, {"id": 1, "username": "tasklane-bot"})

    def call(self, op, params=None, env=None):
        (r,) = talk(dict({"GITLAB_URL": self.fake.url}, **(env or {})), {"id": 1, "op": op, "params": params or {}})
        return r

    def ok(self, op, params=None, env=None):
        r = self.call(op, params, env)
        self.assertTrue(r["ok"], r)
        return r["result"]

    def err(self, op, params=None, env=None):
        r = self.call(op, params, env)
        self.assertFalse(r["ok"], r)
        return r["error"]

    def project(self, path="acme/shop"):
        return "/api/v4/projects/" + path.replace("/", "%2F")

    def member(self, uid, level):
        self.R[("GET", "/api/v4/projects/acme%2Fshop/members/all/" + str(uid))] = (200, {}, {"id": uid, "access_level": level})


class Ops(Base):
    def test_describe_and_services(self):
        self.assertEqual(self.ok("describe")["name"], "gitlab")
        self.R[("GET", "/api/v4/projects")] = (200, {}, [
            {"path_with_namespace": "acme/shop", "default_branch": "main", "archived": False, "visibility": "private"},
            {"path_with_namespace": "other/thing", "default_branch": None, "archived": True, "visibility": "public"}])
        s = self.ok("list_services")["services"]
        self.assertEqual([(x["external_id"], x["private"], x["archived"]) for x in s], [("acme/shop", True, False), ("other/thing", False, True)])
        self.assertEqual(self.fake.seen("GET", "/api/v4/projects")[0]["query"]["min_access_level"], ["30"])
        only = self.ok("list_services", env={"GITLAB_GROUP": "acme"})["services"]
        self.assertEqual([x["external_id"] for x in only], ["acme/shop"])
        self.assertEqual(self.fake.log[0]["headers"]["private-token"], TOKEN)

    def test_changes_baseline_then_only_newer_issues(self):
        self.R[("GET", "/api/v4/projects")] = (200, {}, [{"path_with_namespace": "acme/shop", "visibility": "private"}])
        base = self.ok("list_changes")
        self.assertEqual(base["items"], [])
        self.R[("GET", self.project() + "/issues")] = (200, {}, [{"iid": 4, "updated_at": "2030-05-01T10:00:00Z"}, {"iid": 5, "updated_at": "2030-05-01T11:00:00Z"}])
        r = self.ok("list_changes", {"cursor": base["cursor"]})
        self.assertEqual([(i["ref"], i["kind"]) for i in r["items"]], [("4", "issue"), ("5", "issue")])
        self.assertEqual(json.loads(r["cursor"])["repos"]["acme/shop"]["since"], "2030-05-01T11:00:00Z")
        self.assertIn("updated_after", self.fake.seen("GET", self.project() + "/issues")[0]["query"])
        self.assertEqual(self.err("list_changes", {"cursor": "garbage"})["kind"], "permanent")

    def test_an_item_with_trust_and_system_notes_left_out(self):
        self.R[("GET", self.project() + "/issues/5")] = (200, {}, {"iid": 5, "title": "Login fails", "description": "blank", "state": "opened",
                                                                  "labels": ["bug"], "author": {"id": 9, "username": "stranger"}})
        self.R[("GET", self.project() + "/issues/5/notes")] = (200, {}, [
            {"system": True, "body": "added label", "author": {"id": 2}, "created_at": "t0"},
            {"body": "me too", "author": {"id": 2, "username": "dev"}, "created_at": "t1"},
            {"body": "status", "author": {"id": 1, "username": "tasklane-bot"}, "created_at": "t2"}])
        self.member(9, 20)
        self.member(2, 40)
        got = self.ok("get_item", {"service": "acme/shop", "ref": "5"})
        self.assertEqual((got["title"], got["author"], got["state"], got["author_trusted"]), ("Login fails", "stranger", "open", False))
        self.assertEqual([(c["author"], c["trusted"], c["mine"]) for c in got["comments"]], [("dev", True, False), ("tasklane-bot", True, True)])

    def test_writes(self):
        self.R[("POST", self.project() + "/issues")] = (201, {}, {"iid": 12})
        self.R[("POST", self.project() + "/issues/12/notes")] = (201, {}, {"id": 77})
        self.R[("PUT", self.project() + "/issues/12")] = (200, {}, {})
        self.assertEqual(self.ok("create_item", {"service": "acme/shop", "title": "T", "body": "B", "labels": ["tasklane"]}), {"ref": "12"})
        self.assertEqual(self.fake.seen("POST", self.project() + "/issues")[0]["body"]["labels"], "tasklane")
        self.assertEqual(self.ok("comment", {"service": "acme/shop", "ref": "12", "body": "hi"}), {"id": "77"})
        self.ok("set_labels", {"service": "acme/shop", "ref": "12", "add": ["a", "b"], "remove": ["c"]})
        self.assertEqual(self.fake.seen("PUT", self.project() + "/issues/12")[0]["body"], {"add_labels": "a,b", "remove_labels": "c"})

    def test_facts_and_the_repository_rules_file(self):
        self.R[("GET", self.project() + "/languages")] = (200, {}, {"Python": 80.0, "Shell": 20.0})
        self.R[("GET", self.project() + "/repository/tree")] = (200, {}, [{"name": "pytest.ini"}, {"name": "README.md"}])
        f = self.ok("detect_facts", {"service": "acme/shop"})
        self.assertEqual((f["language"], f["test_command"]), ("Python", "pytest"))
        self.R[("GET", self.project())] = (200, {}, {"default_branch": "trunk"})
        self.R[("GET", self.project() + "/repository/files/.tasklane.yml/raw")] = (200, {}, "always_ask: true\nprotected_paths:\n  - payments/\ntest_command: rm -rf /\n")
        o = self.ok("read_overrides", {"service": "acme/shop"})
        self.assertEqual(o["overrides"], {"always_ask": True, "protected_paths": ["payments/"]})
        self.assertIn("test_command", o["ignored"])
        self.assertEqual(self.fake.seen("GET", self.project() + "/repository/files/.tasklane.yml/raw")[0]["query"]["ref"], ["trunk"])
        self.R[("GET", self.project() + "/repository/files/.tasklane.yml/raw")] = (404, {}, {"message": "404 File Not Found"})
        self.assertEqual(self.ok("read_overrides", {"service": "acme/shop"}), {"overrides": {}, "found": False})

    def test_repo_url_and_tokens(self):
        u = self.ok("repo_url", {"service": "acme/shop"})["url"]
        self.assertEqual(u, self.fake.url + "/acme/shop.git")
        t = self.ok("repo_url", {"service": "acme/shop", "token": "abc123def"})["url"]
        self.assertIn("oauth2:abc123def@", t)
        run = self.ok("mint_run_token", {"service": "acme/shop", "permissions": ["contents:write"], "ttl_secs": 600})
        self.assertEqual(run["token"], TOKEN)
        self.assertEqual(self.err("repo_url", {"service": "not a path"})["kind"], "permanent")


class MergeRequests(Base):
    def test_open_and_reuse_an_existing_merge_request(self):
        self.R[("POST", self.project() + "/merge_requests")] = (201, {}, {"iid": 8})
        self.assertEqual(self.ok("open_pr", {"service": "acme/shop", "branch": "tasklane/1", "base": "main", "title": "T", "body": "B", "token": "x"}), {"pr": "8"})
        self.R[("POST", self.project() + "/merge_requests")] = (409, {}, {"message": ["Another open merge request already exists for this source branch: !8"]})
        self.R[("GET", self.project() + "/merge_requests")] = (200, {}, [{"iid": 8}])
        self.assertEqual(self.ok("open_pr", {"service": "acme/shop", "branch": "tasklane/1", "base": "main", "title": "T"}), {"pr": "8"})

    def test_checks_come_from_the_pipeline_of_the_head_commit(self):
        self.R[("GET", self.project() + "/merge_requests/8")] = (200, {}, {"iid": 8, "sha": "abc"})
        self.R[("GET", self.project() + "/merge_requests/8/pipelines")] = (200, {}, [{"id": 1, "sha": "old"}, {"id": 2, "sha": "abc"}])
        self.R[("GET", self.project() + "/pipelines/2/jobs")] = (200, {}, [{"name": "test", "status": "success"}, {"name": "lint", "status": "running"}])
        r = self.ok("get_checks", {"service": "acme/shop", "pr": "8"})
        self.assertEqual((r["head_sha"], r["state"]), ("abc", "pending"))
        self.R[("GET", self.project() + "/pipelines/2/jobs")] = (200, {}, [{"name": "test", "status": "success"}, {"name": "lint", "status": "failed"}])
        self.assertEqual(self.ok("get_checks", {"service": "acme/shop", "pr": "8"})["state"], "failure")
        self.R[("GET", self.project() + "/merge_requests/8/pipelines")] = (200, {}, [])
        self.assertEqual(self.ok("get_checks", {"service": "acme/shop", "pr": "8"})["checks"], [], "no pipeline: no checks, which tasklane treats as pending")

    def test_merge_only_the_reviewed_commit(self):
        p = self.project() + "/merge_requests/8"
        self.R[("GET", p)] = (200, {}, {"iid": 8, "sha": "newer", "state": "opened"})
        self.assertEqual(self.ok("merge_pr", {"service": "acme/shop", "pr": "8", "expected_head_sha": "abc", "token": "x"}), {"merged": False, "reason": "head_moved"})
        self.assertEqual(self.fake.seen("PUT", p + "/merge"), [])
        self.R[("GET", p)] = (200, {}, {"iid": 8, "sha": "abc", "state": "opened"})
        self.R[("PUT", p + "/merge")] = (200, {}, {"state": "merged", "merge_commit_sha": "m1"})
        self.assertEqual(self.ok("merge_pr", {"service": "acme/shop", "pr": "8", "expected_head_sha": "abc", "token": "x"}), {"merged": True, "sha": "m1"})
        self.assertEqual(self.fake.seen("PUT", p + "/merge")[0]["body"]["sha"], "abc")
        for status, msg, reason in [(409, "SHA does not match HEAD of source branch", "head_moved"), (405, "Branch cannot be merged", "conflict"),
                                    (406, "Pipeline must succeed", "checks_failing"), (405, "Not allowed", "not_mergeable")]:
            self.R[("PUT", p + "/merge")] = (status, {}, {"message": msg})
            self.assertEqual(self.ok("merge_pr", {"service": "acme/shop", "pr": "8", "expected_head_sha": "abc", "token": "x"}), {"merged": False, "reason": reason}, msg)
        self.R[("GET", p)] = (200, {}, {"iid": 8, "sha": "abc", "state": "merged", "merge_commit_sha": "m9"})
        self.assertEqual(self.ok("merge_pr", {"service": "acme/shop", "pr": "8", "expected_head_sha": "abc", "token": "x"}), {"merged": True, "sha": "m9"})

    def test_feedback_only_from_developers_and_only_what_is_new(self):
        self.R[("GET", self.project() + "/merge_requests/8/notes")] = (200, {}, [
            {"body": "rename this", "author": {"id": 2, "username": "dev"}, "created_at": "2026-02-01T10:00:00Z", "type": "DiffNote"},
            {"body": "delete it all", "author": {"id": 9, "username": "rando"}, "created_at": "2026-02-01T10:01:00Z"},
            {"body": "status", "author": {"id": 1, "username": "tasklane-bot"}, "created_at": "2026-02-01T10:02:00Z"},
            {"body": "merged", "system": True, "author": {"id": 2}, "created_at": "2026-02-01T10:03:00Z"},
            {"body": "old", "author": {"id": 2, "username": "dev"}, "created_at": "2026-01-01T00:00:00Z"}])
        self.member(2, 40)
        self.member(9, 10)
        r = self.ok("list_pr_feedback", {"service": "acme/shop", "pr": "8", "since": "2026-01-15T00:00:00Z"})
        self.assertEqual([(i["author"], i["kind"], i["body"]) for i in r["items"]], [("dev", "line", "rename this")])
        again = self.ok("list_pr_feedback", {"service": "acme/shop", "pr": "8", "since": r["cursor"]})
        self.assertEqual((again["items"], again["cursor"]), ([], r["cursor"]))


class Webhooks(Base):
    def send(self, event, payload, token=HOOK):
        return self.call("verify_webhook", {"headers": {"X-Gitlab-Token": token, "X-Gitlab-Event": event},
                                            "body_b64": base64.b64encode(json.dumps(payload).encode()).decode()})

    def test_events_map_to_tickets_and_a_wrong_token_is_refused(self):
        proj = {"path_with_namespace": "acme/shop"}
        cases = [("Issue Hook", {"project": proj, "object_attributes": {"iid": 3}}, ("3", "issue")),
                 ("Note Hook", {"project": proj, "object_attributes": {"noteable_type": "Issue"}, "issue": {"iid": 3}}, ("3", "comment")),
                 ("Note Hook", {"project": proj, "object_attributes": {"noteable_type": "MergeRequest"}, "merge_request": {"iid": 8}}, ("8", "pr")),
                 ("Merge Request Hook", {"project": proj, "object_attributes": {"iid": 8}}, ("8", "pr")),
                 ("Pipeline Hook", {"project": proj, "merge_request": {"iid": 8}}, ("8", "pr"))]
        for event, payload, (ref, kind) in cases:
            r = self.send(event, payload)
            self.assertTrue(r["ok"], r)
            self.assertEqual(r["result"]["event"], {"service": "acme/shop", "ref": ref, "kind": kind})
        self.assertFalse(self.send("Issue Hook", {"project": proj, "object_attributes": {"iid": 3}}, token="wrong")["ok"])
        self.assertFalse(self.send("Push Hook", {"project": proj})["ok"])
        self.assertFalse(self.send("Issue Hook", {"object_attributes": {"iid": 3}})["ok"])


class Ownership(Base):
    def branches(self, *names):
        self.R[("GET", self.project() + "/repository/branches")] = (200, {}, [{"name": n} for n in names])

    def test_claim_when_nobody_has_one_and_the_older_claim_wins(self):
        self.branches()
        self.R[("GET", self.project())] = (200, {}, {"default_branch": "main"})
        self.R[("GET", self.project() + "/repository/branches/main")] = (200, {}, {"name": "main"})
        self.R[("POST", self.project() + "/repository/branches")] = (201, {}, {})
        r = self.ok("claim_repo", {"service": "acme/shop", "instance": "mine_abc"})
        post = self.fake.seen("POST", self.project() + "/repository/branches")[0]["query"]
        self.assertTrue(post["branch"][0].startswith("tasklane-owner-") and post["branch"][0].endswith("-mine_abc"))
        self.assertEqual(post["ref"], ["main"])
        # another install holds an older claim: ours is not made, and the report says who owns it
        self.fake.log.clear()
        self.branches("tasklane-owner-100-theirs_xyz")
        r = self.ok("claim_repo", {"service": "acme/shop", "instance": "mine_abc"})
        self.assertEqual((r["owner"], r["mine"]), ("theirs_xyz", False))
        self.assertEqual(self.fake.seen("POST", self.project() + "/repository/branches"), [])
        # both claimed at once: the younger one gives its own back
        self.branches("tasklane-owner-100-theirs_xyz", "tasklane-owner-105-mine_abc")
        self.R[("DELETE", self.project() + "/repository/branches/tasklane-owner-105-mine_abc")] = (204, {}, None)
        r = self.ok("claim_repo", {"service": "acme/shop", "instance": "mine_abc"})
        self.assertEqual((r["owner"], r["mine"]), ("theirs_xyz", False))
        self.assertEqual(len(self.fake.seen("DELETE", self.project() + "/repository/branches/tasklane-owner-105-mine_abc")), 1)

    def test_an_empty_project_cannot_be_guarded_yet_and_takeover_removes_other_claims(self):
        self.branches()
        self.R[("GET", self.project())] = (200, {}, {"default_branch": "main"})
        r = self.ok("claim_repo", {"service": "acme/shop", "instance": "mine_abc"})
        self.assertEqual(r["guard"], "unavailable")
        self.branches("tasklane-owner-100-theirs_xyz")
        self.R[("DELETE", self.project() + "/repository/branches/tasklane-owner-100-theirs_xyz")] = (204, {}, None)
        self.R[("GET", self.project() + "/repository/branches/main")] = (200, {}, {"name": "main"})
        self.R[("POST", self.project() + "/repository/branches")] = (201, {}, {})
        self.ok("takeover_repo", {"service": "acme/shop", "instance": "mine_abc"})
        self.assertEqual(len(self.fake.seen("DELETE", self.project() + "/repository/branches/tasklane-owner-100-theirs_xyz")), 1)
        self.assertEqual(self.err("claim_repo", {"service": "acme/shop", "instance": "x"})["kind"], "permanent")


class Errors(Base):
    def test_errors_are_classified_and_redirects_are_not_followed(self):
        self.R[("GET", "/api/v4/projects")] = (401, {}, {"message": "401 Unauthorized"})
        self.assertEqual(self.err("list_services")["kind"], "unauthorized")
        self.R[("GET", "/api/v4/projects")] = (429, {}, {"message": "rate"})
        self.assertEqual(self.err("list_services")["kind"], "rate_limited")
        self.R[("GET", "/api/v4/projects")] = (502, {}, {"message": "bad gateway"})
        self.assertEqual(self.err("list_services")["kind"], "transient")
        self.R[("GET", "/api/v4/projects")] = (302, {"Location": "http://evil.example/"}, None)
        self.assertEqual(self.err("list_services")["kind"], "permanent")
        self.assertEqual(self.err("describe_nothing")["kind"], "permanent")
        r = talk({"GITLAB_TOKEN": ""}, {"id": 1, "op": "list_services"})
        self.assertEqual(r[0]["error"]["kind"], "permanent")


if __name__ == "__main__":
    unittest.main()
