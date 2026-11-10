import json
import os
import subprocess
import sys
import tempfile
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import run_evals as ev  # noqa: E402

STUB = r'''
import json, os, sys
mode = os.environ.get("STUB_MODE", "good")
for line in sys.stdin:
    req = json.loads(line)
    p = req["params"]
    role = p["role"]
    t = p["ticket"]
    if mode == "crash":
        sys.exit(1)
    if mode == "error":
        print(json.dumps({"id": req["id"], "ok": False, "error": {"kind": "transient", "message": "rate limited"}}), flush=True)
        continue
    if role == "triage":
        out = {"level": 2 if "migration" in t["title"] else 1, "plan": "p", "questions": []}
    elif role == "reviewer":
        out = {"verdict": "changes" if "password" in p["diff"] else "approve", "notes": "n"}
    elif role == "duplicate":
        hit = [c["ref"] for c in p["candidates"] if c["title"] == t["title"]]
        out = {"duplicate_of": hit[0] if hit else None, "reason": "r"}
    else:
        out = {"status": "continue", "next_action": "status", "params": {"host": "1.2.3.4"} if mode == "leak" else {}, "reason": "r"}
    if mode == "worse" and role == "triage":
        out["level"] = 1
    out["usage"] = {"tokens_in": 10, "tokens_out": 5, "secs": 0}
    print(json.dumps({"id": req["id"], "ok": True, "result": out}), flush=True)
'''

CASES = [
    {"id": "t-low", "role": "triage", "why": "w", "ticket": {"title": "typo"}, "expect": {"level": [1], "questions": "none"}},
    {"id": "t-high", "role": "triage", "why": "w", "ticket": {"title": "a migration"}, "expect": {"level": [2, 3]}},
    {"id": "r-bad", "role": "reviewer", "why": "w", "ticket": {"title": "x"}, "diff": "+ password = 'x'", "expect": {"verdict_not": ["approve"]}},
    {"id": "d-same", "role": "duplicate", "why": "w", "ticket": {"title": "same"},
     "candidates": [{"ref": "9", "title": "same"}], "expect": {"duplicate_of": "9"}},
    {"id": "v-1", "role": "device", "why": "w", "ticket": {"title": "cam"}, "observations": [], "actions": [],
     "expect": {"status": ["continue"], "params_lack": ["host"]}},
]


