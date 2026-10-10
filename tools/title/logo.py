import random, sys
from PIL import Image, ImageDraw, ImageFont, ImageFilter, ImageChops, ImageOps
FONT = r'C:\Windows\Fonts\BRLNSB.TTF'
RED, GREEN, YELLOW, BLUE = (226, 30, 34), (40, 170, 60), (250, 200, 20), (30, 80, 220)
WOOD, WOOD_DARK = (196, 132, 70), (110, 62, 28)
def glyph(ch, size, angle):
    f = ImageFont.truetype(FONT, size)
    l, t, r, b = f.getbbox(ch)
    pad = size // 4
    m = Image.new('L', (r - l + 2 * pad, b - t + 2 * pad), 0)
    ImageDraw.Draw(m).text((pad - l, pad - t), ch, font=f, fill=255)
    m = m.rotate(angle, resample=Image.BICUBIC, expand=True)
    return m.crop(m.getbbox())
def face(mask, color, size):
    # bevel: light from the top left on a blurred height field
    r = max(2, size // 34)
    h = mask.filter(ImageFilter.GaussianBlur(r))
    d = max(1, r // 2)
    a = ImageChops.offset(h, d, d); bb = ImageChops.offset(h, -d, -d)
    light = ImageChops.subtract(a, bb, scale=1.0, offset=128)   # >128 lit, <128 shaded
    light = light.point(lambda v: max(0, min(255, int(128 + (v - 128) * 1.3))))
    base = Image.new('RGB', mask.size, color)
    # top-to-bottom gradient: lighter top
    grad = Image.linear_gradient('L').resize(mask.size).point(lambda v: 255 - v // 3)
    base = ImageChops.multiply(base, Image.merge('RGB', [grad] * 3))
    hi = light.point(lambda v: int(max(0, v - 128) * 1.1))
    lo = light.point(lambda v: 255 - int(max(0, 128 - v) * 1.2))
    base = ImageChops.multiply(base, Image.merge('RGB', [lo] * 3))
    base = ImageChops.screen(base, Image.merge('RGB', [hi] * 3))
    return base
def wood(size):
    random.seed(64)
    w, h = size
    img = Image.new('RGB', (w, h), WOOD)
    d = ImageDraw.Draw(img)
    for y in range(0, h, 3):
        k = random.uniform(0.75, 1.1)
        c = tuple(int(v * k) for v in WOOD)
        d.line([(0, y), (w, y + random.randint(-6, 6))], fill=c, width=random.choice((2, 3, 4)))
    return img.filter(ImageFilter.GaussianBlur(1))
def word(text, colors, size, angles, lift, spacing):
    gs = [glyph(c, size, a) for c, a in zip(text, angles)]
    W = sum(g.width for g in gs) + spacing * (len(gs) - 1)
    H = max(g.height for g in gs) + max(lift) + 4
    out = []
    x = 0
    for g, col, up in zip(gs, colors, lift):
        out.append((g, col, x, H - g.height - up))
        x += g.width + spacing
    return out, W, H
def render(parts, W, H, size, depth):
    # parts: (glyph mask, colour, x, y) in canvas coordinates
    canvas = Image.new('RGBA', (W, H), (0, 0, 0, 0))
    full = Image.new('L', (W, H), 0)
    for g, _, x, y in parts:
        for k in range(depth + 1):
            full.paste(255, (x + k // 2, y + k), g)
    # extrusion (wood), drawn back to front per letter so nearer faces cover
    wd = wood((W, H))
    for g, col, x, y in parts:
        side = Image.new('L', (W, H), 0)
        for k in range(depth, 0, -1):
            side.paste(255, (x + k // 2, y + k), g)
        shade = Image.new('L', (W, H), 0)
        for k in range(depth, 0, -1):
            v = int(255 * (0.55 + 0.45 * (1 - k / depth)))
            shade.paste(v, (x + k // 2, y + k), g)
        s = ImageChops.multiply(wd, Image.merge('RGB', [shade] * 3))
        canvas.paste(s, (0, 0), side)
        m = Image.new('L', (W, H), 0); m.paste(g, (x, y))
        f = Image.new('RGB', (W, H)); f.paste(face(g, col, size), (x, y))
        # thin dark rim between face and side
        rim = m.filter(ImageFilter.MaxFilter(3))
        canvas.paste((40, 20, 10), (0, 0), ImageChops.subtract(rim, m))
        canvas.paste(f, (0, 0), m)
    # outline around everything
    o = max(3, size // 40) | 1
    out = full.filter(ImageFilter.MaxFilter(o))
    bg = Image.new('RGBA', (W, H), (25, 12, 5, 0)); bg.putalpha(out)
    bg.alpha_composite(canvas)
    return bg
def logo(scale=2):
    s = scale
    pad = 40 * s
    # SUPER (small, top left), SEKIRO (big), 64 (top right), like Super Mario 64
    sup, sw, sh = word('SUPER', [RED, YELLOW, BLUE, GREEN, RED], 200 * s, [8, 4, 0, -4, -8], [0, 10*s, 16*s, 10*s, 0], 6 * s)
    sek, kw, kh = word('SEKIRO', [BLUE, RED, YELLOW, GREEN, RED, YELLOW], 330 * s, [10, 5, 2, -2, -5, -10], [0, 18*s, 28*s, 28*s, 18*s, 0], 4 * s)
    six, xw, xh = word('64', [RED, YELLOW], 220 * s, [6, 2], [0, 8*s], 2 * s)
    depth = 20 * s
    W = kw + 2 * pad + depth
    top_y = pad
    sek_y = top_y + int(sh * 0.78)
    H = sek_y + kh + depth + pad
    parts = []
    sx = pad + int(kw * 0.06)
    parts += [(g, c, sx + x, top_y + y) for g, c, x, y in sup]
    xx = pad + kw - xw - int(kw * 0.02)
    parts += [(g, c, xx + x, top_y + y + int(sh * 0.08)) for g, c, x, y in six]
    parts += [(g, c, pad + x, sek_y + y) for g, c, x, y in sek]
    img = render(parts, W, H, 300 * s, depth)
    return img.crop(img.getbbox())
if __name__ == '__main__':
    im = logo()
    im.save(sys.argv[1] if len(sys.argv) > 1 else 'logo.png')
    print(im.size)
