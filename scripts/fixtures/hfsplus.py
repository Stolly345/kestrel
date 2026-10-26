"""Cut the HFS+ volume out of an xorriso -hfsplus hybrid ISO: hfsplus.py in.iso out.hfs [--hfsx]

No Linux tool fills an HFS+ image without mounting it, but xorriso builds one inside a hybrid
ISO (Apple partition map). The volume runs from its partition's start to the end of the file
(its files' blocks lie past the partition's nominal end). --hfsx relabels it as HFSX, the
case-sensitive variant (signature "HX", version 5).
"""
import struct, sys

d = open(sys.argv[1], "rb").read()
bs = struct.unpack(">H", d[2:4])[0]
count = struct.unpack(">I", d[bs + 4 : bs + 8])[0]
for i in range(count):
    e = d[bs * (1 + i) : bs * (2 + i)]
    if e[48:80].rstrip(b"\0") == b"Apple_HFS":
        v = bytearray(d[struct.unpack(">I", e[8:12])[0] * bs :])
        assert v[1024:1026] == b"H+", "no HFS+ volume header"
        if "--hfsx" in sys.argv:
            v[1024:1028] = b"HX\x00\x05"
        open(sys.argv[2], "wb").write(v)
        break
else:
    sys.exit("no Apple_HFS partition")
