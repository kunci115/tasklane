#!/usr/bin/env python3
"""Measure the model roles of tasklane against fixed cases.

Why: a prompt, a model or an engine setting can make triage, review, duplicate detection or the
device troubleshooter worse without any test failing. This runs each case in `cases/*.jsonl`
through an engine plug-in (the same JSON-lines protocol the daemon uses), checks the answer against
what the case expects, and compares the pass rates with a saved baseline. Standard library only.

  evals/run_evals.py --validate                      check the case files (no model, used in CI)
  evals/run_evals.py                                 run every case once with engine-claude
  evals/run_evals.py --role reviewer --repeat 3      the model is not deterministic: run each case 3 times
  evals/run_evals.py --write-baseline                save today's pass rates as the baseline
  evals/run_evals.py --engine "python3 plugins/engine-cli/engine_cli.py"

Exit code: 0 = no regression against the baseline, 1 = a case got worse or an engine call failed,
2 = bad usage or bad case files. Cases that pass less often than the baseline allows are listed first.
"""
import argparse
import json
import os
import shlex
import subprocess
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
DEFAULT_ENGINE = "python3 " + os.path.join(ROOT, "plugins", "engine-claude", "engine_claude.py")
ROLES = ("triage", "reviewer", "duplicate", "device")
# what each role's `expect` may contain
EXPECT_KEYS = {
    "triage": {"level", "questions"},
    "reviewer": {"verdict", "verdict_not"},
    "duplicate": {"duplicate_of"},
    "device": {"status", "next_action", "params_lack"},
}
NEEDS = {"reviewer": ("diff",), "duplicate": ("candidates",), "device": ("observations", "actions")}


class CaseError(Exception):
    pass


def load_cases(directory, role=None):
    cases, seen = [], set()
    if not os.path.isdir(directory):
        raise CaseError("no such directory: %s" % directory)
    for name in sorted(os.listdir(directory)):
        if not name.endswith(".jsonl"):
            continue
        with open(os.path.join(directory, name), encoding="utf-8") as f:
            for n, line in enumerate(f, 1):
                if not line.strip():
                    continue
                where = "%s:%d" % (name, n)
                try:
                    c = json.loads(line)
                except ValueError as e:
                    raise CaseError("%s: not JSON (%s)" % (where, e))
                check_case(c, where)
                if c["id"] in seen:
                    raise CaseError("%s: duplicate id %r" % (where, c["id"]))
                seen.add(c["id"])
                if role is None or c["role"] == role:
                    cases.append(c)
    return cases


def check_case(c, where):
    if not isinstance(c, dict):
        raise CaseError("%s: a case is a JSON object" % where)
    if not isinstance(c.get("id"), str) or not c["id"].strip():
        raise CaseError("%s: id must be a non-empty string" % where)
    role = c.get("role")
    if role not in ROLES:
        raise CaseError("%s: role must be one of %s" % (where, ", ".join(ROLES)))
    if not isinstance(c.get("ticket"), dict):
        raise CaseError("%s: ticket must be an object" % where)
    for k in NEEDS.get(role, ()):
        if k not in c:
            raise CaseError("%s: a %s case needs %r" % (where, role, k))
    exp = c.get("expect")
    if not isinstance(exp, dict) or not exp:
        raise CaseError("%s: expect must be a non-empty object" % where)
    bad = set(exp) - EXPECT_KEYS[role]
    if bad:
        raise CaseError("%s: %s cases do not know expect keys %s" % (where, role, sorted(bad)))
    if not isinstance(c.get("why"), str) or not c["why"].strip():
        raise CaseError("%s: say why in `why`: a case nobody can explain gets deleted when it fails" % where)


