import http.server
import json
import os
import subprocess
import shutil
import sys
import threading
import unittest
from pathlib import Path

HERE = Path(__file__).parent
sys.path.insert(0, str(HERE))
import tool_broker as tb  # noqa: E402

SHIM = HERE / "tl_mcp_shim.js"


class Docs(http.server.BaseHTTPRequestHandler):
    seen = []

    def log_message(self, *a):
        pass

    def do_GET(self):
        Docs.seen.append({"path": self.path, "auth": self.headers.get("Authorization")})
        if self.path.startswith("/api/redirect"):
            self.send_response(302)
            self.send_header("Location", "/api/x")
            self.end_headers()
            return
        if self.path.startswith("/api/bin"):
            body, ctype = b"\x00\x01", "application/octet-stream"
        elif self.path.startswith("/api/big"):
            body, ctype = b"y" * 300_000, "text/plain"
        elif self.path.startswith("/api/echo"):
            body, ctype = ("key=" + (self.headers.get("Authorization") or "")).encode(), "text/plain"
        else:
            body, ctype = json.dumps({"path": self.path, "tables": ["orders", "users"]}).encode(), "application/json"
        self.send_response(200)
        self.send_header("Content-Type", ctype)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)


class Base(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.srv = http.server.HTTPServer(("127.0.0.1", 0), Docs)
        cls.base = "http://127.0.0.1:%d/api" % cls.srv.server_port
        threading.Thread(target=cls.srv.serve_forever, daemon=True).start()

    @classmethod
    def tearDownClass(cls):
        cls.srv.shutdown()

    def grants(self, **kw):
        g = {"id": 7, "name": "schema", "description": "Database schema", "base_url": self.base,
             "header_name": "Authorization", "header_value": "Bearer s3cret-token", "max_calls": 3}
        g.update(kw)
        return [g]

    def call(self, broker, tool, path, token=None):
        import socket
        s = socket.socket(socket.AF_UNIX)
        s.connect(broker.path)
        s.sendall((json.dumps({"token": token or broker.token, "op": "call", "tool": tool, "args": {"path": path}}) + "\n").encode())
        data = s.makefile().readline()
        s.close()
        return json.loads(data)


class Rules(Base):
    def test_a_path_below_the_base_is_read_with_the_credential_and_nothing_else_is_reachable(self):
        with tb.Broker(self.grants()) as b:
            r = self.call(b, "schema", "/tables?x=1")
            self.assertTrue(r["ok"], r)
            self.assertIn("orders", r["text"])
            self.assertIn("not instructions", r["text"])
            self.assertEqual(Docs.seen[-1]["path"], "/api/tables?x=1")
            self.assertEqual(Docs.seen[-1]["auth"], "Bearer s3cret-token")
            before = len(Docs.seen)
            for bad in ["http://evil.example/x", "//evil.example/x", "/a/../../etc/passwd", "/a/%2e%2e/b", "/a/%252e%252e/b",
                        "tables", "", "/a\\b", "/a b", "/" + "x" * 600]:
                r = self.call(b, "schema", bad)
                self.assertFalse(r["ok"], bad)
            self.assertEqual(len(Docs.seen), before, "nothing was fetched for a bad path")

    def test_the_wrong_token_an_unknown_tool_and_a_second_run_get_nothing(self):
        with tb.Broker(self.grants()) as b:
            self.assertFalse(self.call(b, "schema", "/x", token="nope")["ok"])
            self.assertFalse(self.call(b, "other", "/x")["ok"])

    def test_the_call_limit_and_the_byte_limit_hold(self):
        with tb.Broker(self.grants(max_calls=2)) as b:
            self.assertTrue(self.call(b, "schema", "/a")["ok"])
            self.assertTrue(self.call(b, "schema", "/b")["ok"])
            r = self.call(b, "schema", "/c")
            self.assertFalse(r["ok"])
            self.assertIn("limit", r["error"])
            self.assertEqual(b.usage(), {"7": 2})
        with tb.Broker(self.grants(max_calls=50)) as b:
            ok = [self.call(b, "schema", "/big")["ok"] for _ in range(6)]
            self.assertEqual(ok[:4], [True] * 4)
            self.assertFalse(all(ok), "the data for a run is capped")

    def test_redirects_and_non_text_are_refused_and_the_credential_is_never_returned(self):
        with tb.Broker(self.grants()) as b:
            self.assertFalse(self.call(b, "schema", "/redirect")["ok"])
            self.assertFalse(self.call(b, "schema", "/bin")["ok"])
            r = self.call(b, "schema", "/echo")
            self.assertIn("[redacted]", r["text"])
            self.assertNotIn("s3cret-token", json.dumps(r))

    def test_a_grant_with_credentials_in_the_address_or_a_bad_name_is_dropped(self):
        for bad in ({"base_url": "http://user:pw@host/x"}, {"name": "Bad Name"}, {"base_url": "ftp://host/x"}, {"name": "1abc"}):
            with tb.Broker(self.grants(**bad)) as b:
                self.assertEqual(b.tool_list(), [], bad)

    def test_the_directory_and_socket_are_private_to_the_user_unless_a_sandbox_user_needs_them(self):
        with tb.Broker(self.grants()) as b:
            self.assertEqual(os.stat(b.dir).st_mode & 0o777, 0o700)
            self.assertEqual(os.stat(b.path).st_mode & 0o777, 0o600)
            self.assertEqual(os.stat(b.write_config({"a": 1})).st_mode & 0o777, 0o600)
        with tb.Broker(self.grants(), private=False) as b:
            self.assertEqual(os.stat(b.dir).st_mode & 0o777, 0o711, "enter but not list")
            self.assertEqual(os.stat(b.path).st_mode & 0o777, 0o666)

    def test_the_socket_directory_is_removed_when_the_run_ends(self):
        with tb.Broker(self.grants()) as b:
            d = b.dir
            self.assertTrue(os.path.exists(b.path))
        self.assertFalse(os.path.exists(d))


@unittest.skipUnless(shutil.which("node"), "node is needed for the MCP shim")
class Shim(Base):
    """The MCP server the agent talks to, in front of the broker."""

    def test_the_shim_speaks_mcp_and_forwards_calls(self):
        with tb.Broker(self.grants()) as b:
            env = dict(os.environ, TL_SOCK=b.path, TL_TOKEN=b.token)
            p = subprocess.Popen(["node", str(SHIM)], stdin=subprocess.PIPE, stdout=subprocess.PIPE, env=env, text=True)
            def rpc(msg):
                p.stdin.write(json.dumps(msg) + "\n")
                p.stdin.flush()
                return json.loads(p.stdout.readline())
            init = rpc({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2025-03-26"}})
            self.assertEqual(init["result"]["protocolVersion"], "2025-03-26")
            self.assertIn("tools", init["result"]["capabilities"])
            p.stdin.write(json.dumps({"jsonrpc": "2.0", "method": "notifications/initialized"}) + "\n")
            tools = rpc({"jsonrpc": "2.0", "id": 2, "method": "tools/list"})["result"]["tools"]
            self.assertEqual([t["name"] for t in tools], ["schema"])
            self.assertEqual(tools[0]["inputSchema"]["required"], ["path"])
            out = rpc({"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {"name": "schema", "arguments": {"path": "/tables"}}})
            self.assertFalse(out["result"]["isError"])
            self.assertIn("orders", out["result"]["content"][0]["text"])
            bad = rpc({"jsonrpc": "2.0", "id": 4, "method": "tools/call", "params": {"name": "schema", "arguments": {"path": "/../x"}}})
            self.assertTrue(bad["result"]["isError"])
            self.assertEqual(rpc({"jsonrpc": "2.0", "id": 5, "method": "nope"})["error"]["code"], -32601)
            p.stdin.close()
            p.wait(timeout=10)

    def test_a_shim_with_the_wrong_token_gets_no_tools(self):
        with tb.Broker(self.grants()) as b:
            env = dict(os.environ, TL_SOCK=b.path, TL_TOKEN="wrong")
            p = subprocess.Popen(["node", str(SHIM)], stdin=subprocess.PIPE, stdout=subprocess.PIPE, env=env, text=True)
            p.stdin.write(json.dumps({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}) + "\n")
            p.stdin.flush()
            self.assertEqual(json.loads(p.stdout.readline())["result"]["tools"], [])
            p.stdin.close()
            p.wait(timeout=10)


if __name__ == "__main__":
    unittest.main()
