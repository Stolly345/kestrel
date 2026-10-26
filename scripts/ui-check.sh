#!/usr/bin/env bash
# UI check: drives the real app through its main screens under a virtual X display, with a
# throwaway home folder of demo files, and checks what it can (the app starts and exits cleanly,
# nothing panics or logs a GTK critical, Extract produces files, a second Extract asks about the
# name clash). Every screen is saved as a PNG in target/ui-check/ for a human (or Claude) to look at.
#
#   scripts/ui-check.sh            # all scenarios
#   scripts/ui-check.sh preview    # only scenarios whose name contains "preview"
#
# Needs: xvfb (xvfb-run), dbus-run-session; run as yourself. Search scenarios need the index
# service and are left out: everything here works on the demo folder alone.
set -u
cd "$(dirname "$0")/.."
root=$PWD
only=${1:-}
command -v xvfb-run >/dev/null || { echo "needs xvfb: sudo apt install xvfb"; exit 1; }
cargo build --release -q -p kestrel || exit 1
bin=$root/target/release/kestrel
out=$root/target/ui-check
rm -rf "$out" && mkdir -p "$out"

# a throwaway home: none of your own files or settings appear in the screenshots
H=$(mktemp -d /tmp/kestrel-ui.XXXXXX)
# runtime dir outside the home: GIO shows FUSE mounts under $HOME (gvfs, doc) as drives
R=$(mktemp -d /tmp/kestrel-ui-run.XXXXXX)
trap 'rm -rf "$H" "$R"' EXIT
D=$H/Demo
mkdir -p "$D/Projects/aurora/src" "$H/.config/gtk-4.0" "$H/.local/share/nautilus/scripts"
printf '[Settings]\ngtk-enable-animations=false\n' >"$H/.config/gtk-4.0/settings.ini"
fx=$root/crates/kestrel/tests/fixtures/archives
cp "$fx/sample.zip" "$fx/sample.tar.gz" "$fx/sample.7z" "$D/"
printf '# Aurora\n\nA tiny **weather station** dashboard.\n\n- Reads sensors every *30 s*\n- Charts the last 7 days\n- Alerts on frost\n\n```\ncargo run --release\n```\n' >"$D/notes.md"
printf 'fn main() {\n    println!("aurora");\n}\n' >"$D/Projects/aurora/src/main.rs"
for n in "IMG_0001.txt" "IMG_0002.txt" "IMG_0003.txt"; do echo "$n" >"$D/$n"; done
ln -s Projects/aurora "$D/aurora-link"
printf '#!/bin/sh\necho "$@" > /dev/null\n' >"$H/.local/share/nautilus/scripts/Count lines" && chmod +x "$H/.local/share/nautilus/scripts/Count lines"
python3 -I - "$D" <<'PY'
import struct, sys, zlib
d = sys.argv[1]
# a one-page PDF (offsets computed, so the xref table is valid)
objs = [b"<< /Type /Catalog /Pages 2 0 R >>",
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 595 842] /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> >>",
        None,
        b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>"]
text = b"BT /F1 36 Tf 72 720 Td (Kestrel preview) Tj /F1 18 Tf 0 -48 Td (One page, rendered in the sandbox.) Tj ET"
objs[3] = b"<< /Length %d >>\nstream\n" % len(text) + text + b"\nendstream"
pdf, offs = b"%PDF-1.4\n", []
for i, o in enumerate(objs, 1):
    offs.append(len(pdf))
    pdf += b"%d 0 obj\n" % i + o + b"\nendobj\n"
