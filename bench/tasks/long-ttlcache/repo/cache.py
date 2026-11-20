"""TTLCache spec.

TTLCache(max_size, ttl, clock=time.monotonic)
- set(key, value): insert or update; resets the key's age and makes it most-recently-used.
- get(key, default=None): value if present and not expired, and marks it most-recently-used.
  An expired key counts as a miss and is removed. A missing key counts as a miss.
- delete(key) -> bool: True only if a live (non-expired) key was removed.
- len(cache): number of live keys.
- Inserting a NEW key when the cache already holds max_size live keys evicts the
  least-recently-used live key. Expired keys are purged first and do not count as evictions.
- stats() -> {"hits": int, "misses": int, "evictions": int}
- max_size < 1 or ttl <= 0 raises ValueError.
A key expires when clock() - time_it_was_set >= ttl.
"""
import time


class TTLCache:
    pass
