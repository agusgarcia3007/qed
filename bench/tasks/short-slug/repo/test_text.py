import unittest
from text import title_case

class T(unittest.TestCase):
    def test_title(self):
        self.assertEqual(title_case("hello  world"), "Hello World")
