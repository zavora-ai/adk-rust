"""Draws the invoice fixture with invented personal fields and writes its ground truth.

Every name, address, email and phone number on the page is invented. Boxes are the
font's ink boxes from Pillow's `textbbox`, in source pixels (origin top-left, x1 and y1
exclusive). `doc-page-1000x1300.png` beside it comes from the NVIDIA NIM probe's
`make_images.py` (same font, same conventions).

    python3 generate.py   # needs Pillow and the macOS Arial fonts
"""

import json
from pathlib import Path

from PIL import Image, ImageDraw, ImageFont

HERE = Path(__file__).parent
FONTS = Path("/System/Library/Fonts/Supplemental")
W, H = 1000, 1300

regular = ImageFont.truetype(str(FONTS / "Arial.ttf"), 26)
bold = ImageFont.truetype(str(FONTS / "Arial Bold.ttf"), 26)
title = ImageFont.truetype(str(FONTS / "Arial Bold.ttf"), 44)

image = Image.new("RGB", (W, H), "white")
draw = ImageDraw.Draw(image)
objects = []


def text(xy, value, font, label, personal=False):
    draw.text(xy, value, fill=(0, 0, 0), font=font)
    x0, y0, x1, y1 = draw.textbbox(xy, value, font=font)
    objects.append({"label": label, "text": value, "bbox": [x0, y0, x1, y1], "personal": personal})


text((80, 70), "INVOICE INV-4721", title, "title")
text((80, 170), "Bill to", bold, "bill-to-heading")
text((80, 210), "Amara Okafor", regular, "name", personal=True)
text((80, 250), "14 Larch Street, Springfield 62704", regular, "address", personal=True)
text((80, 290), "amara.okafor@example.com", regular, "email", personal=True)
text((80, 330), "+1 415 555 0142", regular, "phone", personal=True)
text((620, 210), "Date: 2026-11-19", regular, "date")
text((620, 250), "Due: 2026-12-19", regular, "due")

# The items table: ruled cells, text 16 px in from each cell's left edge.
top, row_h = 420, 64
cols = [80, 480, 660, 920]
rows = [("Item", "Qty", "Amount"), ("Torque sensor", "2", "24.00"),
        ("Fiducial card", "1", "12.70"), ("Seal kit", "3", "21.50")]
for r in range(len(rows) + 1):
    draw.line([(cols[0], top + r * row_h), (cols[-1], top + r * row_h)], fill="black", width=2)
for c in cols:
    draw.line([(c, top), (c, top + len(rows) * row_h)], fill="black", width=2)
for r, row in enumerate(rows):
    for c, value in enumerate(row):
        font = bold if r == 0 else regular
        text((cols[c] + 16, top + r * row_h + 18), value, font, f"cell r{r}c{c}")
objects.append({"label": "table", "text": None, "bbox": [cols[0], top, cols[-1] + 1, top + len(rows) * row_h + 1], "personal": False})

text((660, top + len(rows) * row_h + 30), "Total: 58.20", bold, "total")
text((80, 1230), "Thank you for your order.", regular, "footer")

image.save(HERE / "invoice-personal-1000x1300.png", optimize=True)
(HERE / "invoice-personal-1000x1300.groundtruth.json").write_text(json.dumps({
    "image": "invoice-personal-1000x1300.png",
    "width": W,
    "height": H,
    "frame": "source pixels, origin top-left, x1/y1 exclusive",
    "note": "Every personal field is invented.",
    "objects": objects,
}, indent=1) + "\n")
