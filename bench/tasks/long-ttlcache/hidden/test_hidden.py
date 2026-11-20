import unittest
from cache import TTLCache

class Clock:
    def __init__(self): self.t = 0.0
    def __call__(self): return self.t

class H(unittest.TestCase):
    def setUp(self):
        self.k = Clock()
        self.c = TTLCache(2, 10, clock=self.k)
    def test_lru_by_get(self):
        self.c.set("a", 1); self.c.set("b", 2)
        self.c.get("a")
        self.c.set("c", 3)
        self.assertIsNone(self.c.get("b"))
        self.assertEqual(self.c.get("a"), 1)
        self.assertEqual(self.c.stats()["evictions"], 1)
    def test_update_moves_to_mru_without_eviction(self):
        self.c.set("a", 1); self.c.set("b", 2); self.c.set("a", 9)
        self.assertEqual(len(self.c), 2)
        self.c.set("c", 3)
        self.assertIsNone(self.c.get("b"))
        self.assertEqual(self.c.get("a"), 9)
    def test_expiry_boundary(self):
        self.c.set("a", 1)
        self.k.t = 9.99
        self.assertEqual(self.c.get("a"), 1)
        self.k.t = 10
        self.assertIsNone(self.c.get("a"))
    def test_len_and_purge_not_eviction(self):
        self.c.set("a", 1); self.c.set("b", 2)
        self.k.t = 11
        self.assertEqual(len(self.c), 0)
        self.c.set("c", 3); self.c.set("d", 4)
        self.assertEqual(self.c.stats()["evictions"], 0)
        self.assertEqual(len(self.c), 2)
    def test_set_resets_age(self):
        self.c.set("a", 1)
        self.k.t = 8; self.c.set("a", 2)
        self.k.t = 15
        self.assertEqual(self.c.get("a"), 2)
    def test_stats_and_delete(self):
        self.c.set("a", 1)
        self.c.get("a"); self.c.get("zz")
        self.k.t = 20
        self.c.get("a")
        self.assertEqual(self.c.stats(), {"hits": 1, "misses": 2, "evictions": 0})
        self.c.set("b", 1)
        self.assertTrue(self.c.delete("b"))
        self.assertFalse(self.c.delete("b"))
        self.c.set("e", 1); self.k.t = 40
        self.assertFalse(self.c.delete("e"))
    def test_default_and_invalid(self):
        self.assertEqual(self.c.get("x", 7), 7)
        for args in [(0, 1), (1, 0), (1, -1)]:
            with self.assertRaises(ValueError):
                TTLCache(*args)
