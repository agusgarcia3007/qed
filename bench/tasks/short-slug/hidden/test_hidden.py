import unittest
from text import slugify, title_case

class H(unittest.TestCase):
    def test_example(self):
        self.assertEqual(slugify("  Hello, World_2026 "), "hello-world-2026")
    def test_runs_collapse(self):
        self.assertEqual(slugify("a--b__c  d"), "a-b-c-d")
        self.assertEqual(slugify("A - B"), "a-b")
    def test_edges(self):
        self.assertEqual(slugify("--x--"), "x")
        self.assertEqual(slugify(""), "")
        self.assertEqual(slugify("!!!"), "")
    def test_drop_non_ascii(self):
        self.assertEqual(slugify("Caf\u00e9 Ol\u00e9"), "caf-ol")
    def test_regression(self):
        self.assertEqual(title_case("a b"), "A B")
