import unittest
from duration import parse_duration

class H(unittest.TestCase):
    def test_compound(self):
        self.assertEqual(parse_duration("1h30m"), 5400)
        self.assertEqual(parse_duration("2m5s"), 125)
        self.assertEqual(parse_duration("1h0m1s"), 3601)
    def test_invalid(self):
        for bad in ["", "abc", "5x", "1h30", "5sx", "h1"]:
            with self.assertRaises(ValueError, msg=bad):
                parse_duration(bad)
