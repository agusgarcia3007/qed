from datetime import datetime, time, timedelta, timezone
from zoneinfo import ZoneInfo


def next_daily(after, hh, mm, tz):
    """Return the first instant strictly after `after` (an aware datetime) whose
    wall-clock time in IANA zone `tz` is hh:mm, as an aware datetime in that zone.

    - If hh:mm does not exist that day (spring-forward gap), use the instant you get
      by reading hh:mm with the offset in force before the gap (e.g. 02:30 -> 03:30 CEST).
    - If hh:mm happens twice (fall-back), use the first occurrence.
    """
    z = ZoneInfo(tz)
    now = after.astimezone(timezone.utc)
    first = datetime.combine(now.astimezone(z).date(), time(hh, mm), tzinfo=z).astimezone(timezone.utc)
    while first <= now:
        first += timedelta(days=1)
    return first.astimezone(z)
