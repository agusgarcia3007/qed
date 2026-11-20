import unittest
from semver import satisfies

class T(unittest.TestCase):
    def test_basic(self):
        self.assertTrue(satisfies("1.2.3", "^1.0.0"))
        self.assertFalse(satisfies("2.0.0", "^1.0.0"))
