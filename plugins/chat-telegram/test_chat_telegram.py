import json
import os
import subprocess
import sys
import threading
import unittest
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

SCRIPT = Path(__file__).with_name("chat_telegram.py")
TOKEN = "123456:SECRET-TOKEN-abc"
BOT = {"id": 999, "username": "tl_bot", "is_bot": True, "first_name": "TL"}


class Fake(ThreadingHTTPServer):
    def __init__(self):
        super().__init__(("127.0.0.1", 0), Handler)
        self.updates = []
        self.calls = []        # (method, params)
        self.forced = {}       # method -> (status, body)
        self.next_id = 100

    def reset(self):
        self.updates, self.calls, self.forced = [], [], {}
        self.next_id = 100


class Handler(BaseHTTPRequestHandler):
    def log_message(self, *a):
        pass

    def do_POST(self):
        srv = self.server
        params = json.loads(self.rfile.read(int(self.headers.get("Content-Length", 0))) or b"{}")
        prefix = f"/bot{TOKEN}/"
        if not self.path.startswith(prefix):
            return self.reply(404, {"ok": False, "error_code": 404, "description": "Not Found"})
        method = self.path[len(prefix):]
        srv.calls.append((method, params))
        if method in srv.forced:
            return self.reply(*srv.forced[method])
        if method == "getMe":
            return self.reply(200, {"ok": True, "result": BOT})
        if method == "getUpdates":
            off = params.get("offset", 0)
            return self.reply(200, {"ok": True, "result": [u for u in srv.updates if u["update_id"] >= off]})
        if method == "sendMessage":
            srv.next_id += 1
            return self.reply(200, {"ok": True, "result": {"message_id": srv.next_id}})
        return self.reply(200, {"ok": True, "result": True})

    def reply(self, status, body):
        data = json.dumps(body).encode()
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)


def msg(uid, text=None, chat=-100, user=7, **extra):
    m = {"message_id": uid, "date": 1, "chat": {"id": chat, "type": "supergroup"},
         "from": {"id": user, "username": "alice", "first_name": "Alice"}}
    if text is not None:
        m["text"] = text
    m.update(extra)
    return {"update_id": uid, "message": m}


