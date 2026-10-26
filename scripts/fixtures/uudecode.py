"""Decode a uuencoded file (libarchive's test files): uudecode.py in.uu out"""
import binascii, sys

lines = open(sys.argv[1]).read().splitlines()
start = next(i for i, l in enumerate(lines) if l.startswith("begin "))
body = []
for l in lines[start + 1 :]:
    if l == "end":
        break
    if l and l != "`":
        body.append(binascii.a2b_uu(l))
open(sys.argv[2], "wb").write(b"".join(body))
