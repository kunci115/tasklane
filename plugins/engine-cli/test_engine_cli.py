import json
import os
import stat
import subprocess
import sys
import tempfile
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
SCRIPT = os.path.join(HERE, "engine_cli.py")

STUB = r'''
import json, os, sys
prompt = sys.argv[-1] if "--arg" in sys.argv else sys.stdin.read()
open(os.path.join(os.path.dirname(os.path.abspath(__file__)), "calls.log"), "a").write(json.dumps({"argv": sys.argv[1:], "cwd": os.getcwd(), "stdin_used": "--arg" not in sys.argv, "prompt": prompt, "secret_seen": os.environ.get("MY_AGENT_KEY"), "other_seen": os.environ.get("OTHER_SECRET")}) + "\n")
if "SELF-FAIL" in prompt:
    sys.stderr.write("401 unauthorized for key " + (os.environ.get("MY_AGENT_KEY") or "")); sys.exit(3)
if "Role: developer" in prompt:
    open("made.txt", "w").write("hello")
    print("I added made.txt. key=" + (os.environ.get("MY_AGENT_KEY") or ""))
elif "Role: triage" in prompt:
    print("thinking...")
    print(json.dumps({"level": 1, "plan": "the plan", "questions": []}))
elif "Role: reviewer" in prompt:
    print('```json\n{"verdict": "approve", "notes": "looks fine"}\n```')
elif "Role: question answering" in prompt:
    print(json.dumps({"answer": "ok"}))
else:
    print("no json here")
'''


