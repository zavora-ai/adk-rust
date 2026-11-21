"""Draws the aisle background and the forklift sprite the video fixtures are made from."""

import sys

from PIL import Image, ImageDraw, ImageFont

font_path = sys.argv[1] if len(sys.argv) > 1 else "/System/Library/Fonts/Supplemental/Arial Bold.ttf"
big, small = ImageFont.truetype(font_path, 28), ImageFont.truetype(font_path, 20)

scene = Image.new("RGB", (640, 360), (200, 192, 168))
d = ImageDraw.Draw(scene)
for y in (0, 290):  # shelving on both sides of the aisle
    d.rectangle([0, y, 639, y + 69], fill=(107, 107, 107))
    for x in range(210, 640, 90):
        d.rectangle([x, y + 8, x + 70, y + 61], fill=(150, 110, 60))  # boxes on the shelves
d.rectangle([196, 70, 203, 289], fill=(240, 220, 0))  # the aisle entrance line
d.text((300, 18), "AISLE 3", font=big, fill=(240, 220, 0))
scene.save("background.png")

lift = Image.new("RGBA", (200, 150), (0, 0, 0, 0))
d = ImageDraw.Draw(lift)
d.rectangle([0, 40, 140, 125], fill=(255, 127, 0))  # body
d.rectangle([20, 0, 90, 40], outline=(20, 20, 20), width=5)  # cab cage
d.rectangle([140, 0, 152, 135], fill=(20, 20, 20))  # mast
d.rectangle([152, 120, 199, 128], fill=(20, 20, 20))  # forks
for cx in (30, 110):  # wheels
    d.ellipse([cx - 18, 108, cx + 18, 144], fill=(30, 30, 30))
d.text((12, 66), "FORKLIFT", font=small, fill=(0, 0, 0))
lift.save("forklift.png")
