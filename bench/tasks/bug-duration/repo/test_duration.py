import unittest
from duration import parse_duration

class T(unittest.TestCase):
    def test_single(self):
        self.assertEqual(parse_duration("45s"), 45)
        self.assertEqual(parse_duration("2m"), 120)
