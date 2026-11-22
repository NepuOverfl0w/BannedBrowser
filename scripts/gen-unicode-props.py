#!/usr/bin/env python3
"""Generate crates/bb-js/src/vm/regex/unicode_props.txt from the Unicode Character Database.

Usage: scripts/gen-unicode-props.py UCD_DIR > crates/bb-js/src/vm/regex/unicode_props.txt

UCD_DIR holds (from https://www.unicode.org/Public/17.0.0/ucd/ and .../emoji/):
UnicodeData.txt Scripts.txt ScriptExtensions.txt PropList.txt DerivedCoreProperties.txt
CaseFolding.txt PropertyValueAliases.txt emoji-data.txt DerivedBinaryProperties.txt
DerivedNormalizationProps.txt emoji-sequences.txt emoji-zwj-sequences.txt

Output lines (code points in hex, ranges `a-b`, separated by `,`):
  B name ranges         binary property (ECMA-262 Table 67) or `Any`/`ASCII`/`Assigned`
  G name ranges         General_Category value
  S name ranges         Script value
  X name ranges         Script_Extensions value
  A alias canonical     alias of a G/S/B name
  E name seqs           property of strings: sequences of hex code points joined by `_`
  F from:to,...         simple case folding (status C and S)
"""
import os
import re
import sys
from collections import defaultdict

D = sys.argv[1]
MAX = 0x10FFFF


def lines(name):
    with open(os.path.join(D, name), encoding="utf-8") as f:
        for line in f:
            line = line.split("#", 1)[0].strip()
            if line:
                yield [x.strip() for x in line.split(";")]


def parse_range(s):
    if ".." in s:
        a, b = s.split("..")
        return int(a, 16), int(b, 16)
    v = int(s, 16)
    return v, v


def ranges_of(cps):
    out = []
    for c in sorted(cps):
        if out and out[-1][1] + 1 == c:
            out[-1][1] = c
        else:
            out.append([c, c])
    return out


def merge(rs):
    rs = sorted(rs)
    out = []
    for a, b in rs:
        if out and a <= out[-1][1] + 1:
            out[-1][1] = max(out[-1][1], b)
        else:
            out.append([a, b])
    return out


def fmt(rs):
    return ",".join(f"{a:x}" if a == b else f"{a:x}-{b:x}" for a, b in rs)


# General_Category from UnicodeData (ranges given as First/Last pairs).
gc = {}
first = None
for f in lines("UnicodeData.txt"):
    cp = int(f[0], 16)
    if f[1].endswith(", First>"):
        first = cp
        continue
    if f[1].endswith(", Last>"):
        for c in range(first, cp + 1):
            gc[c] = f[2]
        continue
    gc[cp] = f[2]
by_gc = defaultdict(list)
for c in range(MAX + 1):
    by_gc[gc.get(c, "Cn")].append(c)
groups = {"L": "Lu Ll Lt Lm Lo", "LC": "Lu Ll Lt", "M": "Mn Mc Me", "N": "Nd Nl No", "P": "Pc Pd Ps Pe Pi Pf Po", "S": "Sm Sc Sk So", "Z": "Zs Zl Zp", "C": "Cc Cf Cs Co Cn"}
out = []
gc_ranges = {k: ranges_of(v) for k, v in by_gc.items()}
for g, members in groups.items():
    gc_ranges[g] = merge([r for m in members.split() for r in gc_ranges.get(m, [])])
for k in sorted(gc_ranges):
    out.append(f"G {k} {fmt(gc_ranges[k])}")

# Scripts and Script_Extensions.
sc_of = {}
by_sc = defaultdict(list)
for f in lines("Scripts.txt"):
    a, b = parse_range(f[0])
    by_sc[f[1]].append((a, b))
    for c in range(a, b + 1):
        sc_of[c] = f[1]
for k in sorted(by_sc):
    out.append(f"S {k} {fmt(merge(by_sc[k]))}")
# Short script names (ScriptExtensions uses them) → long names.
short_to_long = {}
for f in lines("PropertyValueAliases.txt"):
    if f[0] == "sc":
        short_to_long[f[1]] = f[2]
        for extra in f[3:]:
            short_to_long[extra] = f[2]
scx = defaultdict(set)
explicit = {}
for f in lines("ScriptExtensions.txt"):
    a, b = parse_range(f[0])
    names = [short_to_long.get(n, n) for n in f[1].split()]
    for c in range(a, b + 1):
        explicit[c] = names
for c in range(MAX + 1):
    names = explicit.get(c) or [sc_of.get(c, "Unknown")]
    for n in names:
        scx[n].add(c)
for k in sorted(set(by_sc) | set(scx)):
    out.append(f"X {k} {fmt(ranges_of(scx.get(k, set())))}")

# Binary properties.
binary = defaultdict(list)
for fname in ["PropList.txt", "DerivedCoreProperties.txt", "emoji-data.txt", "DerivedBinaryProperties.txt", "DerivedNormalizationProps.txt"]:
    for f in lines(fname):
        if len(f) >= 2:
            a, b = parse_range(f[0])
            binary[f[1]].append((a, b))