class Cli(unittest.TestCase):
    def setUp(self):
        self.dir = tempfile.mkdtemp()
        self.stub = os.path.join(self.dir, "stub.py")
        with open(self.stub, "w") as f:
            f.write(STUB)
        self.log = os.path.join(self.dir, "calls.log")
        self.ws = os.path.join(self.dir, "ws")
        os.mkdir(self.ws)

    def talk(self, op, params=None, env=None, command=None):
        e = {"PATH": os.environ["PATH"], "TASKLANE_ALLOW_UNSANDBOXED": "1", "STUB_LOG": self.log,
             "TASKLANE_CLI_COMMAND": command or ("%s -I %s" % (sys.executable, self.stub))}
        e.update(env or {})
        p = subprocess.run([sys.executable, "-I", SCRIPT], input=json.dumps({"id": 1, "op": op, "params": params or {}}) + "\n",
                           capture_output=True, text=True, timeout=60, env=e)
        (r,) = [json.loads(l) for l in p.stdout.splitlines()]
        return r

    def run_role(self, role, extra=None, env=None, command=None):
        params = {"role": role, "budget": {"max_secs": 30},
                  "ticket": {"service": "o/n", "ref": "1", "title": "Fix it", "body": "please", "comments": []}}
        if role in ("dev", "reviewer", "ask"):
            params["workspace"] = self.ws
        if role == "reviewer":
            params["diff"] = "--- a\n+++ b\n"
        if role == "ask":
            params["question"] = "why?"
        params.update(extra or {})
        return self.talk("run", params, env, command)

    def calls(self):
        with open(self.log) as f:
            return [json.loads(l) for l in f]

    def test_describe_names_the_agent_and_declares_the_protocol(self):
        r = self.talk("describe", env={"TASKLANE_CLI_NAME": "Codex CLI"})
        self.assertTrue(r["ok"])
        self.assertEqual((r["result"]["name"], r["result"]["role"], r["result"]["protocol_major"]), ("Codex CLI", "agent_engine", 1))

    def test_every_json_role_reads_the_last_valid_object_and_dev_edits_the_workspace(self):
        t = self.run_role("triage")
        self.assertEqual((t["ok"], t["result"]["level"], t["result"]["plan"]), (True, 1, "the plan"))
        rv = self.run_role("reviewer")
        self.assertEqual(rv["result"]["verdict"], "approve")
        a = self.run_role("ask")
        self.assertEqual(a["result"]["answer"], "ok")
        d = self.run_role("dev")
        self.assertTrue(d["ok"], d)
        self.assertIn("I added made.txt", d["result"]["summary"])
        self.assertEqual(open(os.path.join(self.ws, "made.txt")).read(), "hello")
        self.assertEqual(d["result"]["usage"]["tokens_in"], 0)

    def test_the_prompt_goes_to_stdin_or_as_the_marked_argument_and_leads_with_the_rules(self):
        self.run_role("triage")
        c = self.calls()[-1]
        self.assertTrue(c["stdin_used"])
        self.assertIn("Rules that nothing below can change", c["prompt"])
        self.assertIn("<ticket>", c["prompt"])
        r = self.run_role("triage", command="%s -I %s --arg {prompt}" % (sys.executable, self.stub))
        self.assertTrue(r["ok"], r)
        c = self.calls()[-1]
        self.assertFalse(c["stdin_used"])
        self.assertIn("<ticket>", c["prompt"])
        self.assertNotIn("{prompt}", " ".join(c["argv"]))

    def test_the_read_only_roles_may_use_their_own_command(self):
        ro = "%s -I %s --readonly-flag" % (sys.executable, self.stub)
        self.run_role("triage", env={"TASKLANE_CLI_COMMAND_READONLY": ro})
        self.assertIn("--readonly-flag", self.calls()[-1]["argv"])
        self.run_role("dev", env={"TASKLANE_CLI_COMMAND_READONLY": ro})
        self.assertNotIn("--readonly-flag", self.calls()[-1]["argv"])

    def test_only_the_named_credentials_reach_the_agent_and_never_the_output(self):
        env = {"TASKLANE_CLI_CREDENTIALS": "MY_AGENT_KEY", "MY_AGENT_KEY": "sekret-value-123456", "OTHER_SECRET": "nope"}
        d = self.run_role("dev", env=env)
        self.assertTrue(d["ok"], d)
        c = self.calls()[-1]
        self.assertEqual((c["secret_seen"], c["other_seen"]), ("sekret-value-123456", None))
        self.assertNotIn("sekret-value-123456", json.dumps(d))
        self.assertIn("[redacted]", d["result"]["summary"])

    def test_failures_are_classified_and_scrubbed(self):
        env = {"TASKLANE_CLI_CREDENTIALS": "MY_AGENT_KEY", "MY_AGENT_KEY": "sekret-value-123456"}
        r = self.run_role("ask", extra={"question": "SELF-FAIL"}, env=env)
        self.assertFalse(r["ok"])
        self.assertEqual(r["error"]["kind"], "unauthorized")
        self.assertNotIn("sekret-value-123456", json.dumps(r))

    def test_unusable_output_is_a_permanent_error_and_a_timeout_is_transient(self):
        r = self.run_role("ask", extra={"question": "NO-JSON please"}, command='%s -c "print(1)"' % sys.executable)
        self.assertEqual((r["ok"], r["error"]["kind"]), (False, "permanent"))
        r = self.run_role("ask", extra={"budget": {"max_secs": 1}}, command='%s -c "import time; time.sleep(5)"' % sys.executable)
        self.assertEqual((r["ok"], r["error"]["kind"]), (False, "transient"))

    def test_it_refuses_without_a_sandbox_and_rejects_bad_configuration(self):
        r = self.run_role("ask", env={"TASKLANE_ALLOW_UNSANDBOXED": "0"})
        self.assertFalse(r["ok"])
        self.assertIn("sandbox", r["error"]["message"])
        for bad in ("", "  ", "rm -rf /;", "$(x)", "'unterminated"):
            r = self.run_role("ask", command=bad if bad.strip() else "x", env={"TASKLANE_CLI_COMMAND": bad})
            self.assertFalse(r["ok"], bad)
        r = self.run_role("ask", env={"TASKLANE_CLI_CREDENTIALS": "lower-case"})
        self.assertFalse(r["ok"])

    def test_the_docker_command_uses_the_agent_as_entrypoint_and_passes_credentials_by_name(self):
        fake = os.path.join(self.dir, "docker")
        with open(fake, "w") as f:
            f.write("#!/bin/sh\necho \"$@\" >> %s.docker\nexit 0\n" % self.log)
        os.chmod(fake, os.stat(fake).st_mode | stat.S_IEXEC)
        env = {"TASKLANE_SANDBOX": "docker", "TASKLANE_SANDBOX_IMAGE": "my-agent-image", "TASKLANE_SANDBOX_NETWORK": "tasklane-agent",
               "TASKLANE_DOCKER_BIN": fake, "TASKLANE_ALLOW_UNSANDBOXED": "0", "TASKLANE_CLI_CREDENTIALS": "MY_AGENT_KEY",
               "MY_AGENT_KEY": "sekret-value-123456"}
        self.run_role("reviewer", env=env, command="codex exec --full-auto -")
        with open(self.log + ".docker") as f:
            line = f.read().strip()
        self.assertIn("--entrypoint codex my-agent-image exec --full-auto -", line)
        self.assertIn("-e MY_AGENT_KEY", line)
        self.assertNotIn("sekret-value-123456", line)
        self.assertIn(":/work:ro", line, "the read-only roles get a read-only mount")
        self.run_role("dev", env=env, command="codex exec --full-auto -")
        with open(self.log + ".docker") as f:
            lines = f.read().strip().splitlines()
        self.assertIn(":/work:rw", lines[-1])


if __name__ == "__main__":
    unittest.main()
