import unittest
from memo import memoize

class T(unittest.TestCase):
    def test_positional(self):
        calls = []
        @memoize()
        def sq(x):
            calls.append(x)
            return x * x
        self.assertEqual(sq(3), 9)
        self.assertEqual(sq(3), 9)
        self.assertEqual(len(calls), 1)
