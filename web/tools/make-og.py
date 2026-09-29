#!/usr/bin/env python3
"""Renders web/assets/img/og.png (1200x630), the link-preview card.

Needs Pillow and the Red Hat Display / Source Code Pro fonts that
web/assets/fonts was subset from (Fedora: redhat-display-fonts,
adobe-source-code-pro-fonts).
"""
from pathlib import Path

from PIL import Image, ImageDraw, ImageFilter, ImageFont

OUT = Path(__file__).resolve().parent.parent / "assets" / "img" / "og.png"
DISPLAY = "/usr/share/fonts/redhat/RedHatDisplay-Black.otf"
TEXT = "/usr/share/fonts/redhat/RedHatText-Regular.otf"
MONO = "/usr/share/fonts/adobe-source-code-pro-fonts/SourceCodePro-Semibold.otf"

W, H, S = 1200, 630, 2  # canvas, supersampling
INK, BLUE, HI, AMBER = (5, 15, 26), (46, 155, 255), (143, 208, 255), (255, 181, 71)


def bezier(p0, p1, p2, p3, n=24):
    pts = []
    for i in range(n + 1):
        t = i / n
        a, b, c, d = (1 - t) ** 3, 3 * (1 - t) ** 2 * t, 3 * (1 - t) * t**2, t**3
        pts.append((a * p0[0] + b * p1[0] + c * p2[0] + d * p3[0], a * p0[1] + b * p1[1] + c * p2[1] + d * p3[1]))
    return pts


# the eye of packaging/files/icu.svg, in its 64x64 space
EYE = (bezier((10, 32), (16, 23), (23, 18), (32, 18)) + bezier((32, 18), (41, 18), (48, 23), (54, 32))[1:]
       + bezier((54, 32), (48, 41), (41, 46), (32, 46))[1:] + bezier((32, 46), (23, 46), (16, 41), (10, 32))[1:])


def font(path, size):
    return ImageFont.truetype(path, size * S)


def main():
    img = Image.new("RGB", (W * S, H * S), INK)

    glow = Image.new("RGB", img.size, INK)
    g = ImageDraw.Draw(glow)
    g.ellipse((760 * S, -60 * S, 1340 * S, 520 * S), fill=(24, 92, 160))
    glow = glow.filter(ImageFilter.GaussianBlur(130 * S))
    img = Image.blend(img, glow, 0.9)
    d = ImageDraw.Draw(img, "RGBA")

    # dot grid
    for x in range(0, W, 28):
        for y in range(0, H, 28):
            d.ellipse(((x - 1) * S, (y - 1) * S, (x + 1) * S, (y + 1) * S), fill=(140, 190, 255, 34))

    # sonar rings around the eye
    cx, cy = 900, 315
    for i, r in enumerate((190, 260, 340, 430)):
        d.ellipse(((cx - r) * S, (cy - r) * S, (cx + r) * S, (cy + r) * S), outline=(46, 155, 255, 120 - i * 26), width=2 * S)

    # the eye
    k = 4.6
    ox, oy = cx - 32 * k, cy - 32 * k
    at = lambda x, y: ((ox + x * k) * S, (oy + y * k) * S)
    d.ellipse((*at(2, 2), *at(62, 62)), fill=BLUE)
    d.polygon([at(x, y) for x, y in EYE], fill=INK)
    d.ellipse((*at(24, 24), *at(40, 40)), fill=BLUE)
    d.ellipse((*at(28.5, 28.5), *at(35.5, 35.5)), fill=INK)

    # copy
    x = 80
    d.rounded_rectangle((x * S, 74 * S, (x + 350) * S, 116 * S), radius=21 * S, outline=(130, 185, 255, 70), width=S)
    d.ellipse(((x + 18) * S, 90 * S, (x + 28) * S, 100 * S), fill=(67, 217, 160))
    d.text(((x + 40) * S, 95 * S), "RAUM 01 · 1 SERVER · 0 CHANNELS", font=font(MONO, 15), fill=(147, 170, 193), anchor="lm")
    big = font(DISPLAY, 96)
    d.text((x * S, 150 * S), "Eine Gruppe.", font=big, fill=(232, 241, 251))
    d.text((x * S, 244 * S), "Ein Raum.", font=big, fill=(232, 241, 251))
    d.text((x * S, 338 * S), "Kein Rauschen.", font=big, fill=BLUE)
    d.rounded_rectangle((x * S, 452 * S, (x + 560) * S, 458 * S), radius=3 * S, fill=HI)
    d.text((x * S, 500 * S), "Minimales Voice-System für eine feste Gruppe.", font=font(TEXT, 30), fill=(195, 213, 231))
    d.text((x * S, 566 * S), "icu.uggs.io", font=font(MONO, 26), fill=AMBER, anchor="ls")

    OUT.parent.mkdir(parents=True, exist_ok=True)
    img.resize((W, H), Image.LANCZOS).save(OUT, optimize=True)
    print(OUT)


if __name__ == "__main__":
    main()
