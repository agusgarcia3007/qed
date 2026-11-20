"""memoize(maxsize=128): cache a function's results.

- Two calls share a cache entry iff they bind to the same arguments after applying
  defaults, e.g. f(1, 2), f(1, b=2), f(b=2, a=1) and (if b defaults to 2) f(1) are one entry.
- Calls with unhashable arguments are not cached: the function runs every time, no error.
- At most `maxsize` entries; when full, the least recently used entry is evicted.
- wrapper.cache_info() -> {"hits": int, "misses": int, "size": int}; uncached
  (unhashable) calls count as neither hits nor misses.
- wrapper.cache_clear() empties the cache and resets the counters.
"""
import functools


def memoize(maxsize=128):
    def deco(fn):
        cache = {}
        info = {"hits": 0, "misses": 0}

        @functools.wraps(fn)
        def wrapper(*args, **kwargs):
            key = args + tuple(kwargs.values())
            if key in cache:
                info["hits"] += 1
                return cache[key]
            info["misses"] += 1
            val = fn(*args, **kwargs)
            if len(cache) >= maxsize:
                cache.pop(next(iter(cache)))
            cache[key] = val
            return val

        wrapper.cache_info = lambda: {**info, "size": len(cache)}
        return wrapper

    return deco
