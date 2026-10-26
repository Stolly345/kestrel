# kestrel-512.png -> folder.png (grey shading + alpha: the part to tint) and bird.png (the rest, untouched)
import sys, colorsys
from collections import deque
from PIL import Image
src, outdir = sys.argv[1:]
im = Image.open(src).convert("RGBA")
w, h = im.size
px = im.load()
def folderish(p):
    r, g, b, a = p
    if a < 20:
        return False
    hh, l, s = colorsys.rgb_to_hls(r / 255, g / 255, b / 255)
    return 0.095 <= hh <= 0.135 and s > 0.55 and l < 0.62
# flood from the folder's outer ring inward; the beak/eye-ring yellows aren't connected to it
seen = [[False] * h for _ in range(w)]
q = deque()
for x in range(w):
    for y in range(h):
        if px[x, y][3] > 20 and folderish(px[x, y]):
            # seed: first opaque folder pixel scanning down each column
            q.append((x, y)); seen[x][y] = True
            break
while q:
    x, y = q.popleft()
    for nx, ny in ((x + 1, y), (x - 1, y), (x, y + 1), (x, y - 1)):
        if 0 <= nx < w and 0 <= ny < h and not seen[nx][ny] and folderish(px[nx, ny]):
            seen[nx][ny] = True
            q.append((nx, ny))
folder = Image.new("LA", (w, h))
bird = Image.new("RGBA", (w, h))
fp, bp = folder.load(), bird.load()
for x in range(w):
    for y in range(h):
        r, g, b, a = px[x, y]
        if seen[x][y]:
            # luminance relative to the front face (240,185,26): 128 = face colour, darker/lighter scale it
            lum = 0.299 * r + 0.587 * g + 0.114 * b
            fp[x, y] = (max(0, min(255, round(lum / 182.3 * 128))), a)
        else:
            bp[x, y] = (r, g, b, a)
folder.save(f"{outdir}/folder.png", optimize=True)
bird.save(f"{outdir}/bird.png", optimize=True)
