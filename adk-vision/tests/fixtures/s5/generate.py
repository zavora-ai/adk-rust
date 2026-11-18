#!/usr/bin/env python3
"""Writes s5.json: twelve labelled targets on a 3024x1964 screenshot of a 1512x982-point
display at device pixel ratio 2, and each provider's answer for them in its own frame,
quantised as the provider quantises it (spec 3.7 S5).

- Gemini: box_2d as [ymin, xmin, ymax, xmax], integers on 0-1000 of the image as sent.
- Claude: integer pixels of the image it processed; the runtime resized it to 1568 px on
  the long edge first.
- sam-3.1: integer source pixels.
- RF-DETR: model-input pixels of a 640x640 tensor, the image letterboxed in the centre,
  to two decimals.

Run: python3 generate.py > s5.json
"""

import json

W, H = 3024, 1964
TARGETS = [
    ("menu", 24, 18, 220, 86),
    ("search", 980, 30, 1880, 110),
    ("avatar", 2860, 20, 2990, 112),
    ("name-field", 400, 300, 1300, 380),
    ("email-field", 400, 430, 1300, 510),
    ("phone-field", 400, 560, 1300, 640),
    ("country", 1500, 300, 2100, 380),
    ("notes", 400, 700, 2100, 1100),
    ("cancel", 1660, 1180, 1880, 1260),
    ("save", 1920, 1180, 2100, 1260),
    ("help", 40, 1880, 160, 1950),
    ("status", 2500, 1880, 2990, 1950),
]

# Claude: the runtime downscales to 1568 px on the long edge, rounding each side.
CW = 1568
CH = round(H * CW / W)

# RF-DETR: centred letterbox into 640x640; the resized size is rounded and the scale
# follows it.
IW = IH = 640
s = min(IW / W, IH / H)
RW, RH = round(W * s), round(H * s)
PAD_X, PAD_Y = (IW - RW) // 2, (IH - RH) // 2
SX, SY = RW / W, RH / H


def gemini(x0, y0, x1, y1):
    return [round(y0 / H * 1000), round(x0 / W * 1000), round(y1 / H * 1000), round(x1 / W * 1000)]


def claude(x0, y0, x1, y1):
    return [round(x0 * CW / W), round(y0 * CH / H), round(x1 * CW / W), round(y1 * CH / H)]


def rfdetr(x0, y0, x1, y1):
    return [round(x0 * SX + PAD_X, 2), round(y0 * SY + PAD_Y, 2), round(x1 * SX + PAD_X, 2), round(y1 * SY + PAD_Y, 2)]


out = {
    "image": {"width": W, "height": H, "display": "built-in", "devicePixelRatio": 2.0},
    "targets": [{"name": n, "box": [x0, y0, x1, y1]} for n, x0, y0, x1, y1 in TARGETS],
    "gemini": [{"name": n, "box_2d": gemini(x0, y0, x1, y1)} for n, x0, y0, x1, y1 in TARGETS],
    "claude": {
        "sent": [CW, CH],
        "answers": [{"name": n, "box": claude(x0, y0, x1, y1)} for n, x0, y0, x1, y1 in TARGETS],
    },
    "sam": [{"name": n, "box": [x0, y0, x1, y1]} for n, x0, y0, x1, y1 in TARGETS],
    "rfdetr": {
        "input": [IW, IH],
        "answers": [{"name": n, "box": rfdetr(x0, y0, x1, y1)} for n, x0, y0, x1, y1 in TARGETS],
    },
}
print(json.dumps(out, indent=2))
