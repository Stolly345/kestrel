"""Write packaging/THIRD-PARTY-LICENSES.txt: the licence texts of every crate compiled into
kestrel, kestrel-indexd and ks (plus the C libgit2 that libgit2-sys builds in), as their MIT/Apache
licences require for binary distribution. Identical texts are printed once with every crate
that uses them. Run from anywhere: python3 scripts/third-party-licenses.py
"""
import hashlib, json, os, pathlib, subprocess, sys

root = pathlib.Path(__file__).resolve().parent.parent
out = root / "packaging" / "THIRD-PARTY-LICENSES.txt"
NAMES = ("LICENSE", "LICENCE", "COPYING", "NOTICE", "UNLICENSE", "COPYRIGHT")


def cargo(*args):
    return subprocess.run(["cargo", *args], cwd=root, check=True, capture_output=True, text=True).stdout


# crates linked into the shipped binaries (normal dependencies only, no build tools or tests)
linked = set()
for binary in ("kestrel", "kestrel-indexd", "ks"):
    for line in cargo("tree", "-p", binary, "-e", "normal", "--prefix", "none", "-f", "{p}").splitlines():
        name, version = line.split()[:2]
        linked.add((name, version.lstrip("v")))
meta = json.loads(cargo("metadata", "--format-version", "1"))
workspace = {p["name"] for p in meta["packages"] if p["source"] is None}

texts = {}  # sha -> (text, [crate labels])
missing, crates = [], 0
for p in sorted(meta["packages"], key=lambda p: (p["name"], p["version"])):
    if (p["name"], p["version"]) not in linked or p["name"] in workspace:
        continue
    crates += 1
    label = f'{p["name"]} {p["version"]} ({p.get("license") or "see text"})'
    d = pathlib.Path(p["manifest_path"]).parent
    files = sorted(f for f in d.iterdir() if f.is_file() and f.name.upper().startswith(NAMES))
    if p["name"] == "libgit2-sys" and (d / "libgit2" / "COPYING").exists():
        files.append(d / "libgit2" / "COPYING")  # the C library compiled into the binary
    if not files:
        missing.append(label)
    for f in files:
        text = f.read_text(errors="replace").strip()
        key = hashlib.sha256(text.encode()).hexdigest()
        texts.setdefault(key, (text, []))[1].append(label if f.parent == d else f"{label}: bundled C libgit2")

with open(out, "w") as o:
    o.write("Third-party software compiled into Kestrel\n")
    o.write("==========================================\n\n")
    o.write("Kestrel itself is MIT OR Apache-2.0 (LICENSE-MIT, LICENSE-APACHE). The bundled 7-Zip has\n")
    o.write("its own licence in 7zip-License.txt. Below: every Rust crate (and the C libgit2) built into\n")
    o.write(f"the kestrel, kestrel-indexd and ks binaries, {crates} crates in all, with their licence texts.\n")
    if missing:
        o.write("\nNo licence file shipped in the crate (licence per its Cargo.toml):\n")
        o.writelines(f"  {m}\n" for m in missing)
    for text, users in sorted(texts.values(), key=lambda t: t[1][0]):
        o.write("\n" + "-" * 78 + "\n")
        o.writelines(f"{u}\n" for u in users)
        o.write("-" * 78 + "\n\n" + text + "\n")
print(f"{out.relative_to(root)}: {len(texts)} distinct texts, {len(missing)} crates without a licence file")
