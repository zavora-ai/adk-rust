#!/usr/bin/env python3
"""Draws the forty Document desk pages of spec 3.7 S1 and S2 and writes their ground truth.

Twelve invoices, twelve receipts, six contracts with tables and ten identity cards. Every
name, address, email, phone number and identity number is invented; the identity cards'
portraits are drawn shapes, captioned "Generated portrait, not a real person". Five fields
are hard on purpose (S2): three amounts written by hand and two values under a rubber
stamp.

Boxes are Pillow's `textbbox` ink boxes in source pixels (origin top-left, x1 and y1
exclusive). `documents.json` lists every page with its kind, fields, personal strings and,
for identity cards, the portrait box.

    python3 generate.py   # needs Pillow and the macOS Arial and Bradley Hand fonts
"""

import json
import math
import os
import random
from pathlib import Path

from PIL import Image, ImageDraw, ImageFilter, ImageFont

HERE = Path(__file__).parent
FONTS = Path("/System/Library/Fonts/Supplemental")


def font(name, size):
    return ImageFont.truetype(str(FONTS / name), size)


REGULAR = font("Arial.ttf", 26)
SMALL = font("Arial.ttf", 18)
BOLD = font("Arial Bold.ttf", 26)
TITLE = font("Arial Bold.ttf", 40)
HAND_FONT = "SnellRoundhand.ttc"
STAMP = font("Arial Black.ttf", 64)

FIRST = ["Amara", "Bruno", "Chen", "Dalia", "Emeka", "Farah", "Goran", "Hana", "Ivo", "Juno",
         "Kofi", "Lena", "Mateo", "Nia", "Oskar", "Priya", "Quinn", "Rosa", "Sami", "Tove"]
LAST = ["Okafor", "Varga", "Lindqvist", "Moreau", "Tanaka", "Ibeh", "Novak", "Ferreira",
        "Haddad", "Kowalski", "Mensah", "Duarte", "Sato", "Brandt", "Achieng", "Rossi"]
STORES = ["Larch Street Market", "Harbour Hardware", "Copperleaf Books", "Juniper Pharmacy",
          "Northgate Bakery", "Quayside Coffee", "Willow Garden Supply", "Redfern Stationers",
          "Ember Grill", "Cedar Lane Grocers", "Tidewater Fish Co", "Maple Print Shop"]
ITEMS = ["Torque sensor", "Fiducial card", "Seal kit", "Gasket set", "Cable ties",
         "Bearing pack", "Relay module", "Hex bolts", "Thermal paste", "Pressure gauge"]
# The draw of each hard field, chosen so PP-OCRv6 tiny reads it as one block below the
# desk's 0.8 threshold while every printed block on every page stays above 0.85.
SEEDS = json.loads(os.environ.get("DOC_SEEDS", "null")) or {
    "invoice-05.png": 0, "invoice-09.png": 15, "receipt-03.png": 4, "receipt-07.png": 26,
    "contract-04.png": 18,
}
GOODS = ["Bread", "Coffee", "Notebook", "Batteries", "Tape", "Apples", "Soap", "Pens", "Rice"]


def person(i):
    first, last = FIRST[i % len(FIRST)], LAST[(i * 7) % len(LAST)]
    return f"{first} {last}", f"{first.lower()}.{last.lower()}@example.com"