wanted = """ASCII_Hex_Digit Alphabetic Bidi_Control Bidi_Mirrored Case_Ignorable Cased
Changes_When_Casefolded Changes_When_Casemapped Changes_When_Lowercased
Changes_When_NFKC_Casefolded Changes_When_Titlecased Changes_When_Uppercased Dash
Default_Ignorable_Code_Point Deprecated Diacritic Emoji Emoji_Component Emoji_Modifier
Emoji_Modifier_Base Emoji_Presentation Extended_Pictographic Extender Grapheme_Base
Grapheme_Extend Hex_Digit IDS_Binary_Operator IDS_Trinary_Operator ID_Continue ID_Start
Ideographic Join_Control Logical_Order_Exception Lowercase Math Noncharacter_Code_Point
Pattern_Syntax Pattern_White_Space Quotation_Mark Radical Regional_Indicator
Sentence_Terminal Soft_Dotted Terminal_Punctuation Unified_Ideograph Uppercase
Variation_Selector White_Space XID_Continue XID_Start""".split()
for k in wanted:
    out.append(f"B {k} {fmt(merge(binary[k]))}")
out.append("B Any 0-10ffff")
out.append("B ASCII 0-7f")
out.append(f"B Assigned {fmt(merge([(0, MAX)] if False else [r for k, v in gc_ranges.items() if k not in groups and k != 'Cn' for r in v]))}")

# Aliases: binary short names (PropertyAliases is fixed for these), gc and sc value aliases.
binary_alias = {"AHex": "ASCII_Hex_Digit", "Alpha": "Alphabetic", "Bidi_C": "Bidi_Control", "Bidi_M": "Bidi_Mirrored", "CI": "Case_Ignorable",
    "CWCF": "Changes_When_Casefolded", "CWCM": "Changes_When_Casemapped", "CWKCF": "Changes_When_NFKC_Casefolded", "CWL": "Changes_When_Lowercased",
    "CWT": "Changes_When_Titlecased", "CWU": "Changes_When_Uppercased", "DI": "Default_Ignorable_Code_Point", "Dep": "Deprecated", "Dia": "Diacritic",
    "EBase": "Emoji_Modifier_Base", "EComp": "Emoji_Component", "EMod": "Emoji_Modifier", "EPres": "Emoji_Presentation", "Ext": "Extender",
    "ExtPict": "Extended_Pictographic", "Gr_Base": "Grapheme_Base", "Gr_Ext": "Grapheme_Extend", "Hex": "Hex_Digit", "IDC": "ID_Continue",
    "IDS": "ID_Start", "IDSB": "IDS_Binary_Operator", "IDST": "IDS_Trinary_Operator", "Ideo": "Ideographic", "Join_C": "Join_Control",
    "LOE": "Logical_Order_Exception", "Lower": "Lowercase", "NChar": "Noncharacter_Code_Point", "Pat_Syn": "Pattern_Syntax",
    "Pat_WS": "Pattern_White_Space", "QMark": "Quotation_Mark", "RI": "Regional_Indicator", "SD": "Soft_Dotted", "STerm": "Sentence_Terminal",
    "Term": "Terminal_Punctuation", "UIdeo": "Unified_Ideograph", "Upper": "Uppercase", "VS": "Variation_Selector", "space": "White_Space",
    "XIDC": "XID_Continue", "XIDS": "XID_Start"}
for a, c in sorted(binary_alias.items()):
    out.append(f"A B:{a} {c}")
for f in lines("PropertyValueAliases.txt"):
    if f[0] == "gc":
        for alias in [f[1]] + f[3:]:
            if alias != f[2]:
                out.append(f"A G:{alias} {f[1]}")
        if f[2] != f[1]:
            out.append(f"A G:{f[2]} {f[1]}")
    if f[0] == "sc":
        for alias in [f[1]] + f[3:]:
            if alias != f[2]:
                out.append(f"A S:{alias} {f[2]}")

# Properties of strings (v flag, §22.2.2.9.2 Table 69).
seqs = defaultdict(list)
for fname in ["emoji-sequences.txt", "emoji-zwj-sequences.txt"]:
    for f in lines(fname):
        if len(f) < 2:
            continue
        kind = f[1]
        if ".." in f[0]:
            a, b = parse_range(f[0])
            for c in range(a, b + 1):
                seqs[kind].append(f"{c:x}")
        else:
            seqs[kind].append("_".join(x.lower() for x in f[0].split()))
names = {"Basic_Emoji": "Basic_Emoji", "Emoji_Keycap_Sequence": "Emoji_Keycap_Sequence", "RGI_Emoji_Modifier_Sequence": "RGI_Emoji_Modifier_Sequence",
    "RGI_Emoji_Flag_Sequence": "RGI_Emoji_Flag_Sequence", "RGI_Emoji_Tag_Sequence": "RGI_Emoji_Tag_Sequence", "RGI_Emoji_ZWJ_Sequence": "RGI_Emoji_ZWJ_Sequence"}
all_rgi = []
for k, n in names.items():
    out.append(f"E {n} {','.join(seqs[k])}")
    all_rgi += seqs[k]
out.append(f"E RGI_Emoji {','.join(all_rgi)}")

# Simple case folding.
fold = []
for f in lines("CaseFolding.txt"):
    if f[1] in ("C", "S"):
        fold.append(f"{int(f[0], 16):x}:{int(f[2], 16):x}")
out.append("F " + ",".join(fold))

print("# Generated by scripts/gen-unicode-props.py from the Unicode 17.0 UCD. Do not edit.")
print("\n".join(out))
