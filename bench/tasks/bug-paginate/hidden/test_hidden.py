import unittest
from pager import paginate

class H(unittest.TestCase):
    def test_partial_last_page(self):
        self.assertEqual(paginate(list(range(25)), 3, 10), (list(range(20, 25)), 3))
        self.assertEqual(paginate(list(range(25)), 1, 10)[1], 3)
    def test_exact(self):
        self.assertEqual(paginate(list(range(30)), 3, 10), (list(range(20, 30)), 3))
    def test_out_of_range(self):
        self.assertEqual(paginate(list(range(25)), 4, 10), ([], 3))
        self.assertEqual(paginate(list(range(25)), 0, 10), ([], 3))
    def test_empty(self):
        self.assertEqual(paginate([], 1, 10), ([], 0))