class Page:
    def __init__(self, file, kind, size, background="white"):
        self.file, self.kind = file, kind
        self.image = Image.new("RGB", size, background)
        self.draw = ImageDraw.Draw(self.image)
        self.fields, self.personal, self.extra = [], [], {}

    def text(self, xy, value, fnt=REGULAR, fill=(0, 0, 0)):
        self.draw.text(xy, value, fill=fill, font=fnt)
        return list(self.draw.textbbox(xy, value, font=fnt))

    def field(self, name, value, box, personal=False, hard=None):
        entry = {"name": name, "value": value, "box": box, "personal": personal}
        if hard:
            entry["hard"] = hard
        self.fields.append(entry)
        if personal:
            self.personal.append(value)

    def labelled(self, xy, label, name, value, fnt=REGULAR, personal=False):
        """Draws `label: value` on one line; the field's box is the value's."""
        prefix = f"{label}: "
        start = self.draw.textbbox(xy, prefix, font=fnt)[2]
        self.text(xy, prefix, fnt)
        box = self.text((start + 4, xy[1]), value, fnt)
        self.field(name, value, box, personal)
        return box

    def handwritten(self, xy, name, value):
        """A hurried pen: each glyph turned, lifted and sized by hand, the ink soft at the
        edges. Returns the ink's box."""
        rnd = random.Random(f"{value}:{SEEDS.get(self.file, 0)}")
        layer = Image.new("RGBA", (60 * len(value) + 40, 140), (0, 0, 0, 0))
        x = 10
        for ch in value:
            glyph = Image.new("RGBA", (90, 120), (0, 0, 0, 0))
            size = int(40 * rnd.uniform(0.85, 1.2))
            ImageDraw.Draw(glyph).text((15, 15), ch, font=font(HAND_FONT, size), fill=(20, 30, 120, 255))
            glyph = glyph.rotate(rnd.uniform(-28, 28), resample=Image.BICUBIC)
            layer.paste(glyph, (x, int(rnd.uniform(-3, 3)) + 10), glyph)
            x += int(size * rnd.uniform(0.48, 0.56))
        layer = layer.crop(layer.getbbox()).filter(ImageFilter.GaussianBlur(1.6))
        self.image.paste(layer, xy, layer)
        box = [xy[0], xy[1], xy[0] + layer.width, xy[1] + layer.height]
        self.field(name, value, box, hard="handwriting")
        return box

    def stamp(self, word, name):
        """A red rubber stamp, turned and half transparent, over the field `name`: a word in a
        frame, or with no word a company seal's double ring."""
        rnd = random.Random(f"{word}:{SEEDS.get(self.file, 0)}")
        if word:
            layer = Image.new("RGBA", (420, 160), (0, 0, 0, 0))
            d = ImageDraw.Draw(layer)
            d.rectangle([6, 6, 413, 153], outline=(200, 20, 30, 225), width=10)
            w = d.textlength(word, font=STAMP)
            d.text(((420 - w) / 2, 34), word, fill=(200, 20, 30, 215), font=STAMP)
        else:
            layer = Image.new("RGBA", (150, 150), (0, 0, 0, 0))
            d = ImageDraw.Draw(layer)
            d.ellipse([4, 4, 146, 146], outline=(40, 60, 170, 210), width=9)
            d.ellipse([26, 26, 124, 124], outline=(40, 60, 170, 210), width=6)
            for k in range(8):
                a = k * 3.14159 / 4
                cx, cy = 75 + 50 * math.cos(a), 75 + 50 * math.sin(a)
                d.regular_polygon((cx, cy, 6), 5, fill=(40, 60, 170, 210))
        layer = layer.rotate(rnd.uniform(-20, -6), expand=True, resample=Image.BICUBIC)
        field = next(f for f in self.fields if f["name"] == name)
        x0, y0, x1, y1 = field["box"]
        centre = ((x0 + x1) / 2 + rnd.uniform(-40, 40), (y0 + y1) / 2 + rnd.uniform(-30, 30))
        self.image.paste(layer, (int(centre[0] - layer.width / 2), int(centre[1] - layer.height / 2)), layer)
        field["hard"] = "stamp"

    def save(self):
        self.image.save(HERE / self.file, optimize=True)
        entry = {"file": self.file, "kind": self.kind, "width": self.image.width,
                 "height": self.image.height, "fields": self.fields, "personal": self.personal}
        entry.update(self.extra)
        return entry


def invoice(n, i):
    page = Page(f"invoice-{n:02d}.png", "invoice", (800, 1040))
    number = f"INV-{4800 + n * 17}"
    box = page.text((60, 50), f"INVOICE {number}", TITLE)
    title_word = page.draw.textbbox((60, 50), "INVOICE ", font=TITLE)[2]
    page.field("number", number, [title_word, box[1], box[2], box[3]])
    page.text((60, 140), "Bill to", BOLD)
    name, email = person(i)
    page.labelled((60, 182), "Name", "customer", name, personal=True)
    page.text((60, 224), f"Email: {email}")
    page.personal.append(email)
    date = f"2026-{(n % 9) + 1:02d}-{(n * 3) % 27 + 1:02d}"
    page.labelled((480, 140), "Date", "date", date)
    top, row = 320, 56
    cols = [60, 420, 560, 740]
    rows = [("Item", "Qty", "Amount")]
    total = 0.0
    for k in range(3):
        qty = (n + k) % 4 + 1
        amount = round(qty * (7.5 + ((n * 13 + k * 5) % 40)), 2)
        total += amount
        rows.append((ITEMS[(n + k * 3) % len(ITEMS)], str(qty), f"{amount:.2f}"))
    for r in range(len(rows) + 1):
        page.draw.line([(cols[0], top + r * row), (cols[-1], top + r * row)], fill="black", width=2)
    for c in cols:
        page.draw.line([(c, top), (c, top + len(rows) * row)], fill="black", width=2)
    for r, values in enumerate(rows):
        for c, value in enumerate(values):
            page.text((cols[c] + 14, top + r * row + 14), value, BOLD if r == 0 else REGULAR)
    total_text = f"{total:.2f}"
    y = top + len(rows) * row + 40
    if n == 5:
        page.text((480, y), "Total:", BOLD)
        page.handwritten((580, y - 14), "total", total_text)
    else:
        page.labelled((480, y), "Total", "total", total_text, BOLD)
    page.text((60, 960), "Thank you for your order.", SMALL)
    if n == 9:
        page.stamp("PAID", "date")
    return page.save()


