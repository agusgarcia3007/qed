import unittest
from csvparse import parse_csv

class H(unittest.TestCase):
    def test_multiline_and_escaped_quotes(self):
        t = 'id,note\r\n1,"line one\r\nline two"\r\n2,"she said ""hi"""\r\n'
        self.assertEqual(parse_csv(t), [["id", "note"], ["1", "line one\r\nline two"], ["2", 'she said "hi"']])
    def test_commas_in_quotes_and_empty_fields(self):
        self.assertEqual(parse_csv('a,"b,c",,\n'), [["a", "b,c", "", ""]])
        self.assertEqual(parse_csv('""'), [[""]])
    def test_literal_quote_in_unquoted(self):
        self.assertEqual(parse_csv('5" pipe,x'), [['5" pipe', "x"]])
    def test_record_rules(self):
        self.assertEqual(parse_csv(""), [])
        self.assertEqual(parse_csv("a"), [["a"]])
        self.assertEqual(parse_csv("a\n\nb"), [["a"], [""], ["b"]])
        self.assertEqual(parse_csv("a\r\nb\r\n"), [["a"], ["b"]])
    def test_errors(self):
        for bad in ['"abc', 'a,"b"c', '"x"y\n']:
            with self.assertRaises(ValueError, msg=bad):
                parse_csv(bad)
