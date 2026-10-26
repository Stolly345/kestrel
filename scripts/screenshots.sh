#!/usr/bin/env bash
# README screenshots: the real app on a made-up home folder ("maya", shown as ~/… so no temp path
# appears), with its own search index. Writes docs/screenshots/{kestrel,search,preview}.png.
#
#   scripts/screenshots.sh
#
# Needs: xvfb, LibreOffice (to make the PDFs), git.
set -eu
cd "$(dirname "$0")/.."
root=$PWD
cargo build --release -q -p kestrel -p kestrel-indexd
W=$(mktemp -d /tmp/kestrel-shots.XXXXXX)
[ -n "${KEEP:-}" ] || trap 'rm -rf "$W"' EXIT
H=$W/maya
mkdir -p "$H"/{Desktop,Documents,Downloads,Music,Pictures,Videos,Projects} "$H/.config"
cat >"$H/.config/user-dirs.dirs" <<'EOF'
XDG_DESKTOP_DIR="$HOME/Desktop"
XDG_DOCUMENTS_DIR="$HOME/Documents"
XDG_DOWNLOAD_DIR="$HOME/Downloads"
XDG_MUSIC_DIR="$HOME/Music"
XDG_PICTURES_DIR="$HOME/Pictures"
XDG_VIDEOS_DIR="$HOME/Videos"
EOF
mkdir -p "$H/.config/gtk-4.0"
printf '[Settings]\ngtk-enable-animations=false\n' >"$H/.config/gtk-4.0/settings.ini"