def receipt(n, i):
    page = Page(f"receipt-{n:02d}.png", "receipt", (520, 820))
    store = STORES[(n - 1) % len(STORES)]
    box = page.text((40, 40), store, BOLD)
    page.field("store", store, box)
    page.text((40, 84), f"{10 + n} Harbour Road", SMALL)
    date = f"2026-{(n % 11) + 1:02d}-{(n * 5) % 27 + 1:02d}"
    page.labelled((40, 130), "Date", "date", date)
    y, total = 200, 0.0
    for k in range(4):
        price = round(1.2 + ((n * 7 + k * 11) % 50) / 4, 2)
        total += price
        page.text((40, y), GOODS[(n + k) % len(GOODS)])
        right = f"{price:.2f}"
        w = page.draw.textlength(right, font=REGULAR)
        page.text((480 - w, y), right)
        y += 48
    page.draw.line([(40, y + 10), (480, y + 10)], fill="black", width=2)
    total_text = f"{total:.2f}"
    page.text((40, y + 40), "TOTAL", BOLD)
    if n in (3, 7):
        page.handwritten((360, y + 24), "total", total_text)
    else:
        w = page.draw.textlength(total_text, font=BOLD)
        box = page.text((480 - w, y + 40), total_text, BOLD)
        page.field("total", total_text, box)
    page.text((40, 740), "Keep this receipt for returns.", SMALL)
    return page.save()


def contract(n, i):
    page = Page(f"contract-{n:02d}.png", "contract", (800, 1040))
    number = f"C-2026-{n:02d}"
    page.text((60, 50), "SERVICE AGREEMENT", TITLE)
    page.labelled((60, 120), "Agreement", "number", number, BOLD)
    name, email = person(i + 3)
    page.text((60, 180), "This agreement is made between Zavoria Parts Ltd", SMALL)
    page.text((60, 206), "and the client named below, on the terms in the table.", SMALL)
    top, row = 280, 60
    cols = [60, 340, 740]
    fee = f"{1200 + n * 85}.00"
    rows = [("Term", "Value"), ("Client", name), ("Start date", f"2026-{n + 2:02d}-01"),
            ("Monthly fee", fee), ("Duration", f"{6 + n} months")]
    for r in range(len(rows) + 1):
        page.draw.line([(cols[0], top + r * row), (cols[-1], top + r * row)], fill="black", width=2)
    for c in cols:
        page.draw.line([(c, top), (c, top + len(rows) * row)], fill="black", width=2)
    for r, (term, value) in enumerate(rows):
        fnt = BOLD if r == 0 else REGULAR
        page.text((cols[0] + 14, top + r * row + 16), term, fnt)
        box = page.text((cols[1] + 14, top + r * row + 16), value, fnt)
        if term == "Client":
            page.field("client", value, box, personal=True)
        elif term == "Monthly fee":
            page.field("fee", value, box)
    page.text((60, 640), f"Notices go to {email}.", SMALL)
    page.personal.append(email)
    page.text((60, 900), "Signed for Zavoria Parts Ltd", SMALL)
    if n == 4:
        page.stamp("PAID", "fee")
    return page.save()


def radial(size, inner, outer):
    """An ellipse shaded from `inner` at its centre to `outer` at its rim."""
    w, h = size
    tile = Image.new("RGB", size, outer)
    draw = ImageDraw.Draw(tile)
    for i in range(40):
        t = i / 40
        colour = tuple(int(outer[k] + (inner[k] - outer[k]) * t) for k in range(3))
        draw.ellipse([w * t / 2, h * t / 2, w - w * t / 2, h - h * t / 2], fill=colour)
    return tile


