"""Draws packaging/linux/dev.formalsnake.formalpacer.png and assets/tray.png: uneven frames held level
under a cap.

Run from the repo root: python3 packaging/linux/icon.py
"""

from PIL import Image, ImageDraw, ImageFilter

S = 2048  # drawn at 4x, scaled down for clean edges
OUT = 512


def lerp(a, b, t):
    return tuple(round(x + (y - x) * t) for x, y in zip(a, b))


def squircle_mask(size, box, radius):
    mask = Image.new("L", (size, size), 0)
    ImageDraw.Draw(mask).rounded_rectangle(box, radius=radius, fill=255)
    return mask


img = Image.new("RGBA", (S, S), (0, 0, 0, 0))

box = (200, 200, S - 200, S - 200)
shadow = Image.new("RGBA", (S, S), (0, 0, 0, 0))
ImageDraw.Draw(shadow).rounded_rectangle((box[0], box[1] + 40, box[2], box[3] + 40), radius=420, fill=(40, 20, 90, 110))
img.alpha_composite(shadow.filter(ImageFilter.GaussianBlur(50)))

top, bottom = (124, 140, 255), (88, 48, 196)
grad = Image.new("RGBA", (S, S))
gd = ImageDraw.Draw(grad)
for y in range(S):
    gd.line([(0, y), (S, y)], fill=lerp(top, bottom, y / S) + (255,))
img.paste(grad, (0, 0), squircle_mask(S, box, 420))

glyph = Image.new("RGBA", (S, S), (0, 0, 0, 0))
g = ImageDraw.Draw(glyph)
# Frame times: uneven on the left, then held level under the cap.
heights = [420, 760, 300, 640, 560, 560, 560, 560]
bars, w, gap = len(heights), 110, 62
total = bars * w + (bars - 1) * gap
x0 = (S - total) // 2
base = 1480
for i, h in enumerate(heights):
    x = x0 + i * (w + gap)
    alpha = 150 if i < 4 else 250
    g.rounded_rectangle((x, base - h, x + w, base), radius=w // 2, fill=(255, 255, 255, alpha))
# The cap, just above the held frames.
cap_y = base - 560 - 110
g.rounded_rectangle((x0 + 4 * (w + gap) - 40, cap_y - 28, x0 + total + 60, cap_y + 28), radius=28, fill=(140, 255, 214, 255))
img.alpha_composite(glyph)

img.resize((OUT, OUT), Image.LANCZOS).save("packaging/linux/dev.formalsnake.formalpacer.png")
img.resize((64, 64), Image.LANCZOS).save("assets/tray.png")
