# Archive samples

One sample per extension Kestrel offers Extract on (`fileops::ARCHIVE_EXTS`), used by the
`every_format_extracts` test. Built by `scripts/make-archive-fixtures.sh`; re-run it to add a
format or rebuild them. Most hold `hello.txt` ("Hello from Kestrel") and `sub/data.txt`.

Built here with Linux tools, except:

| Sample | Source | Licence |
|---|---|---|
| sample.lzh, sample.lha | libarchive v3.8.10, `test_read_format_lha_lh6.lzh.uu` | BSD-2-Clause |
| oldrar.rar, .r00, .r01 | libarchive v3.8.10, `test_rar_multivolume_single_file.part1-3.rar.uu`, relabelled to old-style naming by `scripts/fixtures/oldrar.py` | BSD-2-Clause |
| sample.chm | PuTTY 0.83 manual (`putty.chm`) | MIT |
| sample.dmg | `scripts/fixtures/udif.py` (no Linux tool writes UDIF) | — |
| sample.hfs, sample.hfsx | HFS+ volume cut from an `xorriso -hfsplus` hybrid ISO by `scripts/fixtures/hfsplus.py` | — |
| sample.msp | copy of sample.msi (same OLE compound-file container; no Linux tool writes patches) | — |
