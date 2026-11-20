import json, os, unittest
from semver import satisfies

CASES = json.load(open(os.path.join(os.path.dirname(__file__), "cases.json")))

class H(unittest.TestCase):
    def test_against_node_semver(self):
        bad = [(v, r, exp) for v, r, exp in CASES if satisfies(v, r) != exp]
        self.assertEqual(bad[:15], [], f"{len(bad)}/{len(CASES)} mismatches")
    def test_invalid_version(self):
        for v in ["1.2", "a.b.c", "1.2.3.4", "01.2.3"]:
            with self.assertRaises(ValueError, msg=v):
                satisfies(v, "*")
