"""Minimal macOS UDIF disk image (.dmg) holding raw bytes: udif.py in.img out.dmg

No Linux tool writes .dmg. This is the simplest valid one: the data fork is the image as
one raw ("UDRW") chunk, the resource-fork plist has its blkx table, and a koly trailer ends it.
"""
import base64, struct, sys

data = open(sys.argv[1], "rb").read()
data += b"\x00" * (-len(data) % 512)
sectors = len(data) // 512

# mish (blkx) table: header, one raw chunk, terminator
mish = b"mish" + struct.pack(">IQQQII", 1, 0, sectors, 0, 0, 0) + b"\x00" * 24
mish += struct.pack(">II", 0, 0) + b"\x00" * 128  # no checksum
mish += struct.pack(">I", 2)
mish += struct.pack(">IIQQQQ", 0x00000001, 0, 0, sectors, 0, len(data))
mish += struct.pack(">IIQQQQ", 0xFFFFFFFF, 0, sectors, 0, len(data), 0)

plist = f"""<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict><key>resource-fork</key><dict><key>blkx</key><array><dict>
<key>Attributes</key><string>0x0050</string>
<key>CFName</key><string>disk image (Apple_HFS : 0)</string>
<key>Data</key><data>{base64.b64encode(mish).decode()}</data>
<key>ID</key><string>0</string>
<key>Name</key><string>disk image (Apple_HFS : 0)</string>
</dict></array></dict></dict></plist>
""".encode()

koly = b"koly" + struct.pack(">III", 4, 512, 1)
koly += struct.pack(">QQQQQ", 0, 0, len(data), 0, 0)
koly += struct.pack(">II", 1, 1) + b"\x00" * 16  # segment 1 of 1, segment id
koly += struct.pack(">II", 0, 0) + b"\x00" * 128  # data checksum: none
koly += struct.pack(">QQ", len(data), len(plist)) + b"\x00" * 120
koly += struct.pack(">II", 0, 0) + b"\x00" * 128  # master checksum: none
koly += struct.pack(">IQ", 1, sectors) + b"\x00" * 12
assert len(koly) == 512, len(koly)
open(sys.argv[2], "wb").write(data + plist + koly)