def portrait(image, box, hue):
    """A drawn head and shoulders, shaded and softened like a photograph and clipped to its
    frame: generated, not anyone's likeness."""
    x0, y0, x1, y1 = box
    w, h = x1 - x0, y1 - y0
    skin = [(232, 190, 160), (198, 146, 112), (156, 106, 76), (240, 205, 175)][hue % 4]
    shade = tuple(max(0, c - 42) for c in skin)
    hair = [(55, 38, 28), (20, 20, 22), (120, 80, 40), (70, 66, 62)][hue % 4]
    shirt = [(52, 70, 98), (120, 50, 50), (50, 100, 72), (80, 80, 96)][hue % 4]
    tile = Image.new("RGB", (w, h), (205, 214, 226))
    draw = ImageDraw.Draw(tile)
    draw.polygon([(10, h), (25, h * 0.78), (w * 0.35, h * 0.68), (w * 0.65, h * 0.68),
                  (w - 25, h * 0.78), (w - 10, h)], fill=shirt)
    draw.polygon([(w * 0.42, h * 0.68), (w / 2, h * 0.8), (w * 0.58, h * 0.68)], fill=(230, 230, 235))
    draw.rectangle([w * 0.43, h * 0.55, w * 0.57, h * 0.7], fill=shade)
    face = radial((int(w * 0.46), int(h * 0.5)), skin, shade)
    mask = Image.new("L", face.size, 0)
    ImageDraw.Draw(mask).ellipse([0, 0, face.size[0], face.size[1]], fill=255)
    tile.paste(face, (int(w * 0.27), int(h * 0.12)), mask)
    draw.ellipse([w * 0.24, h * 0.3, w * 0.31, h * 0.42], fill=shade)
    draw.ellipse([w * 0.69, h * 0.3, w * 0.76, h * 0.42], fill=shade)
    draw.chord([w * 0.25, h * 0.07, w * 0.75, h * 0.42], 180, 360, fill=hair)
    draw.rectangle([w * 0.25, h * 0.2, w * 0.3, h * 0.34], fill=hair)
    draw.rectangle([w * 0.7, h * 0.2, w * 0.75, h * 0.34], fill=hair)
    for ex in (0.4, 0.6):
        draw.ellipse([w * ex - 8, h * 0.33 - 4, w * ex + 8, h * 0.33 + 4], fill=(245, 245, 245))
        draw.ellipse([w * ex - 4, h * 0.33 - 4, w * ex + 4, h * 0.33 + 4], fill=(60, 40, 30))
        draw.line([w * ex - 10, h * 0.29, w * ex + 10, h * 0.285], fill=hair, width=3)
    draw.line([w / 2, h * 0.36, w * 0.47, h * 0.45, w * 0.52, h * 0.46], fill=shade, width=3)
    draw.arc([w * 0.42, h * 0.46, w * 0.58, h * 0.54], 20, 160, fill=(150, 70, 70), width=4)
    image.paste(tile.filter(ImageFilter.GaussianBlur(1.2)), (x0, y0))
    ImageDraw.Draw(image).rectangle(box, outline=(140, 160, 190), width=2)


def id_card(n, i):
    page = Page(f"id-card-{n:02d}.png", "identity_card", (640, 400), background=(236, 241, 246))
    page.draw.rectangle([0, 0, 640, 64], fill=(28, 74, 140))
    page.text((236, 10), "REPUBLIC OF ZAVORIA", font("Arial Bold.ttf", 22), fill="white")
    page.text((236, 36), "IDENTITY CARD", font("Arial Bold.ttf", 20), fill="white")
    box = [28, 84, 208, 312]
    portrait(page.image, box, n)
    page.extra["portrait"] = box
    page.text((28, 318), "Generated portrait, not a real person", font("Arial.ttf", 11),
              fill=(40, 60, 90))
    name, email = person(i + 11)
    number = f"{100 + n * 37:03d}-{(n * 53) % 90 + 10:02d}-{(n * 7919) % 9000 + 1000:04d}"
    phone = f"555-{(n * 113) % 900 + 100:03d}-{(n * 4271) % 9000 + 1000:04d}"
    page.labelled((236, 88), "Name", "name", name, font("Arial.ttf", 22), personal=True)
    page.labelled((236, 132), "Email", "email", email, font("Arial.ttf", 22), personal=True)
    page.labelled((236, 176), "Phone", "phone", phone, font("Arial.ttf", 22), personal=True)
    page.labelled((236, 220), "ID No", "id_number", number, font("Arial.ttf", 22), personal=True)
    page.labelled((236, 264), "Expires", "expires", f"{2030 + n % 7}", font("Arial.ttf", 22))
    page.text((236, 350), "Specimen for testing. All personal data is invented.",
              font("Arial.ttf", 13), fill=(40, 60, 90))
    return page.save()


pages = []
for n in range(1, 13):
    pages.append(invoice(n, n))
for n in range(1, 13):
    pages.append(receipt(n, n + 20))
for n in range(1, 7):
    pages.append(contract(n, n + 40))
for n in range(1, 11):
    pages.append(id_card(n, n + 60))

hard = [(p["file"], f["name"], f["hard"]) for p in pages for f in p["fields"] if f.get("hard")]
assert len(pages) == 40 and len(hard) == 5, hard
(HERE / "documents.json").write_text(json.dumps({
    "frame": "source pixels, origin top-left, x1/y1 exclusive",
    "note": "Every personal field is invented; portraits are drawn shapes, not people.",
    "pages": pages,
}, indent=1) + "\n")
print(f"{len(pages)} pages, hard fields: {hard}")