# documents: PDFs rendered by LibreOffice from HTML
html=$W/html
mkdir -p "$html"
cat >"$html/guide.html" <<'EOF'
<html><body style="font-family: sans-serif">
<h1 style="color:#c2410c">Kestrel Field Guide</h1>
<p><i>Falco sparverius</i>, the American kestrel, is the smallest falcon in North America.</p>
<h2>Identification</h2>
<p>Rusty back and tail, slate-blue wings on the male, two dark vertical stripes on a pale face.
Often seen perched on wires along open country, bobbing its tail.</p>
<h2>Hunting</h2>
<p>Hovers in place over fields on fast wingbeats, then drops onto grasshoppers, mice and small birds.</p>
<p style="page-break-before: always"></p>
<h2>Where to look</h2>
<ul><li>Farmland and meadows</li><li>Road edges with utility wires</li><li>City parks and stadium lights</li></ul>
<h2>Calls</h2><p>A rapid, high <i>klee-klee-klee</i>, loudest near the nest.</p>
<p style="page-break-before: always"></p>
<h2>Field notes</h2>
<table border="1" cellpadding="6" style="border-collapse:collapse">
<tr><th>Date</th><th>Place</th><th>Seen</th></tr>
<tr><td>12 Sep</td><td>River meadow</td><td>2 hovering</td></tr>
<tr><td>3 Oct</td><td>Airport fence</td><td>1 male on wire</td></tr>
<tr><td>21 Oct</td><td>Old orchard</td><td>pair, calling</td></tr>
</table></body></html>
EOF
invoice() { # month amount
    cat >"$html/invoice-$1.html" <<EOF
<html><body style="font-family: sans-serif"><h1>Invoice</h1>
<p><b>Aurora Studio</b><br>Design and illustration</p><p>Invoice 2026-$1 &middot; due in 30 days</p>
<table border="1" cellpadding="8" style="border-collapse:collapse;width:100%">
<tr><th align="left">Item</th><th align="right">Amount</th></tr>
<tr><td>Brand illustration, kestrel series</td><td align="right">$2</td></tr></table>
<p align="right"><b>Total $2</b></p></body></html>
EOF
}
invoice 09 '$1,240.00'
invoice 10 '$1,860.00'
soffice -env:UserInstallation="file://$W/lo" --headless --convert-to pdf --outdir "$html" "$html"/*.html >/dev/null 2>&1
cp "$html/guide.pdf" "$H/Documents/Kestrel Field Guide.pdf"
cp "$html/invoice-09.pdf" "$H/Documents/Invoice 2026-09 Aurora Studio.pdf"
cp "$html/invoice-10.pdf" "$H/Documents/Invoice 2026-10 Aurora Studio.pdf"
printf '# Q3 report\n\nRevenue up **18%%**; Aurora Studio became our largest client.\n\n- Ship the kestrel illustration series\n- Hire a second designer\n' >"$H/Documents/Quarterly report Q3.md"
printf '# Meeting notes\n\n- Aurora Studio wants the poster by Friday\n- Print run: 200 copies\n' >"$H/Documents/Meeting notes.md"

# downloads: ISO images (sparse: real sizes, no disk used) and things that merely contain "iso"
truncate -s 6012954624 "$H/Downloads/ubuntu-26.04-desktop-amd64.iso"
truncate -s 692060160 "$H/Downloads/debian-13.1.0-amd64-netinst.iso"
truncate -s 2537553920 "$H/Downloads/Fedora-Workstation-Live-43.iso"
truncate -s 1288490188 "$H/Downloads/archlinux-2026.10.01-x86_64.iso"
printf 'Week 1 starts Monday 29 December 2025\n' >"$H/Downloads/iso-week-calendar-2026.txt"
printf '1. Wash hands\n2. Mask on\n' >"$H/Downloads/isolation-checklist.txt"

# pictures: smooth gradients
python3 -I - "$H/Pictures" <<'PY'
import struct, sys, zlib
d = sys.argv[1]
def png(name, w, h, f):
    rows = b"".join(b"\0" + bytes(v for x in range(w) for v in f(x / w, y / h)) for y in range(h))
    c = lambda t, b: struct.pack(">I", len(b)) + t + b + struct.pack(">I", zlib.crc32(t + b))
    open(f"{d}/{name}", "wb").write(b"\x89PNG\r\n\x1a\n" + c(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 2, 0, 0, 0)) + c(b"IDAT", zlib.compress(rows)) + c(b"IEND", b""))
png("Aurora over the fjord.png", 480, 320, lambda x, y: (int(20 + 60 * y), int(120 + 120 * (1 - y) * x), int(110 + 100 * y)))
png("Sunrise at the harbour.png", 480, 320, lambda x, y: (int(250 - 40 * y), int(150 + 60 * (1 - y)), int(90 + 120 * y)))
png("Isometric sketch.png", 400, 300, lambda x, y: (int(230 - 30 * x), int(230 - 30 * y), 240))
PY
cp "$H/Pictures/Isometric sketch.png" "$H/Downloads/isometric-sketch.png"

# a small project with git history
P=$H/Projects/aurora-site
mkdir -p "$P/src"
printf '# Aurora Studio website\n' >"$P/README.md"
printf 'fn main() {\n    println!("aurora");\n}\n' >"$P/src/main.rs"
git -C "$P" init -q -b main && git -C "$P" add . && git -C "$P" -c user.name=Maya -c user.email=maya@example.com commit -qm "First page"

out=$root/docs/screenshots
mkdir -p "$out"
R=$W/run
mkdir -p "$R" && chmod 700 "$R"
export HOME=$H XDG_RUNTIME_DIR=$R GDK_BACKEND=x11 GDK_DEBUG=no-portals KESTREL_SOCK=$R/kestrel.sock
unset XDG_CONFIG_HOME XDG_DATA_HOME XDG_CACHE_HOME
KESTREL_ROOT=$H KESTREL_STATE=$R/index.bin "$root/target/release/kestrel-indexd" --no-watch 2>"$R/indexd.log" &
IXD=$!
for _ in $(seq 50); do grep -q indexed "$R/indexd.log" && break; sleep 0.2; done
shot() { # name, kestrel args…
    local name=$1
    shift
    xvfb-run -a -s "-screen 0 1440x900x24" dbus-run-session -- "$root/target/release/kestrel" "$@" \
        --screenshot "$out/$name.png" --delay 9000 >"$R/$name.log" 2>&1 </dev/null
    echo "$name: $([ -s "$out/$name.png" ] && echo ok || echo FAILED)"
}
cd "$H"
# a first visit makes the thumbnails; the real shots then show them straight away
shot warmup "$H/Documents" >/dev/null
rm -f "$out/warmup.png"
shot kestrel --search aurora --select "Invoice 2026-10 Aurora Studio.pdf" --action preview
shot search --search .iso
shot preview "$H/Documents" --select "Kestrel Field Guide.pdf" --action preview
kill $IXD 2>/dev/null
pkill -f "wsdd .*--listen $R/" 2>/dev/null || true
echo "screenshots in docs/screenshots/"
