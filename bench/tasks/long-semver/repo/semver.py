"""satisfies(version, range) -> bool, matching node-semver 7 `satisfies` (default options).

version: MAJOR.MINOR.PATCH with an optional -prerelease (dot-separated identifiers).
range grammar:
- comparators: =, >, >=, <, <= or a bare version (same as =); whitespace-separated
  comparators are ANDed, `||` ORs comparator sets.
- hyphen ranges "A - B": inclusive; a partial B rounds up (1.2 - 2.3 means >=1.2.0 <2.4.0).
- X-ranges: "*", "" , "1", "1.x", "1.2.*".
- tilde: ~1.2.3, ~1.2, ~1 allow patch-level changes (minor-level if only major given).
- caret: ^ allows changes that do not modify the left-most non-zero part (^0.2.3, ^0.0.3, ^0.x ...).
- precedence: SemVer 2.0.0 (prerelease < release; numeric identifiers compare numerically
  and sort before alphanumeric ones; a longer set wins when all previous are equal).
- prereleases: a version with a prerelease tag only satisfies a comparator set if some
  comparator in that set has the same MAJOR.MINOR.PATCH and also has a prerelease tag.
Invalid versions raise ValueError.
"""


def satisfies(version, range_):
    raise NotImplementedError
