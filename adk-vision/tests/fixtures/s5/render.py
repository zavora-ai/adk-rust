#!/usr/bin/env python3
"""Renders s5-screen.png: the twelve S5 targets of s5.json drawn as filled, labelled
widgets on a 3024x1964 screenshot, so live providers answer for pixels whose truth is
known exactly (spec 3.7 S5).

Each target fills [x0, x1) x [y0, y1) with its own colour and carries its name in white;
the background is light grey. Requires Pillow.

Run: python3 render.py
"""

import json
from pathlib import Path

from PIL import Image, ImageDraw, ImageFont

FIXTURE = json.loads(Path(__file__).with_name("s5.json").read_text())
W, H = FIXTURE["image"]["width"], FIXTURE["image"]["height"]
TARGETS = [(t["name"], *t["box"]) for t in FIXTURE["targets"]]

COLOURS = [
    (31, 119, 180),
    (214, 39, 40),
    (44, 160, 44),
    (148, 103, 189),
    (140, 86, 75),
    (227, 119, 194),
    (127, 127, 127),
    (23, 190, 207),
    (188, 128, 34),
    (0, 90, 50),
    (60, 60, 160),
    (200, 80, 0),
]

image = Image.new("RGB", (W, H), (236, 236, 236))
draw = ImageDraw.Draw(image)
for (name, x0, y0, x1, y1), colour in zip(TARGETS, COLOURS):
    # PIL's rectangle includes its far edge; the targets' x1 and y1 are exclusive.
    draw.rectangle([x0, y0, x1 - 1, y1 - 1], fill=colour)
    text = name.replace("-", " ").upper()
    # The largest size up to 36 px whose text fits in 80% of the widget's width.
    size = 36
    while True:
        font = ImageFont.load_default(size=size)
        left, top, right, bottom = draw.textbbox((0, 0), text, font=font)
        if right - left <= 0.8 * (x1 - x0) or size <= 12:
            break
        size -= 2
    tx = x0 + (x1 - x0 - (right - left)) / 2 - left
    ty = y0 + (y1 - y0 - (bottom - top)) / 2 - top
    draw.text((tx, ty), text, fill=(255, 255, 255), font=font)

out = Path(__file__).with_name("s5-screen.png")
image.save(out, optimize=True)
print(f"wrote {out} ({out.stat().st_size} bytes)")
