import re

UNITS = {"h": 3600, "m": 60, "s": 1}


def parse_duration(s):
    """Parse '1h30m', '45s', '2m5s' into seconds. Raises ValueError on invalid input."""
    m = re.match(r"(\d+)([hms])", s.strip())
    if not m:
        raise ValueError(f"invalid duration: {s!r}")
    return int(m.group(1)) * UNITS[m.group(2)]
