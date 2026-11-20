import unittest
from cache import TTLCache

class T(unittest.TestCase):
    def test_basic(self):
        c = TTLCache(2, 10)
        c.set("a", 1)
        self.assertEqual(c.get("a"), 1)