class Harness(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.cases = os.path.join(self.tmp.name, "cases")
        os.mkdir(self.cases)
        self.stub = os.path.join(self.tmp.name, "stub.py")
        with open(self.stub, "w") as f:
            f.write(STUB)
        self.baseline = os.path.join(self.tmp.name, "baseline.json")
        self.write_cases(CASES)

    def write_cases(self, cases, name="x.jsonl"):
        with open(os.path.join(self.cases, name), "w") as f:
            for c in cases:
                f.write(json.dumps(c) + "\n")

    def run_cli(self, *args, mode="good"):
        env = dict(os.environ, STUB_MODE=mode)
        p = subprocess.run([sys.executable, os.path.join(HERE, "run_evals.py"), "--cases", self.cases,
                            "--engine", "%s %s" % (sys.executable, self.stub), "--baseline", self.baseline, *args],
                           capture_output=True, text=True, env=env, timeout=60)
        return p.returncode, p.stdout + p.stderr

    def test_the_cases_shipped_with_tasklane_are_well_formed(self):
        cases = ev.load_cases(os.path.join(HERE, "cases"))
        self.assertGreaterEqual(len(cases), 20)
        self.assertEqual({c["role"] for c in cases}, set(ev.ROLES))

    def test_a_good_engine_passes_every_case_and_tokens_are_counted(self):
        code, out = self.run_cli("--write-baseline")
        self.assertEqual(code, 0, out)
        self.assertIn("all        5/5 passed (100%)", out)
        self.assertIn("15 tokens", out)
        with open(self.baseline) as f:
            self.assertEqual(set(json.load(f)["cases"].values()), {1.0})

    def test_a_case_that_got_worse_than_the_baseline_fails_the_run(self):
        self.assertEqual(self.run_cli("--write-baseline")[0], 0)
        code, out = self.run_cli(mode="worse")
        self.assertEqual(code, 1, out)
        self.assertIn("REGRESSION", out)
        self.assertIn("t-high", out)
        self.assertIn("100% -> 0%", out)

    def test_same_results_as_the_baseline_pass(self):
        self.run_cli("--write-baseline")
        code, out = self.run_cli()
        self.assertEqual(code, 0, out)
        self.assertNotIn("REGRESSION", out)

    def test_a_failing_case_that_was_already_failing_is_not_a_regression(self):
        self.run_cli("--write-baseline", mode="worse")
        code, out = self.run_cli(mode="worse")
        self.assertEqual(code, 0, out)
        self.assertIn("- t-high:", out, "it is still listed")

    def test_tolerance_allows_a_small_drop(self):
        self.run_cli("--write-baseline", "--repeat", "2")
        with open(self.baseline) as f:
            b = json.load(f)
        b["cases"]["t-high"] = 1.0
        with open(self.baseline, "w") as f:
            json.dump(b, f)
        code, _ = self.run_cli(mode="worse")
        self.assertEqual(code, 1)
        code, _ = self.run_cli("--tolerance", "1", mode="worse")
        self.assertEqual(code, 0)

    def test_an_answer_that_carries_an_address_fails_the_device_case(self):
        code, out = self.run_cli(mode="leak")
        self.assertIn("params carry 'host'", out)
        self.assertEqual(code, 0, "no baseline: nothing to compare with, errors only fail the run")

    def test_engine_errors_fail_the_run_even_without_a_baseline(self):
        code, out = self.run_cli(mode="error")
        self.assertEqual(code, 1, out)
        self.assertIn("rate limited", out)

    def test_an_engine_that_dies_counts_as_errors_not_a_crash_of_the_runner(self):
        code, out = self.run_cli(mode="crash")
        self.assertEqual(code, 1, out)
        self.assertIn("engine", out)

    def test_role_filter_and_partial_baseline_keep_the_other_roles(self):
        self.run_cli("--write-baseline")
        code, out = self.run_cli("--write-baseline", "--role", "triage", mode="worse")
        self.assertEqual(code, 0, out)
        with open(self.baseline) as f:
            b = json.load(f)["cases"]
        self.assertEqual(b["t-high"], 0.0)
        self.assertEqual(b["r-bad"], 1.0, "reviewer cases were not run and keep their number")

    def test_bad_case_files_are_refused_with_a_reason(self):
        for bad, needle in [
            ({"id": "x", "role": "nope", "why": "w", "ticket": {}, "expect": {"a": 1}}, "role must be"),
            ({"id": "x", "role": "triage", "ticket": {}, "expect": {"level": [1]}}, "say why"),
            ({"id": "x", "role": "triage", "why": "w", "ticket": {}, "expect": {"verdict": ["approve"]}}, "do not know expect keys"),
            ({"id": "x", "role": "reviewer", "why": "w", "ticket": {}, "expect": {"verdict": ["approve"]}}, "needs 'diff'"),
            ({"id": "x", "role": "triage", "why": "w", "ticket": {}, "expect": {}}, "non-empty"),
        ]:
            self.write_cases([bad], "bad.jsonl")
            code, out = self.run_cli("--validate")
            self.assertEqual(code, 2, out)
            self.assertIn(needle, out)
            os.remove(os.path.join(self.cases, "bad.jsonl"))

    def test_duplicate_ids_are_refused(self):
        self.write_cases([CASES[0]], "again.jsonl")
        code, out = self.run_cli("--validate")
        self.assertEqual(code, 2)
        self.assertIn("duplicate id", out)

    def test_judging_each_role(self):
        self.assertIsNone(ev.judge("triage", {"level": [1], "questions": "none"}, {"level": 1, "questions": []}))
        self.assertIn("asked a question", ev.judge("triage", {"questions": "none"}, {"level": 1, "questions": ["?"]}))
        self.assertIn("asked nothing", ev.judge("triage", {"questions": "some"}, {"level": 1, "questions": []}))
        self.assertIn("not acceptable", ev.judge("reviewer", {"verdict_not": ["approve"]}, {"verdict": "approve"}))
        self.assertIsNone(ev.judge("duplicate", {"duplicate_of": None}, {"duplicate_of": None}))
        self.assertIsNone(ev.judge("device", {"next_action": ["restart", None]}, {"next_action": None, "params": {}}))


if __name__ == "__main__":
    unittest.main()
