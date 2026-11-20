import unittest
from memo import memoize

def counted():
    calls = []
    @memoize(maxsize=2)
    def f(a, b=2):
        calls.append((a, b))
        return a * 10 + b
    return f, calls

class H(unittest.TestCase):
    def test_kw_order(self):
        f, _ = counted()
        self.assertEqual(f(a=1, b=2), 12)
        self.assertEqual(f(b=1, a=2), 21)
    def test_binding_equivalence(self):
        f, calls = counted()
        f(1, 2); f(1, b=2); f(b=2, a=1); f(1)
        self.assertEqual(len(calls), 1)
        self.assertEqual(f.cache_info(), {"hits": 3, "misses": 1, "size": 1})
    def test_unhashable_bypass(self):
        calls = []
        @memoize()
        def g(x):
            calls.append(1)
            return len(x)
        self.assertEqual(g([1, 2]), 2)
        self.assertEqual(g([1, 2]), 2)
        self.assertEqual(len(calls), 2)
        self.assertEqual(g.cache_info(), {"hits": 0, "misses": 0, "size": 0})
    def test_lru(self):
        f, calls = counted()
        f(1); f(2); f(1); f(3)   # 2 is least recently used -> evicted
        f(1)
        self.assertEqual(len(calls), 3)
        f(2)
        self.assertEqual(len(calls), 4)
    def test_clear(self):
        f, calls = counted()
        f(1); f(1)
        f.cache_clear()
        self.assertEqual(f.cache_info(), {"hits": 0, "misses": 0, "size": 0})
        f(1)
        self.assertEqual(len(calls), 2)
    def test_wraps(self):
        f, _ = counted()
        self.assertEqual(f.__name__, "f")
