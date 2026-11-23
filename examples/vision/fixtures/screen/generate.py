#!/usr/bin/env python3
"""Draws the Screen copilot's desktop for spec 3.7 S14 and writes its layout.

The display is 1512x982 points at device pixel ratio 2, so every screenshot is 3024x1964
pixels. Three windows are open: the browser showing the CRM, the desktop customer form the
record is copied into, and a mail window the copilot may not touch. The customer form has two
identical "Save" buttons, one under its notes and one in its footer: the seeded
mis-grounding of S14.

`desktop.png` is the screen before anything is typed. The stand-in desktop draws typed text,
and the footer's "Customer saved", from `glyphs.png`, a strip of the printable ASCII glyphs,
so it can show whatever the copilot types. `layout.json` holds the windows and widgets in
screenshot pixels and the glyph strip's cells.

Every name, address, phone number and account number is invented; the phone number is in
Ofcom's range for drama. Text is Pillow's bundled Aileron Regular (CC0).

    python3 generate.py   # needs Pillow 10.1 or later
"""

import json
from pathlib import Path

from PIL import Image, ImageDraw, ImageFont

HERE = Path(__file__).parent
RATIO = 2
POINTS = (1512, 982)
SIZE = (POINTS[0] * RATIO, POINTS[1] * RATIO)

TEXT = ImageFont.load_default(size=30)
TITLE = ImageFont.load_default(size=26)
HEADING = ImageFont.load_default(size=36)

DESKTOP = (58, 86, 112)
WINDOW = (250, 250, 250)
TITLE_BAR = (224, 224, 224)
INK = (20, 20, 20)
PLACEHOLDER = (128, 128, 128)
BORDER = (150, 150, 150)
BUTTON = (232, 236, 244)
BUTTON_BORDER = (110, 120, 140)

# The record the CRM shows, as the browser window draws it (the CRM serves the same values).
RECORD = [
    ("Name", "Amara Okafor"),
    ("Email", "amara.okafor@example.com"),
    ("Phone", "+44 20 7946 0958"),
    ("Company", "Larch Street Market"),
    ("Account", "ACC-104233"),
]
# The form's fields: label, placeholder.
FIELDS = [
    ("name", "Name", "Full name"),
    ("email", "Email", "Email address"),
    ("phone", "Phone", "Phone number"),
    ("company", "Company", "Company name"),
    ("account", "Account", "Account number"),
]


def px(box):
    return [v * RATIO for v in box]


image = Image.new("RGB", SIZE, DESKTOP)
draw = ImageDraw.Draw(image)
windows, widgets = [], []


def window(title, app, frame):
    x0, y0, x1, y1 = px(frame)
    draw.rectangle([x0, y0, x1 - 1, y1 - 1], fill=WINDOW, outline=BORDER, width=2)
    draw.rectangle([x0, y0, x1 - 1, y0 + 56 - 1], fill=TITLE_BAR, outline=BORDER, width=2)
    draw.text(((x0 + x1) / 2, y0 + 28), title, fill=INK, font=TITLE, anchor="mm")
    windows.append({"title": title, "app": app, "frame": [x0, y0, x1, y1]})


def text(xy, value, font=TEXT, fill=INK):
    draw.text(px(xy), value, fill=fill, font=font, anchor="lm")


def button(wid, window_title, label, frame):
    x0, y0, x1, y1 = px(frame)
    draw.rectangle([x0, y0, x1 - 1, y1 - 1], fill=BUTTON, outline=BUTTON_BORDER, width=2)
    draw.text(((x0 + x1) / 2, (y0 + y1) / 2), label, fill=INK, font=TEXT, anchor="mm")
    widgets.append(
        {"id": wid, "window": window_title, "kind": "button", "text": label, "box": [x0, y0, x1, y1]}
    )


# The menu bar.
draw.rectangle([0, 0, SIZE[0] - 1, 24 * RATIO - 1], fill=(236, 236, 236))

