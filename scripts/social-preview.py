#!/usr/bin/env python3
"""GitHub social preview (1280x640): the icon, the name, one promise and the app in dark mode.

    python3 scripts/social-preview.py      # writes docs/social-preview.png

Upload it by hand (GitHub has no API for it): repo Settings > General > Social preview > Edit.
Reddit and Mastodon link cards show it. Rerun after docs/screenshots/kestrel-dark.png (made by
scripts/screenshots.sh) or the wording changes. The card only promotes the app: no URL, no
version, no tech stack, no other product's name, so it never goes stale.
Colours are Kestrel's dark Vantage tokens (crates/kestrel/src/style-dark.css), the face is the
bundled Geologica. Needs python3-pil and python3-gi (GdkPixbuf reads the SVG icon).
"""
import io, os
import gi
gi.require_version("GdkPixbuf", "2.0")
from gi.repository import GdkPixbuf
from PIL import Image, ImageDraw, ImageFilter, ImageFont

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
W, H = 1280, 640
FONT = f"{ROOT}/packaging/fonts/Geologica.ttf"
# style-dark.css: --color-paper, paper-3, ink, muted, lime (oklch, converted to sRGB)
PAPER, PAPER3, INK, MUTED, LIME = (4, 9, 12), (17, 26, 31), (235, 239, 242), (137, 148, 154), (188, 240, 83)


def font(size, weight):
    f = ImageFont.truetype(FONT, size)
    f.set_variation_by_axes([weight, 0, 0, 0])  # weight, cursive, sharpness, slant
    return f


def svg_png(path, size):
    pb = GdkPixbuf.Pixbuf.new_from_file_at_size(path, size, size)
    ok, data = pb.save_to_bufferv("png", [], [])
    return Image.open(io.BytesIO(data)).convert("RGBA")


def rounded(img, r):
    m = Image.new("L", img.size, 0)
    ImageDraw.Draw(m).rounded_rectangle([0, 0, img.width - 1, img.height - 1], r, fill=255)
    out = img.convert("RGBA")
    out.putalpha(m)
    return out


def glow(cx, cy, r, colour, alpha):
    """A soft pool of colour, blurred so it has no edge."""
    g = Image.new("RGBA", (W, H), (0, 0, 0, 0))
    ImageDraw.Draw(g).ellipse([cx - r, cy - r, cx + r, cy + r], fill=(*colour, alpha))
    return g.filter(ImageFilter.GaussianBlur(r * 0.6))


bg = Image.new("RGBA", (W, H), (*PAPER, 255))
bg = Image.alpha_composite(bg, glow(980, 360, 340, LIME, 46))
bg = Image.alpha_composite(bg, glow(160, 640, 300, PAPER3, 255))

# the app, tilted toward the reader by size alone: big, running off the right and bottom edges
shot = Image.open(f"{ROOT}/docs/screenshots/kestrel-dark.png").convert("RGBA")
sw = 820
shot = shot.resize((sw, round(shot.height * sw / shot.width)), Image.LANCZOS)
x, y = W - sw + 70, 112
frame = Image.new("RGBA", (sw + 2, shot.height + 2), (0, 0, 0, 0))
ImageDraw.Draw(frame).rounded_rectangle([0, 0, sw + 1, shot.height + 1], 14, fill=(*MUTED, 90))
shadow = Image.new("RGBA", (W, H), (0, 0, 0, 0))
ImageDraw.Draw(shadow).rounded_rectangle([x, y + 18, x + sw, y + shot.height + 18], 14, fill=(0, 0, 0, 200))
bg = Image.alpha_composite(bg, shadow.filter(ImageFilter.GaussianBlur(26)))
bg.alpha_composite(rounded(frame, 15), (x - 1, y - 1))
bg.alpha_composite(rounded(shot, 14), (x, y))

# the words: name, one promise, one line of what it is
d = ImageDraw.Draw(bg)
L = 72
bg.alpha_composite(svg_png(f"{ROOT}/packaging/dev.kestrel.Kestrel.svg", 104), (L - 6, 92))
d.text((L, 214), "Kestrel", font=font(104, 650), fill=INK)
d.text((L + 2, 352), "Find any file", font=font(38, 500), fill=INK)
d.text((L + 2, 398), "the moment you type.", font=font(38, 500), fill=LIME)
d.text((L + 2, 486), "A fast, friendly file manager", font=font(25, 400), fill=MUTED)
d.text((L + 2, 520), "with whole-disk search built in.", font=font(25, 400), fill=MUTED)

out = f"{ROOT}/docs/social-preview.png"
bg.convert("RGB").save(out, optimize=True)
print("wrote", out)
