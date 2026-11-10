import json
import os
import stat
import subprocess
import sys
import tempfile
import time
import unittest
from pathlib import Path

HERE = Path(__file__).parent
sys.path.insert(0, str(HERE))
import credential_pool as pool  # noqa: E402

SCRIPT = HERE / "engine_claude.py"

# a stand-in for the CLI: what it answers depends on which credential it was given
FAKE = """#!{py}
import json, os, sys
d = os.path.dirname(os.path.abspath(__file__))
plan = json.load(open(os.path.join(d, "plan.json")))
tok = os.environ.get("CLAUDE_CODE_OAUTH_TOKEN") or os.environ.get("ANTHROPIC_API_KEY") or ""
prompt = sys.stdin.read()
with open(os.path.join(d, "calls.log"), "a") as f:
    f.write(json.dumps({{"token": tok, "interrupted": "cut off before it finished" in prompt,
                        "both": bool(os.environ.get("CLAUDE_CODE_OAUTH_TOKEN") and os.environ.get("ANTHROPIC_API_KEY"))}}) + "\\n")
how = plan.get(tok, "ok")
text = {{"ok": "Changed the file.", "limit": "Claude usage limit reached. Your limit will reset at 5pm.",
        "refused": "Invalid API key - please check it"}}[how]
print(json.dumps({{"type": "result", "subtype": "error" if how != "ok" else "success", "is_error": how != "ok",
                  "result": text, "duration_ms": 10, "usage": {{"input_tokens": 1, "output_tokens": 1}}}}))
"""

T1, T2, T3 = "sk-ant-oat01-" + "a" * 30, "sk-ant-oat01-" + "b" * 30, "sk-ant-oat01-" + "c" * 30


