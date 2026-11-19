#!/usr/bin/env python3
"""Draws scene-1024x640.png: COCO-class objects a closed-set detector knows, at known boxes.

A stop sign on a post and a wall clock on a plain wall, drawn with Pillow, so detection
accuracy is measured on an image the project owns. The boxes printed (and written by
generate.py) are the drawn geometry, `x1`/`y1` exclusive. Run from anywhere:

    python3 adk-vision/tests/fixtures/eval/scene.py
"""

import json
import math
from pathlib import Path

from PIL import Image, ImageDraw, ImageFont

HERE = Path(__file__).resolve().parent
W, H = 1024, 640

FONTS = [
    "/System/Library/Fonts/Supplemental/Arial Bold.ttf",
    "/usr/share/fonts/truetype/dejavu/DejaVuSans-Bold.ttf",
]


def font(size):
    for path in FONTS:
        if Path(path).exists():
            return ImageFont.truetype(path, size)
    return ImageFont.load_default()


def octagon(cx, cy, r):
    return [(cx + r * math.cos(math.radians(22.5 + 45 * i)), cy + r * math.sin(math.radians(22.5 + 45 * i)))
            for i in range(8)]


def draw():
    image = Image.new("RGB", (W, H), (214, 208, 196))
    d = ImageDraw.Draw(image)
    # Ground.
    d.rectangle([0, 470, W, H], fill=(128, 124, 116))

    # Stop sign: post, white rim, red face, lettering.
    cx, cy, r = 260, 250, 150
    d.rectangle([cx - 9, cy, cx + 9, 600], fill=(105, 105, 110))
    d.polygon(octagon(cx, cy, r), fill=(245, 245, 245))
    d.polygon(octagon(cx, cy, r - 12), fill=(196, 22, 28))
    text = "STOP"
    f = font(84)
    left, top, right, bottom = d.textbbox((0, 0), text, font=f)
    d.text((cx - (right - left) / 2 - left, cy - (bottom - top) / 2 - top), text, font=f, fill=(250, 250, 250))
    rim = octagon(cx, cy, r)
    stop = [min(x for x, _ in rim), min(y for _, y in rim), max(x for x, _ in rim), max(y for _, y in rim)]

    # Wall clock: rim, face, hour marks, hands, hub.
    kx, ky, kr = 740, 230, 130
    d.ellipse([kx - kr, ky - kr, kx + kr, ky + kr], fill=(40, 40, 44))
    d.ellipse([kx - kr + 14, ky - kr + 14, kx + kr - 14, ky + kr - 14], fill=(250, 250, 246))
    numerals = font(30)
    for hour in range(1, 13):
        a = math.radians(hour * 30 - 90)
        inner, outer = kr - 34, kr - 20
        d.line([kx + inner * math.cos(a), ky + inner * math.sin(a), kx + outer * math.cos(a), ky + outer * math.sin(a)],
               fill=(30, 30, 30), width=5)
        label = str(hour)
        left, top, right, bottom = d.textbbox((0, 0), label, font=numerals)
        tx, ty = kx + (kr - 58) * math.cos(a), ky + (kr - 58) * math.sin(a)
        d.text((tx - (right - left) / 2 - left, ty - (bottom - top) / 2 - top), label, font=numerals, fill=(30, 30, 30))
    for angle, length, width in [(-30, kr - 70, 9), (60, kr - 40, 6)]:
        a = math.radians(angle - 90)
        d.line([kx, ky, kx + length * math.cos(a), ky + length * math.sin(a)], fill=(20, 20, 20), width=width)
    d.ellipse([kx - 9, ky - 9, kx + 9, ky + 9], fill=(180, 30, 30))
    clock = [kx - kr, ky - kr, kx + kr + 1, ky + kr + 1]

    image.save(HERE / "scene-1024x640.png", optimize=True)
    boxes = {"stop sign": [round(v, 1) for v in stop], "clock": clock}
    (HERE / "scene-1024x640.groundtruth.json").write_text(json.dumps({
        "image": "scene-1024x640.png",
        "width": W,
        "height": H,
        "frame": "source pixels, origin top-left, x right, y down, x1/y1 exclusive",
        "note": "Drawn by scene.py; every object is generated.",
        "objects": [{"label": label, "bbox": box} for label, box in boxes.items()],
    }, indent=2) + "\n")
    return boxes


if __name__ == "__main__":
    print(json.dumps(draw()))