# The browser, showing the customer's record in the CRM.
BROWSER = "Larch CRM - Chromium"
window(BROWSER, "org.chromium.Chromium", (8, 34, 744, 700))
text((24, 92), "Larch CRM", HEADING)
text((24, 140), "Customers / C-1042", TITLE, PLACEHOLDER)
for i, (label, value) in enumerate(RECORD):
    y = 190 + i * 48
    text((24, y), label, TEXT, PLACEHOLDER)
    text((150, y), value)

# The desktop form the record is copied into.
FORM = "Customer form"
window(FORM, "com.example.customer-form", (760, 34, 1504, 830))
text((790, 92), "New customer", HEADING)
for i, (wid, label, placeholder) in enumerate(FIELDS):
    top = 120 + i * 56
    text((790, top + 20), label)
    x0, y0, x1, y1 = px((900, top, 1470, top + 40))
    draw.rectangle([x0, y0, x1 - 1, y1 - 1], fill=(255, 255, 255), outline=BORDER, width=2)
    draw.text((x0 + 24, (y0 + y1) / 2), placeholder, fill=PLACEHOLDER, font=TEXT, anchor="lm")
    widgets.append(
        {
            "id": wid,
            "window": FORM,
            "kind": "input",
            "label": label,
            "placeholder": placeholder,
            "box": [x0, y0, x1, y1],
        }
    )
text((790, 420), "Notes")
x0, y0, x1, y1 = px((900, 400, 1470, 520))
draw.rectangle([x0, y0, x1 - 1, y1 - 1], fill=(255, 255, 255), outline=BORDER, width=2)
draw.text((x0 + 24, y0 + 40), "Add a note for the team", fill=PLACEHOLDER, font=TEXT, anchor="lm")
widgets.append(
    {
        "id": "note",
        "window": FORM,
        "kind": "textarea",
        "label": "Notes",
        "placeholder": "Add a note for the team",
        "box": [x0, y0, x1, y1],
    }
)
# The notes' own Save: it saves the note, and with no note it does nothing.
button("note-save", FORM, "Save", (1380, 532, 1470, 568))
draw.line(px((776, 744, 1488, 744)), fill=BORDER, width=2)
widgets.append({"id": "status", "window": FORM, "kind": "status", "box": px([790, 762, 1180, 802])})
button("cancel", FORM, "Cancel", (1270, 764, 1360, 800))
# The footer's Save: it saves the customer.
button("save", FORM, "Save", (1380, 764, 1470, 800))

# Mail, which the copilot may not act in.
MAIL = "Mail"
window(MAIL, "com.example.mail", (8, 712, 744, 974))
text((24, 790), "From: Amara Okafor")
text((24, 836), "Subject: Invoice question")
button("reply", MAIL, "Reply", (520, 900, 610, 936))
button("send", MAIL, "Send", (630, 900, 720, 936))

image.save(HERE / "desktop.png", optimize=True)

# The glyph strip: each printable ASCII character in a cell as tall as the font's ascent
# plus descent and as wide as its advance, with a gap so no glyph bleeds into the next.
ascent, descent = TEXT.getmetrics()
height = ascent + descent
cells, x = {}, 0
chars = [chr(c) for c in range(32, 127)]
widths = [max(1, round(TEXT.getlength(c))) for c in chars]
strip = Image.new("RGB", (sum(widths) + 4 * len(chars), height), (255, 255, 255))
strip_draw = ImageDraw.Draw(strip)
for char, width in zip(chars, widths):
    strip_draw.text((x, 0), char, fill=(0, 0, 0), font=TEXT, anchor="la")
    cells[char] = [x, width]
    x += width + 4
strip.save(HERE / "glyphs.png", optimize=True)

layout = {
    "display": {"name": "built-in", "size": list(SIZE), "devicePixelRatio": RATIO},
    "background": list(DESKTOP),
    "windows": windows,
    "widgets": widgets,
    "glyphs": {"file": "glyphs.png", "height": height, "cells": cells},
    "record": {label.lower(): value for label, value in RECORD},
}
(HERE / "layout.json").write_text(json.dumps(layout, indent=2) + "\n")
print(f"wrote desktop.png {SIZE[0]}x{SIZE[1]}, glyphs.png ({len(cells)} glyphs), layout.json")