class Base(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.dir = Path(self.tmp.name)
        self.bin = self.dir / "claude"
        self.bin.write_text(FAKE.format(py=sys.executable))
        self.bin.chmod(0o755)
        self.pool = str(self.dir / "state" / "pool.json")
        self.ws = self.dir / "ws"
        self.ws.mkdir()
        self.plan({})

    def plan(self, d):
        (self.dir / "plan.json").write_text(json.dumps(d))

    def calls(self):
        p = self.dir / "calls.log"
        return [json.loads(l) for l in p.read_text().splitlines()] if p.exists() else []

    def talk(self, *reqs, env=None):
        e = dict(os.environ)
        for k in ("TASKLANE_SANDBOX", "CLAUDE_CODE_OAUTH_TOKEN", "ANTHROPIC_API_KEY"):
            e.pop(k, None)
        e.update({"TASKLANE_ALLOW_UNSANDBOXED": "1", "TASKLANE_CLAUDE_BIN": str(self.bin),
                  "TASKLANE_CLAUDE_POOL_FILE": self.pool})
        e.update(env or {})
        p = subprocess.run([sys.executable, "-I", str(SCRIPT)], input="\n".join(json.dumps(r) for r in reqs) + "\n",
                           env=e, capture_output=True, text=True, timeout=60)
        return [json.loads(l) for l in p.stdout.splitlines()], p.stdout

    def run_dev(self, **env):
        req = {"id": 1, "op": "run", "params": {"role": "dev", "workspace": str(self.ws),
               "ticket": {"service": "a/b", "ref": "1", "title": "t", "body": "b", "comments": []}}}
        out, raw = self.talk(req, env=env)
        return out[0], raw


class PoolFile(Base):
    def test_add_list_and_the_file_is_private_and_tokens_are_not_shown(self):
        pool.add(self.pool, "backup", "oauth", T1)
        self.assertEqual(stat.S_IMODE(os.stat(self.pool).st_mode), 0o600)
        rows = pool.status(self.pool, env={})
        self.assertEqual([r["label"] for r in rows], ["backup"])
        self.assertNotIn(T1, json.dumps(rows))
        self.assertEqual(rows[0]["token"], "…" + T1[-4:])

    def test_bad_input_is_refused(self):
        for args in [("", "oauth", T1), ("x" * 41, "oauth", T1), ("a", "password", T1), ("a", "oauth", "short"),
                     ("a", "oauth", "has space " + "z" * 30), ("environment", "oauth", T1)]:
            with self.assertRaises(pool.PoolError, msg=str(args)):
                pool.add(self.pool, *args)
        pool.add(self.pool, "a", "oauth", T1)
        with self.assertRaises(pool.PoolError):
            pool.add(self.pool, "a", "oauth", T2)

    def test_order_is_priority_and_sleeping_or_disabled_accounts_are_skipped(self):
        for label, tok in (("one", T1), ("two", T2), ("three", T3)):
            pool.add(self.pool, label, "oauth", tok)
        now = time.time()
        self.assertEqual([c["label"] for c in pool.candidates(self.pool, env={}, now=now)], ["one", "two", "three"])
        pool.sleep_for(self.pool, "one", "usage limit", 600, now=now)
        pool.set_enabled(self.pool, "three", False)
        self.assertEqual([c["label"] for c in pool.candidates(self.pool, env={}, now=now)], ["two"])
        self.assertEqual([c["label"] for c in pool.candidates(self.pool, env={}, now=now + 601)], ["one", "two"])
        pool.move(self.pool, "two", -1)
        self.assertEqual([c["label"] for c in pool.candidates(self.pool, env={}, now=now + 601)], ["two", "one"])
        self.assertEqual(pool.earliest_wake(self.pool, now=now), 600)
        pool.reset(self.pool, "one")
        self.assertIsNone(pool.earliest_wake(self.pool, now=now))

    def test_the_environment_credential_comes_first(self):
        pool.add(self.pool, "backup", "oauth", T2)
        c = pool.candidates(self.pool, env={"ANTHROPIC_API_KEY": "sk-ant-api03-" + "k" * 30})
        self.assertEqual([x["label"] for x in c], ["environment", "backup"])
        self.assertEqual(c[0]["var"], "ANTHROPIC_API_KEY")

    def test_a_broken_file_is_an_error_not_a_crash(self):
        os.makedirs(os.path.dirname(self.pool), exist_ok=True)
        Path(self.pool).write_text("{nope")
        with self.assertRaises(pool.PoolError):
            pool.status(self.pool, env={})


class Failover(Base):
    def test_a_used_up_account_hands_the_same_run_to_the_next(self):
        pool.add(self.pool, "main", "oauth", T1)
        pool.add(self.pool, "backup", "oauth", T2)
        self.plan({T1: "limit"})
        r, _ = self.run_dev()
        self.assertTrue(r["ok"], r)
        self.assertEqual(r["result"]["account"], "backup")
        self.assertEqual([c["token"] for c in self.calls()], [T1, T2])
        self.assertFalse(self.calls()[0]["interrupted"])
        self.assertTrue(self.calls()[1]["interrupted"], "the developer is told part of the work may be there")
        self.assertEqual({c["both"] for c in self.calls()}, {False}, "one credential at a time")
        row = pool.status(self.pool, env={})[0]
        self.assertGreater(row["cooldown_secs"], 0)
        self.assertEqual(row["last_error"], "usage limit reached")

    def test_the_next_run_skips_the_sleeping_account_without_asking_it_again(self):
        pool.add(self.pool, "main", "oauth", T1)
        pool.add(self.pool, "backup", "oauth", T2)
        self.plan({T1: "limit"})
        self.run_dev()
        self.run_dev()
        self.assertEqual([c["token"] for c in self.calls()], [T1, T2, T2])

    def test_a_refused_credential_sleeps_for_a_day(self):
        pool.add(self.pool, "main", "oauth", T1)
        pool.add(self.pool, "backup", "oauth", T2)
        self.plan({T1: "refused"})
        r, _ = self.run_dev()
        self.assertTrue(r["ok"], r)
        row = pool.status(self.pool, env={})[0]
        self.assertGreater(row["cooldown_secs"], 80000)
        self.assertIn("refused", row["last_error"])

    def test_when_every_account_is_used_up_the_error_says_when_to_come_back(self):
        pool.add(self.pool, "main", "oauth", T1)
        pool.add(self.pool, "backup", "oauth", T2)
        self.plan({T1: "limit", T2: "limit"})
        r, _ = self.run_dev(TASKLANE_CLAUDE_COOLDOWN_SECS="900")
        self.assertFalse(r["ok"])
        self.assertEqual(r["error"]["kind"], "rate_limited")
        self.assertTrue(800 <= r["error"]["retry_after_secs"] <= 900, r)
        # and the next call does not even start the CLI
        n = len(self.calls())
        r, _ = self.run_dev()
        self.assertEqual(r["error"]["kind"], "rate_limited")
        self.assertEqual(len(self.calls()), n)

    def test_an_error_that_is_not_about_the_account_does_not_switch(self):
        pool.add(self.pool, "main", "oauth", T1)
        pool.add(self.pool, "backup", "oauth", T2)
        (self.dir / "claude").write_text("#!/bin/sh\necho garbage\n")
        r, _ = self.run_dev()
        self.assertFalse(r["ok"])
        self.assertNotEqual(r["error"]["kind"], "rate_limited")
        self.assertEqual([a["cooldown_secs"] for a in pool.status(self.pool, env={})], [0, 0])

    def test_tokens_never_reach_the_output(self):
        pool.add(self.pool, "main", "oauth", T1)
        r, raw = self.run_dev()
        self.assertNotIn(T1, raw)
        out, raw = self.talk({"id": 2, "op": "accounts"})
        self.assertNotIn(T1, raw)
        self.assertEqual(out[0]["result"]["rows"][0]["label"], "main")

    def test_without_a_pool_file_nothing_changes(self):
        self.plan({})
        r, _ = self.run_dev(TASKLANE_CLAUDE_POOL_FILE="", CLAUDE_CODE_OAUTH_TOKEN=T1)
        self.assertTrue(r["ok"], r)
        self.assertNotIn("account", r["result"])
        self.assertEqual([c["token"] for c in self.calls()], [T1])

    def test_a_file_that_holds_a_token_scrubs_it_from_model_text(self):
        pool.add(self.pool, "main", "oauth", T1)
        (self.dir / "claude").write_text("#!%s\nimport json\nprint(json.dumps({'type':'result','subtype':'success','is_error':False,"
                                         "'result':'I saw %s','duration_ms':1,'usage':{}}))\n" % (sys.executable, T1))
        r, raw = self.run_dev()
        self.assertTrue(r["ok"], r)
        self.assertNotIn(T1, raw)
        self.assertIn("[redacted]", r["result"]["summary"])


class Operations(Base):
    def act(self, **p):
        out, _ = self.talk({"id": 1, "op": "account", "params": p})
        return out[0]

    def test_manage_accounts_through_the_protocol(self):
        r = self.act(action="add", label="backup", kind="oauth", token=T2)
        self.assertTrue(r["ok"], r)
        out, _ = self.talk({"id": 2, "op": "accounts"})
        self.assertEqual([x["label"] for x in out[0]["result"]["rows"]], ["backup"])
        self.assertTrue(self.act(action="disable", label="backup")["ok"])
        self.assertFalse(pool.status(self.pool, env={})[0]["enabled"])
        self.assertTrue(self.act(action="enable", label="backup")["ok"])
        self.assertTrue(self.act(action="remove", label="backup")["ok"])
        self.assertEqual(pool.status(self.pool, env={}), [])

    def test_errors_are_plain(self):
        self.assertIn("look like a token", self.act(action="add", label="x", kind="oauth", token="abc")["error"]["message"])
        self.assertIn("no account called", self.act(action="remove", label="ghost")["error"]["message"])
        self.assertIn("unknown account action", self.act(action="frobnicate", label="x")["error"]["message"])

    def test_without_a_pool_file_the_console_is_told_so(self):
        out, _ = self.talk({"id": 1, "op": "accounts"}, env={"TASKLANE_CLAUDE_POOL_FILE": ""})
        self.assertEqual(out[0]["result"], {"configured": False, "rows": []})

    def test_command_line_adds_from_standard_input(self):
        env = dict(os.environ, TASKLANE_CLAUDE_POOL_FILE=self.pool)
        p = subprocess.run([sys.executable, str(HERE / "credential_pool.py"), "add", "cli", "oauth"], input=T3 + "\n",
                           env=env, capture_output=True, text=True)
        self.assertEqual(p.returncode, 0, p.stderr)
        p = subprocess.run([sys.executable, str(HERE / "credential_pool.py"), "list"], env=env, capture_output=True, text=True)
        self.assertIn("cli", p.stdout)
        self.assertNotIn(T3, p.stdout)


if __name__ == "__main__":
    unittest.main()
