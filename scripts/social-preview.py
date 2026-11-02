#!/usr/bin/env python3
"""GitHub social preview (1280x640): icon, name, tagline and the README screenshot.

    python3 scripts/social-preview.py      # writes docs/social-preview.png

Upload it by hand (GitHub has no API for it): repo Settings > General > Social preview > Edit.
Reddit and Mastodon link cards show it. Rerun after the screenshot or the tagline changes.
Needs python3-pil, python3-gi-cairo, gir1.2-rsvg-2.0 and the Ubuntu font (all in Ubuntu by default).
"""
import io, os
import cairo, gi
gi.require_version("Rsvg", "2.0")
from gi.repository import Rsvg
from PIL import Image, ImageDraw, ImageFilter, ImageFont

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
W, H = 1280, 640
FONT = "/usr/share/fonts/truetype/ubuntu/Ubuntu[wdth,wght].ttf"


def font(size, weight):
    f = ImageFont.truetype(FONT, size)
    try:
        f.set_variation_by_axes([100, weight])
    except Exception:
        pass
    return f


def svg_png(path, size):
    h = Rsvg.Handle.new_from_file(path)
    surf = cairo.ImageSurface(cairo.FORMAT_ARGB32, size, size)
    cr = cairo.Context(surf)
    vp = Rsvg.Rectangle()
    vp.x, vp.y, vp.width, vp.height = 0, 0, size, size
    h.render_document(cr, vp)
    buf = io.BytesIO()
    surf.write_to_png(buf)
    return Image.open(io.BytesIO(buf.getvalue())).convert("RGBA")


def gradient(top, bottom):
    g = Image.new("RGB", (1, H))
    for y in range(H):
        t = y / (H - 1)
        g.putpixel((0, y), tuple(int(a + (b - a) * t) for a, b in zip(top, bottom)))
    return g.resize((W, H))


def rounded(img, r):
    m = Image.new("L", img.size, 0)
    ImageDraw.Draw(m).rounded_rectangle([0, 0, img.width - 1, img.height - 1], r, fill=255)
    out = img.convert("RGBA")
    out.putalpha(m)
    return out


def card(name, icon, tagline, points, shot, top, bottom, accent, text, sub, file):
    bg = gradient(top, bottom).convert("RGBA")
    # screenshot: right side, running off the bottom-right edge
    s = Image.open(shot).convert("RGBA")
    sw = 760
    s = rounded(s.resize((sw, round(s.height * sw / s.width)), Image.LANCZOS), 14)
    x, y = W - sw + 40, 120
    shadow = Image.new("RGBA", (W, H), (0, 0, 0, 0))
    ImageDraw.Draw(shadow).rounded_rectangle([x, y + 10, x + sw, y + s.height + 10], 14, fill=(0, 0, 0, 120))
    bg = Image.alpha_composite(bg, shadow.filter(ImageFilter.GaussianBlur(18)))
    bg.alpha_composite(s, (x, y))
    d = ImageDraw.Draw(bg)
    # left column
    ic = svg_png(icon, 112)
    bg.alpha_composite(ic, (64, 96))
    d.text((64, 228), name, font=font(82, 700), fill=text)
    yy = 330
    for line in tagline:
        d.text((66, yy), line, font=font(31, 500), fill=text)
        yy += 42
    yy += 22
    for p in points:
        d.ellipse([68, yy + 11, 80, yy + 23], fill=accent)
        d.text((94, yy), p, font=font(25, 400), fill=sub)
        yy += 38
    d.text((66, H - 58), "github.com/Stolly345/" + name.lower(), font=font(22, 500), fill=sub)
    bg.convert("RGB").save(file, optimize=True)
    print("wrote", file)

card("Kestrel", f"{ROOT}/packaging/dev.kestrel.Kestrel.svg",
     ["Instant whole-disk search,", "built into a file manager"],
     ["Everything-style search for Linux", "GTK4 · libadwaita · Rust", "Searches your NAS too"],
     f"{ROOT}/docs/screenshots/kestrel.png",
     (255, 244, 236), (250, 222, 204), (230, 97, 0), (40, 28, 22), (95, 75, 64), f"{ROOT}/docs/social-preview.png")
