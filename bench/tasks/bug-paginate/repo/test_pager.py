import unittest
from pager import paginate

class T(unittest.TestCase):
    def test_full_pages(self):
        self.assertEqual(paginate(list(range(20)), 2, 10), (list(range(10, 20)), 2))
