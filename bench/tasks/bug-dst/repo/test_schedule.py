import unittest
from datetime import datetime
from zoneinfo import ZoneInfo
from schedule import next_daily

M = ZoneInfo("Europe/Madrid")

class T(unittest.TestCase):
    def test_same_day(self):
        r = next_daily(datetime(2026, 6, 1, 8, 0, tzinfo=M), 9, 0, "Europe/Madrid")
        self.assertEqual((r.month, r.day, r.hour, r.minute), (6, 1, 9, 0))