def judge(role, expect, out):
    """None when the answer is acceptable, otherwise a short reason."""
    if role == "triage":
        if "level" in expect and out.get("level") not in expect["level"]:
            return "level %r, wanted one of %r" % (out.get("level"), expect["level"])
        asked = bool(out.get("questions"))
        if expect.get("questions") == "none" and asked:
            return "asked a question it did not need to ask"
        if expect.get("questions") == "some" and not asked:
            return "asked nothing about a ticket that cannot be acted on"
    elif role == "reviewer":
        v = out.get("verdict")
        if "verdict" in expect and v not in expect["verdict"]:
            return "verdict %r, wanted one of %r" % (v, expect["verdict"])
        if v in expect.get("verdict_not", []):
            return "verdict %r is not acceptable here" % v
    elif role == "duplicate":
        if out.get("duplicate_of") != expect["duplicate_of"]:
            return "duplicate_of %r, wanted %r" % (out.get("duplicate_of"), expect["duplicate_of"])
    elif role == "device":
        if "status" in expect and out.get("status") not in expect["status"]:
            return "status %r, wanted one of %r" % (out.get("status"), expect["status"])
        if "next_action" in expect and out.get("next_action") not in expect["next_action"]:
            return "next_action %r, wanted one of %r" % (out.get("next_action"), expect["next_action"])
        params = out.get("params") or {}
        for k in expect.get("params_lack", []):
            if k in params:
                return "params carry %r, which only the system may supply" % k
    return None


def params_for(c, max_secs):
    p = {"role": c["role"], "read_only": True, "profile": {}, "ticket": c["ticket"],
         "budget": {"max_secs": max_secs}}
    for k in ("diff", "candidates", "incident", "device", "observations", "actions"):
        if k in c:
            p[k] = c[k]
    return p


class Engine:
    """One engine process, spoken to over JSON lines."""

    def __init__(self, command):
        self.proc = subprocess.Popen(shlex.split(command), stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                     stderr=subprocess.DEVNULL, text=True, bufsize=1)
        self.n = 0

    def run(self, params):
        self.n += 1
        self.proc.stdin.write(json.dumps({"id": self.n, "op": "run", "params": params}) + "\n")
        self.proc.stdin.flush()
        line = self.proc.stdout.readline()
        if not line:
            raise RuntimeError("the engine stopped answering")
        return json.loads(line)

    def close(self):
        try:
            self.proc.stdin.close()
            self.proc.wait(timeout=10)
        except Exception:
            self.proc.kill()


def run_cases(engine, cases, repeat, max_secs, log):
    results = {}
    for c in cases:
        runs = []
        for _ in range(repeat):
            t0 = time.monotonic()
            try:
                resp = engine.run(params_for(c, max_secs))
            except Exception as e:  # the engine died or spoke nonsense
                runs.append({"status": "error", "why": str(e)})
                continue
            secs = time.monotonic() - t0
            if not resp.get("ok"):
                err = resp.get("error") or {}
                runs.append({"status": "error", "why": "%s: %s" % (err.get("kind"), err.get("message"))})
                continue
            out = resp.get("result") or {}
            usage = out.get("usage") or {}
            why = judge(c["role"], c["expect"], out)
            runs.append({"status": "pass" if why is None else "fail", "why": why or "",
                         "tokens": int(usage.get("tokens_in", 0) or 0) + int(usage.get("tokens_out", 0) or 0),
                         "secs": round(secs, 1)})
        results[c["id"]] = {"role": c["role"], "runs": runs}
        log("%-34s %s" % (c["id"], " ".join({"pass": "ok", "fail": "FAIL", "error": "ERR"}[r["status"]] for r in runs)))
    return results


def rate(runs):
    return sum(1 for r in runs if r["status"] == "pass") / len(runs) if runs else 0.0


def summarise(results):
    per_role = {}
    for cid, r in results.items():
        a = per_role.setdefault(r["role"], {"passes": 0, "runs": 0, "tokens": 0})
        a["passes"] += sum(1 for x in r["runs"] if x["status"] == "pass")
        a["runs"] += len(r["runs"])
        a["tokens"] += sum(x.get("tokens", 0) for x in r["runs"])
    return per_role


def compare(results, baseline, tolerance):
    """Cases that pass less often than the baseline says (minus the tolerance)."""
    worse = []
    for cid, r in results.items():
        before = (baseline.get("cases") or {}).get(cid)
        if before is None:
            continue
        now = rate(r["runs"])
        if now < before - tolerance - 1e-9:
            worse.append((cid, before, now))
    return worse


