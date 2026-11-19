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
from pathlib import Path

from PIL import Image, ImageDraw, ImageFont

HERE = Path(__file__).parent
FONTS = Path("/System/Library/Fonts/Supplemental")


def font(name, size):
    return ImageFont.truetype(str(FONTS / name), size)


REGULAR = font("Arial.ttf", 26)
SMALL = font("Arial.ttf", 18)
BOLD = font("Arial Bold.ttf", 26)
TITLE = font("Arial Bold.ttf", 40)
HAND = font("Bradley Hand Bold.ttf", 36)
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
        box = self.text(xy, value, HAND, fill=(20, 30, 120))
        self.field(name, value, box, hard="handwriting")
        return box

    def stamp(self, centre, word, name):
        """A red rubber stamp, rotated and half transparent, over the field `name`."""
        layer = Image.new("RGBA", (420, 160), (0, 0, 0, 0))
        d = ImageDraw.Draw(layer)
        d.rectangle([6, 6, 413, 153], outline=(200, 20, 30, 190), width=8)
        w = d.textlength(word, font=STAMP)
        d.text(((420 - w) / 2, 34), word, fill=(200, 20, 30, 170), font=STAMP)
        layer = layer.rotate(-14, expand=True, resample=Image.BICUBIC)
        x, y = centre[0] - layer.width // 2, centre[1] - layer.height // 2
        self.image.paste(layer, (x, y), layer)
        for f in self.fields:
            if f["name"] == name:
                f["hard"] = "stamp"

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
        page.handwritten((580, y - 8), "total", total_text)
    else:
        page.labelled((480, y), "Total", "total", total_text, BOLD)
    page.text((60, 960), "Thank you for your order.", SMALL)
    if n == 9:
        page.stamp((620, 150), "PAID", "date")
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
        w = page.draw.textlength(total_text, font=HAND)
        page.handwritten((480 - w, y + 30), "total", total_text)
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
        page.stamp((480, top + 3 * row + 30), "APPROVED", "fee")
    return page.save()


def portrait(image, box, hue):
    """A drawn head and shoulders, clipped to its frame: generated, not anyone's photograph."""
    x0, y0, x1, y1 = box
    w, h = x1 - x0, y1 - y0
    tile = Image.new("RGB", (w, h), (220, 226, 235))
    draw = ImageDraw.Draw(tile)
    cx = w // 2
    skin = [(234, 192, 150), (198, 140, 100), (150, 100, 70), (240, 205, 170)][hue % 4]
    shirt = [(60, 110, 160), (150, 60, 60), (60, 130, 90), (90, 90, 110)][hue % 4]
    hair = [(70, 45, 30), (20, 20, 20), (120, 80, 40), (60, 60, 60)][hue % 4]
    draw.ellipse([cx - w * 0.46, h * 0.68, cx + w * 0.46, h * 1.5], fill=shirt)
    draw.rectangle([cx - w * 0.08, h * 0.52, cx + w * 0.08, h * 0.72], fill=skin)
    draw.ellipse([cx - w * 0.24, h * 0.14, cx + w * 0.24, h * 0.6], fill=skin)
    draw.chord([cx - w * 0.26, h * 0.1, cx + w * 0.26, h * 0.44], 180, 360, fill=hair)
    draw.ellipse([cx - w * 0.12, h * 0.32, cx - w * 0.06, h * 0.36], fill=(30, 30, 30))
    draw.ellipse([cx + w * 0.06, h * 0.32, cx + w * 0.12, h * 0.36], fill=(30, 30, 30))
    draw.arc([cx - w * 0.08, h * 0.42, cx + w * 0.08, h * 0.5], 20, 160, fill=(150, 60, 50), width=3)
    image.paste(tile, (x0, y0))
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
