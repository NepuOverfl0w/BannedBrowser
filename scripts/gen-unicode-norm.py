#!/usr/bin/env python3
"""Generate crates/bb-js/src/vm/unicode_norm.txt from the Unicode Character Database.

Usage: scripts/gen-unicode-norm.py UCD_DIR > crates/bb-js/src/vm/unicode_norm.txt

UCD_DIR holds UnicodeData.txt and DerivedNormalizationProps.txt
(from https://www.unicode.org/Public/17.0.0/ucd/).

Output lines (code points in hex):
  C ccc ranges          Canonical_Combining_Class `ccc` (decimal) for ranges `a-b,c,...`
  D cp seq              canonical Decomposition_Mapping of cp (one level, `_`-joined)
  K cp seq              compatibility Decomposition_Mapping of cp (one level)
  X ranges              Full_Composition_Exclusion
Hangul syllables are decomposed and composed algorithmically (UAX #15, §16 of the Core
Specification) and are not listed.
"""
import os
import sys

D = sys.argv[1]


def lines(name):
    with open(os.path.join(D, name), encoding="utf-8") as f:
        for line in f:
            line = line.split("#", 1)[0].strip()
            if line:
                yield line


def ranges(cps):
    cps = sorted(cps)
    out = []
    i = 0
    while i < len(cps):
        j = i
        while j + 1 < len(cps) and cps[j + 1] == cps[j] + 1:
            j += 1
        out.append(f"{cps[i]:x}" if i == j else f"{cps[i]:x}-{cps[j]:x}")
        i = j + 1
    return ",".join(out)


ccc = {}
canon = {}
compat = {}
first = None
for line in lines("UnicodeData.txt"):
    f = line.split(";")
    cp = int(f[0], 16)
    name = f[1]
    if name.endswith(", First>"):
        first = cp
        continue
    span = range(first, cp + 1) if name.endswith(", Last>") else [cp]
    first = None
    for c in span:
        if int(f[3]) != 0:
            ccc[c] = int(f[3])
    if f[5]:
        parts = f[5].split()
        if parts[0].startswith("<"):
            compat[cp] = [int(p, 16) for p in parts[1:]]
        else:
            canon[cp] = [int(p, 16) for p in parts]

excl = set()
for line in lines("DerivedNormalizationProps.txt"):
    f = [x.strip() for x in line.split(";")]
    if f[1] != "Full_Composition_Exclusion":
        continue
    if ".." in f[0]:
        a, b = f[0].split("..")
        excl.update(range(int(a, 16), int(b, 16) + 1))
    else:
        excl.add(int(f[0], 16))

by_class = {}
for c, k in ccc.items():
    by_class.setdefault(k, []).append(c)
for k in sorted(by_class):
    print(f"C {k} {ranges(by_class[k])}")
for c in sorted(canon):
    print(f"D {c:x} {'_'.join(f'{x:x}' for x in canon[c])}")
for c in sorted(compat):
    print(f"K {c:x} {'_'.join(f'{x:x}' for x in compat[c])}")
print(f"X {ranges(excl)}")
