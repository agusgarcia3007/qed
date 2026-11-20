import unittest
from csvparse import parse_csv

class T(unittest.TestCase):
    def test_simple(self):
        self.assertEqual(parse_csv("a,b\nc,d\n"), [["a", "b"], ["c", "d"]])
