#!/usr/bin/env bash
# Blank documents for New ▸ Word / Excel / PowerPoint / PDF (built into the app with include_bytes!).
# A zero-byte .docx isn't a document: most apps refuse it and previews break, so these are real,
# empty files made by LibreOffice. Re-run only to change them; the results are committed.
#
#   scripts/make-templates.sh
set -eu
cd "$(dirname "$0")/.."
out=$PWD/packaging/templates
W=$(mktemp -d)
trap 'rm -rf "$W"' EXIT
: >"$W/blank.txt"
: >"$W/blank.csv"
cat >"$W/blank.fodp" <<'XML'
<?xml version="1.0" encoding="UTF-8"?>
<office:document xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0"
  xmlns:draw="urn:oasis:names:tc:opendocument:xmlns:drawing:1.0"
  office:version="1.3" office:mimetype="application/vnd.oasis.opendocument.presentation">
 <office:body><office:presentation><draw:page draw:name="Slide1"/></office:presentation></office:body>
</office:document>
XML
lo() { soffice -env:UserInstallation="file://$W/lo" --headless --convert-to "$1" --outdir "$W" "$2" >/dev/null 2>&1; }
lo 'docx:MS Word 2007 XML' "$W/blank.txt"
lo 'pdf:writer_pdf_Export' "$W/blank.txt"
lo 'xlsx:Calc MS Excel 2007 XML' "$W/blank.csv"
lo 'pptx:Impress MS PowerPoint 2007 XML' "$W/blank.fodp"
for e in docx pdf xlsx pptx; do
    cp "$W/blank.$e" "$out/blank.$e"
done
ls -l "$out"
