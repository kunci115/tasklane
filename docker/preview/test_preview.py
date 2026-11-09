"""Tests for the preview helpers with a stub in place of Obscura (the real binary is not run here)."""
import os
import subprocess
import tempfile
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
SHOT = os.path.join(HERE, "tl-shot")
AGENT = os.path.join(HERE, "tl-agent")


class Shot(unittest.TestCase):
    def setUp(self):
        self.dir = tempfile.mkdtemp()
        self.log = os.path.join(self.dir, "calls.log")
        stub = os.path.join(self.dir, "obscura")
        with open(stub, "w") as f:
            f.write('#!/bin/sh\necho "$OBSCURA_ALLOW_PRIVATE_NETWORK $*" >> "%s"\n' % self.log)
        os.chmod(stub, 0o755)
        self.env = dict(os.environ, TASKLANE_OBSCURA_BIN=stub)

    def run_shot(self, *args):
        return subprocess.run(["sh", SHOT, *args], capture_output=True, text=True, env=self.env)

    def calls(self):
        return open(self.log).read() if os.path.exists(self.log) else ""

    def test_a_loopback_page_is_photographed_with_obscura(self):
        r = self.run_shot("http://127.0.0.1:3000/dashboard?tab=1", "home")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(r.stdout.strip(), "/tmp/shots/home.png")
        self.assertEqual(self.calls().strip(),
                         "1 fetch http://127.0.0.1:3000/dashboard?tab=1 --screenshot /tmp/shots/home.png")
        for ok in ("http://localhost:8080/", "http://localhost/x", "http://[::1]:9000/"):
            self.assertEqual(self.run_shot(ok).returncode, 0, ok)

    def test_only_this_machine_is_reachable_through_it(self):
        for bad in ("https://example.com/", "http://example.com/", "http://10.0.0.5:80/", "http://127.0.0.1.evil.com/",
                    "http://127.0.0.1:80@evil.com/", "http://localhost.evil.com/", "file:///etc/passwd",
                    "http://169.254.169.254/latest/meta-data/", "http://127.0.0.1:3000/; rm -rf /",
                    "http://127.0.0.1:3000/$(id)", "http://127.0.0.1:3000/ x", "", "ftp://127.0.0.1/",
                    "http://169.254.169.254/latest/\nhttp://127.0.0.1:3000/", "http://127.0.0.1:3000/\r", "http://127.0.0.1:3000/\tx"):
            r = self.run_shot(bad)
            self.assertEqual(r.returncode, 2, bad)
        self.assertEqual(self.calls(), "", "Obscura was never called for a refused address")

    def test_the_name_cannot_escape_the_screenshot_folder(self):
        for bad in ("../x", "a/b", "a b", "x;y"):
            self.assertEqual(self.run_shot("http://127.0.0.1:3000/", bad).returncode, 2, repr(bad))
        self.assertEqual(self.calls(), "")


class Agent(unittest.TestCase):
    def test_the_agent_command_runs_after_the_preview_server_is_up(self):
        d = tempfile.mkdtemp()
        marker = os.path.join(d, "order.log")
        env = dict(os.environ, TASKLANE_PREVIEW_CMD="echo server-started >> %s" % marker, TASKLANE_PREVIEW_PORT="1")
        # nothing listens on port 1, so the wait would run its full minute; give the script a port that works
        import socket
        srv = socket.socket()
        srv.bind(("127.0.0.1", 0))
        srv.listen(1)
        env["TASKLANE_PREVIEW_PORT"] = str(srv.getsockname()[1])
        r = subprocess.run(["bash", AGENT, "sh", "-c", "echo agent-ran >> %s" % marker], capture_output=True, text=True, env=env, timeout=30)
        srv.close()
        self.assertEqual(r.returncode, 0, r.stderr)
        lines = open(marker).read().split()
        self.assertEqual(lines[-1], "agent-ran")
        self.assertIn("server-started", lines)

    def test_without_a_preview_command_it_just_runs_the_agent(self):
        env = {k: v for k, v in os.environ.items() if not k.startswith("TASKLANE_PREVIEW")}
        r = subprocess.run(["bash", AGENT, "echo", "hello"], capture_output=True, text=True, env=env, timeout=10)
        self.assertEqual((r.returncode, r.stdout.strip()), (0, "hello"))


if __name__ == "__main__":
    unittest.main()
