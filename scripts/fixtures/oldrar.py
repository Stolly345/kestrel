"""Turn a RAR4 x.part1.rar, x.part2.rar… set into old-style x.rar, x.r00, x.r01…:
oldrar.py OUT_STEM part1 part2 …

RAR4 marks .partN naming with the main header's MHD_NEWNUMBERING flag (0x0010); without it,
readers look for .rar/.r00/.r01. Clearing the flag means recomputing that header's CRC16.
"""
import struct, sys, zlib

stem, parts = sys.argv[1], sys.argv[2:]
for i, p in enumerate(parts):
    d = bytearray(open(p, "rb").read())
    assert d[:7] == b"Rar!\x1a\x07\x00", f"{p} is not RAR4"
    o = 7  # main header follows the marker block
    assert d[o + 2] == 0x73, "main header expected"
    flags, size = struct.unpack_from("<HH", d, o + 3)
    struct.pack_into("<H", d, o + 3, flags & ~0x0010)
    struct.pack_into("<H", d, o, zlib.crc32(bytes(d[o + 2 : o + size])) & 0xFFFF)
    open(f"{stem}.rar" if i == 0 else f"{stem}.r{i - 1:02d}", "wb").write(d)
