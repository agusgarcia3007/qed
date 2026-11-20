import unittest
from datetime import datetime, timezone
from zoneinfo import ZoneInfo
from schedule import next_daily

M = ZoneInfo("Europe/Madrid")
NY = ZoneInfo("America/New_York")

def wall(d):
    return (d.year, d.month, d.day, d.hour, d.minute)

class H(unittest.TestCase):
    def test_after_fall_back(self):
        r = next_daily(datetime(2026, 10, 24, 9, 30, tzinfo=M), 9, 0, "Europe/Madrid")
        self.assertEqual(wall(r), (2026, 10, 25, 9, 0))
        self.assertEqual(r.utcoffset().total_seconds(), 3600)
    def test_after_spring_forward(self):
        r = next_daily(datetime(2026, 3, 28, 9, 30, tzinfo=M), 9, 0, "Europe/Madrid")
        self.assertEqual(wall(r), (2026, 3, 29, 9, 0))
    def test_strictly_after(self):
        r = next_daily(datetime(2026, 6, 1, 9, 0, tzinfo=M), 9, 0, "Europe/Madrid")
        self.assertEqual(wall(r), (2026, 6, 2, 9, 0))
    def test_other_input_zone(self):
        after = datetime(2026, 11, 1, 12, 0, tzinfo=timezone.utc)   # 07:00 in New York (EST)
        r = next_daily(after, 9, 0, "America/New_York")
        self.assertEqual(wall(r), (2026, 11, 1, 9, 0))
        self.assertEqual(r.utcoffset().total_seconds(), -5 * 3600)
    def test_gap(self):
        r = next_daily(datetime(2026, 3, 29, 0, 0, tzinfo=M), 2, 30, "Europe/Madrid")
        self.assertEqual(r.astimezone(timezone.utc), datetime(2026, 3, 29, 1, 30, tzinfo=timezone.utc))
    def test_ambiguous_first(self):
        r = next_daily(datetime(2026, 10, 25, 0, 0, tzinfo=M), 2, 30, "Europe/Madrid")
        self.assertEqual(r.astimezone(timezone.utc), datetime(2026, 10, 25, 0, 30, tzinfo=timezone.utc))
    def test_week_of_reminders(self):
        t = datetime(2026, 10, 20, 12, 0, tzinfo=M)
        for _ in range(10):
            t = next_daily(t, 9, 0, "Europe/Madrid")
            self.assertEqual((t.hour, t.minute), (9, 0))
