"""Wrap a zip in a CRX3 header (what Chrome extensions are): crx.py in.zip out.crx"""
import struct, sys

z = open(sys.argv[1], "rb").read()
header = b"\x00" * 40  # stands in for the signed protobuf header; extractors skip it
open(sys.argv[2], "wb").write(b"Cr24" + struct.pack("<II", 3, len(header)) + header + z)