def main(argv=None):
    ap = argparse.ArgumentParser(description="Measure tasklane's model roles against fixed cases.")
    ap.add_argument("--cases", default=os.path.join(HERE, "cases"))
    ap.add_argument("--engine", default=os.environ.get("TASKLANE_EVAL_ENGINE", DEFAULT_ENGINE),
                    help="command of an engine plug-in (default: engine-claude)")
    ap.add_argument("--role", choices=ROLES)
    ap.add_argument("--repeat", type=int, default=1, help="runs per case; the model varies, so 3 gives a rate")
    ap.add_argument("--max-secs", type=int, default=300)
    ap.add_argument("--baseline", default=os.path.join(HERE, "baseline.json"))
    ap.add_argument("--tolerance", type=float, default=0.0,
                    help="how far a case's pass rate may fall below the baseline (0..1)")
    ap.add_argument("--write-baseline", action="store_true")
    ap.add_argument("--validate", action="store_true", help="only check the case files")
    ap.add_argument("--json", dest="json_out", help="write the full result here")
    a = ap.parse_args(argv)
    if a.repeat < 1 or not 0 <= a.tolerance <= 1:
        print("--repeat must be at least 1 and --tolerance between 0 and 1", file=sys.stderr)
        return 2
    try:
        cases = load_cases(a.cases, a.role)
    except CaseError as e:
        print("case files: %s" % e, file=sys.stderr)
        return 2
    if a.validate:
        print("%d cases are well formed" % len(cases))
        return 0
    if not cases:
        print("no cases to run", file=sys.stderr)
        return 2

    engine = Engine(a.engine)
    try:
        results = run_cases(engine, cases, a.repeat, a.max_secs, print)
    finally:
        engine.close()
    summary = summarise(results)
    print()
    for role, s in sorted(summary.items()):
        print("%-10s %d/%d passed (%.0f%%)  %d tokens" % (role, s["passes"], s["runs"], 100.0 * s["passes"] / s["runs"], s["tokens"]))
    total = sum(s["runs"] for s in summary.values())
    passed = sum(s["passes"] for s in summary.values())
    print("%-10s %d/%d passed (%.0f%%)" % ("all", passed, total, 100.0 * passed / total))
    failed = [(cid, r) for cid, r in results.items() for x in r["runs"] if x["status"] != "pass"]
    shown = set()
    for cid, r in failed:
        if cid in shown:
            continue
        shown.add(cid)
        why = next(x["why"] for x in r["runs"] if x["status"] != "pass")
        print("  - %s: %s" % (cid, why))

    report = {"cases": {cid: rate(r["runs"]) for cid, r in results.items()}, "repeat": a.repeat, "summary": summary}
    if a.json_out:
        with open(a.json_out, "w", encoding="utf-8") as f:
            json.dump({**report, "results": results}, f, indent=2)
    if a.write_baseline:
        # a partial run (--role) must not erase what the other roles measured
        merged = {"cases": {}, "repeat": a.repeat}
        if os.path.exists(a.baseline):
            with open(a.baseline, encoding="utf-8") as f:
                merged["cases"].update(json.load(f).get("cases", {}))
        merged["cases"].update(report["cases"])
        with open(a.baseline, "w", encoding="utf-8") as f:
            json.dump(merged, f, indent=2, sort_keys=True)
            f.write("\n")
        print("\nbaseline written to %s" % a.baseline)
        return 0
    errors = sum(1 for r in results.values() for x in r["runs"] if x["status"] == "error")
    if not os.path.exists(a.baseline):
        print("\nno baseline yet (run with --write-baseline once the numbers look right)")
        return 1 if errors else 0
    with open(a.baseline, encoding="utf-8") as f:
        base = json.load(f)
    worse = compare(results, base, a.tolerance)
    new = [cid for cid in results if cid not in base.get("cases", {})]
    if new:
        print("\nnot in the baseline yet: %s" % ", ".join(new))
    if worse:
        print("\nREGRESSION against the baseline:")
        for cid, before, now in worse:
            print("  %-34s %.0f%% -> %.0f%%" % (cid, 100 * before, 100 * now))
    if errors:
        print("\n%d engine calls failed (counted as not passing)" % errors)
    return 1 if worse or errors else 0


if __name__ == "__main__":
    sys.exit(main())
