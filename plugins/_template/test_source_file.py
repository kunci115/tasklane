import json
import os
import subprocess
import sys
import tempfile
import unittest

SCRIPT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "source_file.py")
DATA = {"repos": {"acme/shop": {"default_branch": "main", "issues": {
    "1": {"title": "Login fails", "body": "Blank screen", "author": "ann", "updated_at": "2026-01-01T00:00:00Z",
          "comments": [{"author": "bob", "body": "same here", "at": "t"}]}}}}}


def talk(path, *requests):
    env = {"PATH": os.environ["PATH"], "FILE_SOURCE_PATH": path}
    p = subprocess.run([sys.executable, "-I", SCRIPT], input="\n".join(json.dumps(r) for r in requests) + "\n",
                       capture_output=True, text=True, timeout=30, env=env)
    return [json.loads(l) for l in p.stdout.splitlines()]


class Template(unittest.TestCase):
    def setUp(self):
        self.dir = tempfile.mkdtemp()
        self.path = os.path.join(self.dir, "tickets.json")
        with open(self.path, "w") as f:
            json.dump(DATA, f)

    def test_describe_declares_the_protocol(self):
        (r,) = talk(self.path, {"id": 1, "op": "describe"})
        self.assertTrue(r["ok"])
        self.assertEqual((r["id"], r["result"]["protocol_major"], r["result"]["role"]), (1, 1, "ticket_source"))

    def test_services_items_and_comments(self):
        a, b, c, d = talk(self.path,
                          {"id": 1, "op": "list_services"},
                          {"id": 2, "op": "get_item", "params": {"service": "acme/shop", "ref": "1"}},
                          {"id": 3, "op": "comment", "params": {"service": "acme/shop", "ref": "1", "body": "On it"}},
                          {"id": 4, "op": "get_item", "params": {"service": "acme/shop", "ref": "1"}})
        self.assertEqual(a["result"]["services"][0]["external_id"], "acme/shop")
        self.assertEqual(b["result"]["title"], "Login fails")
        self.assertEqual(c["result"]["id"], "2")
        self.assertEqual(d["result"]["comments"][-1]["body"], "On it")
        self.assertTrue(d["result"]["comments"][-1]["mine"])

    def test_changes_start_from_a_cursor_and_report_only_newer_items(self):
        (first,) = talk(self.path, {"id": 1, "op": "list_changes", "params": {}})
        self.assertEqual(first["result"]["items"], [])
        cursor = first["result"]["cursor"]
        with open(self.path) as f:
            data = json.load(f)
        data["repos"]["acme/shop"]["issues"]["1"]["updated_at"] = "2026-02-01T00:00:00Z"
        with open(self.path, "w") as f:
            json.dump(data, f)
        (second,) = talk(self.path, {"id": 2, "op": "list_changes", "params": {"cursor": cursor}})
        self.assertEqual([i["ref"] for i in second["result"]["items"]], ["1"])

    def test_errors_are_results_with_a_kind_and_the_loop_survives(self):
        a, b, c = talk(self.path,
                       {"id": 1, "op": "get_item", "params": {"service": "nope", "ref": "1"}},
                       {"id": 2, "op": "no_such_op"},
                       {"id": 3, "op": "describe"})
        self.assertEqual((a["ok"], a["error"]["kind"]), (False, "permanent"))
        self.assertEqual((b["ok"], b["error"]["kind"]), (False, "permanent"))
        self.assertTrue(c["ok"], "a bad request does not stop the plug-in")


if __name__ == "__main__":
    unittest.main()
