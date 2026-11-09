import json
import subprocess
import sys
import unittest
from pathlib import Path

SCRIPT = Path(__file__).with_name("engine_claude.py")


def talk(*requests):
    p = subprocess.run([sys.executable, "-I", str(SCRIPT)],
                       input="\n".join(json.dumps(r) for r in requests) + "\n",
                       capture_output=True, text=True, timeout=10)
    return [json.loads(l) for l in p.stdout.splitlines()]


class EngineClaude(unittest.TestCase):
    def test_describe(self):
        (r,) = talk({"id": 1, "op": "describe"})
        self.assertTrue(r["ok"])
        self.assertEqual(r["result"]["role"], "agent_engine")
        self.assertEqual(r["result"]["protocol_major"], 1)

    def test_run_is_refused_for_now(self):
        (r,) = talk({"id": 2, "op": "run", "params": {}})
        self.assertFalse(r["ok"])
        self.assertEqual(r["error"]["kind"], "permanent")

    def test_unknown_op_and_ids_are_echoed(self):
        a, b = talk({"id": 3, "op": "nope"}, {"id": 4, "op": "capabilities"})
        self.assertEqual((a["id"], a["ok"]), (3, False))
        self.assertEqual((b["id"], b["ok"]), (4, True))


if __name__ == "__main__":
    unittest.main()