xref = len(pdf)
pdf += b"xref\n0 %d\n0000000000 65535 f \n" % (len(objs) + 1) + b"".join(b"%010d 00000 n \n" % o for o in offs)
pdf += b"trailer\n<< /Size %d /Root 1 0 R >>\nstartxref\n%d\n%%%%EOF\n" % (len(objs) + 1, xref)
open(f"{d}/report.pdf", "wb").write(pdf)
# a 320x200 gradient PNG
w, h = 320, 200
rows = b"".join(b"\0" + bytes(v for x in range(w) for v in (x * 255 // w, y * 255 // h, 180)) for y in range(h))
chunk = lambda t, b: struct.pack(">I", len(b)) + t + b + struct.pack(">I", zlib.crc32(t + b))
png = b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 2, 0, 0, 0)) + chunk(b"IDAT", zlib.compress(rows)) + chunk(b"IEND", b"")
open(f"{d}/sunrise.png", "wb").write(png)
PY

export HOME=$H XDG_RUNTIME_DIR=$R GDK_BACKEND=x11 GDK_DEBUG=no-portals KESTREL_7ZZ=$root/packaging/7zip/7zz
unset XDG_CONFIG_HOME XDG_DATA_HOME XDG_CACHE_HOME
pass=0 fails=()

# shot NAME [kestrel args…]: open the app, run, save target/ui-check/NAME.png (+ .dialog.png)
shot() {
    local name=$1
    shift
    [ -n "$only" ] && [[ $name != *"$only"* ]] && return 1
    local log=$out/$name.log
    # stdin from /dev/null: timeout runs this in a background process group, and xvfb-run's stty
    # on the terminal would otherwise stop the whole run (SIGTTOU)
    timeout 60 xvfb-run -a -s "-screen 0 1440x900x24" dbus-run-session -- \
        "$bin" "$@" --screenshot "$out/$name.png" --delay "${DELAY:-3000}" >"$log" 2>&1 </dev/null
    local rc=$?
    local why=
    [ $rc -ne 0 ] && why="exit code $rc"
    [ -s "$out/$name.png" ] || why="${why:+$why, }no screenshot"
    grep -qE "panicked|CRITICAL|folder error|action [a-z-]+: " "$log" && why="${why:+$why, }$(grep -m1 -E 'panicked|CRITICAL|folder error|action [a-z-]+: ' "$log")"
    if [ -z "$why" ]; then
        echo "ok   $name"
        pass=$((pass + 1))
    else
        echo "FAIL $name: $why (log: $log)"
        fails+=("$name")
    fi
    return 0
}
# expect NAME CONDITION-DESCRIPTION TEST…: an extra functional check after a scenario
expect() {
    local name=$1 desc=$2
    shift 2
    if "$@"; then echo "ok   $name: $desc"; pass=$((pass + 1)); else echo "FAIL $name: $desc"; fails+=("$name: $desc"); fi
}

shot details "$D"
shot grid "$D" --mode grid
shot columns "$D" --mode columns --select Projects
shot dual "$D" --dual
shot hidden-files "$D" --hidden
shot preview-pdf "$D" --select report.pdf --action preview
shot preview-markdown "$D" --select notes.md --action preview
shot preview-image "$D" --select sunrise.png --action preview
shot preview-archive "$D" --select sample.zip --action preview
shot preview-folder "$D" --select Projects --action preview
shot symlink-status "$D" --select aurora-link
shot properties "$D" --select report.pdf --action properties
shot properties-folder "$D/Projects"  --action properties
shot batch-rename "$D" --select IMG_0001.txt --select IMG_0002.txt --select IMG_0003.txt --action rename
shot palette "$D" --action palette
shot preferences "$D" --action preferences
shot search-syntax "$D" --action syntax
shot terminal-panel "$D/Projects" --action terminal-panel
shot browse-archive "$D" --select sample.zip --action browse-archive
if shot extract "$D" --select sample.zip --action extract; then
    expect extract "sample/hello.txt extracted" test -f "$D/sample/hello.txt"
    if shot extract-conflict "$D" --select sample.zip --action extract; then
        expect extract-conflict "asks Skip / Keep Both / Replace" test -s "$out/extract-conflict.png.dialog.png"
    fi
fi
mkdir -p "$D/tarball" && cp "$fx/sample.tar.gz" "$D/tarball/" # its own folder: sample/ exists by now
if shot extract-tarball "$D/tarball" --select sample.tar.gz --action extract; then
    expect extract-tarball "tarball extracted" test -f "$D/tarball/sample/hello.txt"
fi
if shot trash "$D" --select IMG_0003.txt --action trash; then
    expect trash "IMG_0003.txt moved to the Trash" test -f "$H/.local/share/Trash/files/IMG_0003.txt"
fi

echo "$pass checks passed, ${#fails[@]} failed; screenshots in $out/"
[ ${#fails[@]} -eq 0 ]
