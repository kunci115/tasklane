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


FAKE_MCP = r'''
const readline = require("readline");
const rl = readline.createInterface({ input: process.stdin });
const fs = require("fs");
rl.on("line", (line) => {
  const m = JSON.parse(line);
  fs.appendFileSync(process.env.FAKE_LOG, line + "\n");
  if (m.id === undefined) return;
  if (m.method === "initialize") return console.log(JSON.stringify({jsonrpc:"2.0", id:m.id, result:{protocolVersion:"2024-11-05", capabilities:{tools:{}}, serverInfo:{name:"fake"}}}));
  if (m.method === "tools/list") return console.log(JSON.stringify({jsonrpc:"2.0", id:m.id, result:{tools:[{name:"browser_navigate"},{name:"browser_click"}]}}));
  if (m.method === "tools/call") {
    const big = (m.params.arguments || {}).big ? "x".repeat(200000) : "ok " + m.params.name;
    return console.log(JSON.stringify({jsonrpc:"2.0", id:m.id, result:{content:[{type:"text", text:big}]}}));
  }
  console.log(JSON.stringify({jsonrpc:"2.0", id:m.id, result:{}}));
});
'''

BROWSER = os.path.join(HERE, "tl-browser-mcp")


@unittest.skipUnless(subprocess.run(["which", "node"], capture_output=True).returncode == 0, "node is needed")
class BrowserWrapper(unittest.TestCase):
    def setUp(self):
        self.dir = tempfile.mkdtemp()
        self.log = os.path.join(self.dir, "fake.log")
        self.fake = os.path.join(self.dir, "obscura")
        with open(self.fake, "w") as f:
            f.write("#!/usr/bin/env node\nif (process.argv[2] !== 'mcp') process.exit(3);\n" + FAKE_MCP)
        os.chmod(self.fake, 0o755)
        self.env = dict(os.environ, TASKLANE_OBSCURA_BIN=self.fake, TL_BROWSER_PORT="3000", FAKE_LOG=self.log)

    def start(self, **env):
        import json
        p = subprocess.Popen([BROWSER], stdin=subprocess.PIPE, stdout=subprocess.PIPE, env=dict(self.env, **env), text=True)

        def rpc(msg):
            p.stdin.write(json.dumps(msg) + "\n")
            p.stdin.flush()
            return json.loads(p.stdout.readline())
        return p, rpc

    def forwarded(self):
        return open(self.log).read() if os.path.exists(self.log) else ""

    def call(self, rpc, i, name, args):
        return rpc({"jsonrpc": "2.0", "id": i, "method": "tools/call", "params": {"name": name, "arguments": args}})

    def test_the_agent_can_use_the_browser_on_the_local_app_only(self):
        p, rpc = self.start()
        self.assertEqual(rpc({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}})["result"]["serverInfo"]["name"], "fake")
        self.assertEqual([t["name"] for t in rpc({"jsonrpc": "2.0", "id": 2, "method": "tools/list"})["result"]["tools"]], ["browser_navigate", "browser_click"])
        for i, url in enumerate(["http://127.0.0.1:3000/", "http://localhost:3000/a/b?x=1", "http://127.0.0.1:3000", "http://[::1]:3000/x"]):
            r = self.call(rpc, 10 + i, "browser_navigate", {"url": url})
            self.assertFalse(r["result"].get("isError"), url)
        sent = self.forwarded().count('"tools/call"')
        for i, url in enumerate(["http://127.0.0.1:3001/", "http://127.0.0.1/", "https://127.0.0.1:3000/", "http://evil.example/",
                                 "http://127.0.0.1:3000@evil.example/", "http://evil@127.0.0.1:3000/", "http://127.0.0.1:3000.evil.example/",
                                 "file:///etc/passwd", "javascript:alert(1)", "//evil.example/x", "/relative", "http://127.0.0.1:3000/\nx", ""]):
            r = self.call(rpc, 100 + i, "browser_navigate", {"url": url})
            self.assertTrue(r["result"]["isError"], repr(url))
            self.assertIn("refused", r["result"]["content"][0]["text"])
        self.assertEqual(self.forwarded().count('"tools/call"'), sent, "nothing refused was passed to the browser")
        # an address hidden inside another argument is found too
        r = self.call(rpc, 300, "browser_open", {"options": {"href": "http://evil.example/"}})
        self.assertTrue(r["result"]["isError"])
        # text typed into a page and scripts are not addresses
        r = self.call(rpc, 301, "browser_type", {"text": "visit http://evil.example", "selector": "#q"})
        self.assertFalse(r["result"].get("isError"))
        p.stdin.close()
        p.wait(timeout=10)

    def test_only_the_calls_a_browser_needs_are_passed_on(self):
        p, rpc = self.start()
        r = rpc({"jsonrpc": "2.0", "id": 1, "method": "sampling/createMessage", "params": {}})
        self.assertEqual(r["error"]["code"], -32601)
        self.assertNotIn("sampling", self.forwarded())
        p.stdin.close()
        p.wait(timeout=10)

    def test_the_number_of_calls_and_the_size_of_answers_are_capped(self):
        p, rpc = self.start(TL_BROWSER_MAX_CALLS="3", TL_BROWSER_MAX_REPLY="1000")
        r = self.call(rpc, 1, "browser_evaluate", {"big": True})
        text = r["result"]["content"][0]["text"]
        self.assertTrue(text.endswith("[cut here]") and len(text) < 1100)
        self.call(rpc, 2, "browser_click", {})
        self.call(rpc, 3, "browser_click", {})
        r = self.call(rpc, 4, "browser_click", {})
        self.assertTrue(r["result"]["isError"])
        self.assertIn("limit", r["result"]["content"][0]["text"])
        p.stdin.close()
        p.wait(timeout=10)

    def test_it_will_not_start_without_the_preview_port(self):
        env = dict(self.env)
        env.pop("TL_BROWSER_PORT")
        r = subprocess.run([BROWSER], env=env, capture_output=True, text=True, timeout=10)
        self.assertEqual(r.returncode, 2)
