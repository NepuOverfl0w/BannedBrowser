#!/usr/bin/env python3
"""Generate crates/bb-js/src/vm/tzdata.txt from compiled IANA time zone data.

Usage: scripts/gen-tzdata.py TZSRC_DIR ZONEINFO_DIR > crates/bb-js/src/vm/tzdata.txt

TZSRC_DIR is an unpacked IANA tzdata release (https://data.iana.org/time-zones/releases/),
ZONEINFO_DIR what zic made of it:

    zic -d ZONEINFO_DIR africa antarctica asia australasia europe northamerica \
        southamerica etcetera backward

The release's Zone and Link lines name the zones and links; each zone's TZif file
(RFC 9636) gives its transitions and the POSIX TZ rule for later instants.

Output:
  # version
  Z name|rule|offsets|transitions
      rule         POSIX TZ string for instants after the last transition (may be empty)
      offsets      UT offsets in seconds, comma-separated; the first applies before the
                   first transition
      transitions  `delta:index` pairs: seconds since the previous transition (the first
                   since the epoch, possibly negative) in base 36, and the offset index
  L link target
"""
import os
import struct
import sys

SRC = sys.argv[1]
D = sys.argv[2]
SOURCES = ["africa", "antarctica", "asia", "australasia", "europe", "northamerica", "southamerica",
           "etcetera", "backward"]


def base36(n):
    neg = n < 0
    n = abs(n)
    digits = "0123456789abcdefghijklmnopqrstuvwxyz"
    s = ""
    while True:
        s = digits[n % 36] + s
        n //= 36
        if n == 0:
            break
    return "-" + s if neg else s


def tzif(path):
    b = open(path, "rb").read()
    if b[:4] != b"TZif":
        raise ValueError(path)
    isutc, isstd, leap, timecnt, typecnt, charcnt = struct.unpack(">6l", b[20:44])
    # Skip the version 1 (32-bit) block; use the 64-bit one.
    p = 44 + timecnt * 5 + typecnt * 6 + charcnt + leap * 8 + isstd + isutc
    b = b[p:]
    if b[:4] != b"TZif":
        raise ValueError(path + ": no 64-bit data")
    isutc, isstd, leap, timecnt, typecnt, charcnt = struct.unpack(">6l", b[20:44])
    p = 44
    times = struct.unpack(">%dq" % timecnt, b[p : p + 8 * timecnt])
    p += 8 * timecnt
    idx = b[p : p + timecnt]
    p += timecnt
    types = [struct.unpack(">lBB", b[p + 6 * i : p + 6 * i + 6]) for i in range(typecnt)]
    p += 6 * typecnt + charcnt + leap * 12 + isstd + isutc
    footer = b[p:].split(b"\n")[1].decode()
    return times, idx, types, footer


version = open(os.path.join(SRC, "version"), encoding="utf-8").read().strip()
zones = []
links = []
for name in SOURCES:
    for line in open(os.path.join(SRC, name), encoding="utf-8"):
        f = line.split("#", 1)[0].split()
        if len(f) >= 2 and f[0] == "Zone":
            zones.append(f[1])
        elif len(f) >= 3 and f[0] == "Link":
            links.append((f[2], f[1]))

print(f"# IANA tzdata {version}")
for z in sorted(zones):
    times, idx, types, footer = tzif(os.path.join(D, z))
    # Offsets used, in first-use order; type 0 applies before the first transition.
    offsets = []
    def oi(t):
        o = types[t][0]
        if o not in offsets:
            offsets.append(o)
        return offsets.index(o)
    oi(0)
    pairs = []
    prev_t, prev_o = 0, 0
    for t, i in zip(times, idx):
        o = oi(i)
        if o == prev_o:
            continue  # an abbreviation or DST-flag change only
        pairs.append(f"{base36(t - prev_t)}:{o}")
        prev_t, prev_o = t, o
    print(f"Z {z}|{footer}|{','.join(str(o) for o in offsets)}|{' '.join(pairs)}")
for link, target in sorted(links):
    print(f"L {link} {target}")