class Telegram(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.srv = Fake()
        threading.Thread(target=cls.srv.serve_forever, daemon=True).start()

    @classmethod
    def tearDownClass(cls):
        cls.srv.shutdown()
        cls.srv.server_close()

    def setUp(self):
        self.srv.reset()

    def run_plugin(self, *requests, token=TOKEN, url=None, extra_env=None, raw=None):
        env = {"PATH": os.environ.get("PATH", ""), "TELEGRAM_BOT_TOKEN": token,
               "TELEGRAM_API_URL": url or f"http://127.0.0.1:{self.srv.server_port}"}
        env.update(extra_env or {})
        if token is None:
            del env["TELEGRAM_BOT_TOKEN"]
        stdin = raw if raw is not None else "\n".join(json.dumps(r) for r in requests) + "\n"
        p = subprocess.run([sys.executable, "-I", str(SCRIPT)], input=stdin, env=env,
                           capture_output=True, text=True, timeout=20)
        self.assertNotIn("SECRET-TOKEN", p.stdout + p.stderr)
        return [json.loads(l) for l in p.stdout.splitlines()]

    def recv(self, *extra_env_pairs, n=1):
        reqs = [{"id": i + 1, "op": "receive", "params": {"timeout_secs": 1}} for i in range(n)]
        return self.run_plugin(*reqs)

    def events(self):
        (r,) = self.recv()
        self.assertTrue(r["ok"], r)
        return r["result"]["events"]

    # --- describe
    def test_describe(self):
        (r,) = self.run_plugin({"id": 1, "op": "describe"})
        self.assertEqual(r["result"]["role"], "chat_channel")
        self.assertEqual(r["result"]["protocol_major"], 1)

    def test_malformed_lines_do_not_crash(self):
        rs = self.run_plugin(raw='not json\n[1]\n{"id": 5, "op": "describe"}\n')
        self.assertEqual([r["ok"] for r in rs], [False, False, True])
        self.assertEqual(rs[2]["id"], 5)

    # --- receive
    def test_plain_message(self):
        self.srv.updates = [msg(1, "hello there")]
        (e,) = self.events()
        self.assertEqual(e, {"kind": "message", "group": "-100", "user": "7", "user_name": "alice",
                             "text": "hello there", "message_id": "1"})

    def test_ask_command(self):
        self.srv.updates = [msg(1, "/ask what is the plan?")]
        (e,) = self.events()
        self.assertEqual((e["kind"], e["command"], e["args"]), ("command", "ask", "what is the plan?"))

    def test_task_command_with_bot_name_and_args(self):
        self.srv.updates = [msg(1, "/task@tl_bot fix the login\nbug"), msg(2, "/task@other_bot x"),
                            msg(3, "/ask")]
        a, b, c = self.events()
        self.assertEqual((a["kind"], a["command"], a["args"]), ("command", "task", "fix the login\nbug"))
        self.assertEqual(b["kind"], "message")  # addressed to another bot
        self.assertEqual((c["kind"], c["command"], c["args"]), ("command", "ask", ""))

    def test_callback_query_and_answer(self):
        self.srv.updates = [{"update_id": 5, "callback_query": {
            "id": "cbq1", "from": {"id": 42, "first_name": "Bob"}, "data": "approve:12",
            "message": {"message_id": 77, "chat": {"id": -100, "type": "group"}}}}]
        (e,) = self.events()
        self.assertEqual((e["kind"], e["group"], e["user"], e["data"], e["user_name"]),
                         ("button", "-100", "42", "approve:12", "Bob"))
        self.assertIn(("answerCallbackQuery", {"callback_query_id": "cbq1"}), self.srv.calls)

    def test_joined_left_events(self):
        chat = {"id": -100, "type": "supergroup"}
        self.srv.updates = [
            msg(1, new_chat_members=[{"id": 11, "first_name": "Eve"}]),
            msg(2, left_chat_member={"id": 12, "username": "mal"}),
            {"update_id": 3, "my_chat_member": {"chat": chat, "from": {"id": 7},
                "old_chat_member": {"status": "left", "user": BOT},
                "new_chat_member": {"status": "member", "user": BOT}}},
            {"update_id": 4, "chat_member": {"chat": chat, "from": {"id": 7},
                "old_chat_member": {"status": "member", "user": {"id": 13}},
                "new_chat_member": {"status": "kicked", "user": {"id": 13, "first_name": "K"}}}},
            {"update_id": 5, "my_chat_member": {"chat": chat, "from": {"id": 7},
                "old_chat_member": {"status": "member", "user": BOT},
                "new_chat_member": {"status": "left", "user": BOT}}},
        ]
        evs = [(e["kind"], e["user"]) for e in self.events()]
        self.assertEqual(evs, [("joined", "11"), ("left", "12"), ("joined", "999"),
                               ("left", "13"), ("left", "999")])

    def test_offset_advances_and_timeout_is_empty(self):
        self.srv.updates = [msg(10, "one"), msg(11, "two")]
        r1, r2, r3 = self.recv(n=3)
        self.assertEqual([e["text"] for e in r1["result"]["events"]], ["one", "two"])
        self.assertEqual(r2["result"]["events"], [])
        self.assertEqual(r3["result"]["events"], [])
        polls = [p for m, p in self.srv.calls if m == "getUpdates"]
        self.assertNotIn("offset", polls[0])
        self.assertEqual(polls[1]["offset"], 12)
        self.assertEqual(polls[0]["timeout"], 1)

    def test_skip_backlog(self):
        self.srv.updates = [msg(1, "old")]
        (r,) = self.run_plugin({"id": 1, "op": "receive", "params": {"timeout_secs": 0}},
                               extra_env={"TELEGRAM_SKIP_BACKLOG": "1"})
        self.assertEqual(r["result"]["events"], [])

    # --- send
    def test_send_plain(self):
        (r,) = self.run_plugin({"id": 1, "op": "send", "params": {"group": "-100", "text": "*hi* <b>"}})
        self.assertEqual(r["result"], {"message_id": "101"})
        m, p = self.srv.calls[-1]
        self.assertEqual((p["chat_id"], p["text"]), (-100, "*hi* <b>"))
        self.assertNotIn("parse_mode", p)
        self.assertNotIn("reply_markup", p)

    def test_send_long_text_split(self):
        (r,) = self.run_plugin({"id": 1, "op": "send", "params": {"group": "5", "text": "a" * 5000}})
        sent = [p["text"] for m, p in self.srv.calls if m == "sendMessage"]
        self.assertEqual(len(sent), 2)
        self.assertEqual("".join(sent), "a" * 5000)
        self.assertTrue(all(len(s) <= 4096 for s in sent))
        self.assertEqual(r["result"], {"message_id": "101"})  # the first one

    def test_send_with_buttons(self):
        btns = [{"label": "Approve", "data": "ok:1"}, {"label": "Reject", "data": "no:1"}]
        self.run_plugin({"id": 1, "op": "send", "params": {"group": "-100", "text": "go?", "buttons": btns}})
        _, p = self.srv.calls[-1]
        self.assertEqual(p["reply_markup"], {"inline_keyboard": [
            [{"text": "Approve", "callback_data": "ok:1"}],
            [{"text": "Reject", "callback_data": "no:1"}]]})

    def test_oversize_callback_data_rejected(self):
        (r,) = self.run_plugin({"id": 1, "op": "send", "params": {
            "group": "-100", "text": "x", "buttons": [{"label": "L", "data": "d" * 65}]}})
        self.assertEqual((r["ok"], r["error"]["kind"]), (False, "permanent"))
        self.assertFalse([c for c in self.srv.calls if c[0] == "sendMessage"])

    def test_notifier_send_shape(self):
        (r,) = self.run_plugin({"id": 1, "op": "send", "params": {
            "recipient_ref": "42", "text": "done", "link": "https://x.test/1"}})
        self.assertEqual(r["result"], {"delivered": True})
        _, p = self.srv.calls[-1]
        self.assertEqual((p["chat_id"], p["text"]), (42, "done\nhttps://x.test/1"))

    def test_leave(self):
        (r,) = self.run_plugin({"id": 1, "op": "leave", "params": {"group": "-100"}})
        self.assertEqual(r["result"], {})
        self.assertEqual(self.srv.calls[-1], ("leaveChat", {"chat_id": -100}))

    # --- errors and secrecy
    def test_401(self):
        self.srv.forced["sendMessage"] = (401, {"ok": False, "error_code": 401, "description": "Unauthorized"})
        (r,) = self.run_plugin({"id": 1, "op": "send", "params": {"group": "1", "text": "x"}})
        self.assertEqual(r["error"]["kind"], "unauthorized")

    def test_429_retry_after(self):
        self.srv.forced["sendMessage"] = (429, {"ok": False, "error_code": 429,
            "description": "Too Many Requests", "parameters": {"retry_after": 17}})
        (r,) = self.run_plugin({"id": 1, "op": "send", "params": {"group": "1", "text": "x"}})
        self.assertEqual(r["error"]["kind"], "rate_limited")
        self.assertIn("17", r["error"]["message"])

    def test_500_and_4xx(self):
        self.srv.forced["sendMessage"] = (500, {"ok": False, "description": "boom"})
        (r,) = self.run_plugin({"id": 1, "op": "send", "params": {"group": "1", "text": "x"}})
        self.assertEqual(r["error"]["kind"], "transient")
        for code in (400, 403, 404):
            self.srv.forced["sendMessage"] = (code, {"ok": False, "error_code": code, "description": "no"})
            (r,) = self.run_plugin({"id": 1, "op": "send", "params": {"group": "1", "text": "x"}})
            self.assertEqual(r["error"]["kind"], "permanent", code)

    def test_network_error_is_transient_and_hides_token(self):
        (r,) = self.run_plugin({"id": 1, "op": "send", "params": {"group": "1", "text": "x"}},
                               url="http://127.0.0.1:1")
        self.assertEqual(r["error"]["kind"], "transient")
        self.assertNotIn("SECRET", json.dumps(r))
        self.assertNotIn("127.0.0.1", json.dumps(r))

    def test_token_in_api_description_is_scrubbed(self):
        self.srv.forced["sendMessage"] = (400, {"ok": False, "error_code": 400,
                                                "description": f"bad {TOKEN}"})
        (r,) = self.run_plugin({"id": 1, "op": "send", "params": {"group": "1", "text": "x"}})
        self.assertNotIn("SECRET", json.dumps(r))

    def test_missing_token(self):
        (r,) = self.run_plugin({"id": 1, "op": "leave", "params": {"group": "1"}}, token=None)
        self.assertEqual(r["error"]["kind"], "unauthorized")


if __name__ == "__main__":
    unittest.main()
