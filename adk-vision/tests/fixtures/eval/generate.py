#!/usr/bin/env python3
"""Writes the vision eval sets in this directory from the fixtures' ground truth.

Each image is pinned by its SHA-256, so `adk-eval` refuses a case whose image changed.
The detection scene is drawn by scene.py first.
Masks are rasterised from the drawn geometry, pixel centres inside each shape, and
run-length encoded in COCO's column-major order. Run from anywhere:

    python3 adk-vision/tests/fixtures/eval/generate.py
"""

import hashlib
import json
from pathlib import Path

HERE = Path(__file__).resolve().parent
FIXTURES = HERE.parent


def digest(path):
    return "sha256:" + hashlib.sha256(path.read_bytes()).hexdigest()


def image_part(relative):
    return {"image": {"path": relative, "digest": digest(HERE / relative)}}


def rle(width, height, inside):
    """COCO RLE, column-major, unset run first."""
    counts, current, run = [], False, 0
    for x in range(width):
        for y in range(height):
            bit = inside(x + 0.5, y + 0.5)
            if bit != current:
                counts.append(run)
                current, run = bit, 0
            run += 1
    counts.append(run)
    return {"width": width, "height": height, "counts": counts}


def in_triangle(points):
    (ax, ay), (bx, by), (cx, cy) = points

    def sign(px, py, x1, y1, x2, y2):
        return (px - x2) * (y1 - y2) - (x1 - x2) * (py - y2)

    def inside(x, y):
        d1 = sign(x, y, ax, ay, bx, by)
        d2 = sign(x, y, bx, by, cx, cy)
        d3 = sign(x, y, cx, cy, ax, ay)
        return not ((d1 < 0 or d2 < 0 or d3 < 0) and (d1 > 0 or d2 > 0 or d3 > 0))

    return inside


def turn(invocation, parts, expected):
    return {
        "invocation_id": invocation,
        "user_content": {"role": "user", "parts": parts},
        "expected_vision": expected,
    }


def case(eval_id, description, turns, tags):
    return {"eval_id": eval_id, "description": description, "conversation": turns, "tags": tags}


def write(name, eval_set_id, title, description, cases):
    body = {"eval_set_id": eval_set_id, "name": title, "description": description, "eval_cases": cases}
    (HERE / name).write_text(json.dumps(body, indent=2) + "\n")


def shapes():
    truth = json.loads((FIXTURES / "cloud/shapes-960x540.groundtruth.json").read_text())
    w, h = truth["width"], truth["height"]
    objects = {o["label"]: o for o in truth["objects"]}
    masks = {
        "red rectangle": rle(w, h, lambda x, y: 80 <= x < 320 and 60 <= y < 220),
        "blue circle": rle(w, h, lambda x, y: (x - 640) ** 2 + (y - 150) ** 2 <= 90 * 90),
        "green triangle": rle(w, h, in_triangle(objects["green triangle"]["polygon"])),
    }
    shape_names = list(masks)
    image = "../cloud/shapes-960x540.png"

    # Box prompts 6 px outside each shape, for point-and-box segmenters.
    prompts = []
    for name in shape_names:
        x0, y0, x1, y1 = objects[name]["bbox"]
        prompts.append({"kind": "box", "bbox": {"x0": x0 - 6, "y0": y0 - 6, "x1": x1 + 6, "y1": y1 + 6}})
    regions = [{"box": objects[n]["bbox"], "mask": masks[n]} for n in shape_names]
    write(
        "shapes-segment.test.json", "vision-shapes-segment", "Drawn shapes, box prompts",
        "Three drawn shapes segmented from box prompts 6 px outside each; masks are the exact "
        "rasters of the drawn geometry. For promptable segmenters (EfficientSAM, SAM 2.1).",
        [case("shapes_boxes", "Segment the rectangle, circle and triangle from box prompts",
              [turn("t0", [image_part(image), {"text": json.dumps(prompts)}],
                    {"regions": regions, "count": 3})], ["segment", "masks"])],
    )

    concept_cases = []
    for name in shape_names:
        slug = name.replace(" ", "_")
        concept_cases.append(case(
            f"concept_{slug}", f"Segment the {name} by name",
            [turn("t0", [image_part(image), {"text": name}],
                  {"regions": [{"label": name, "box": objects[name]["bbox"], "mask": masks[name]}],
                   "count": 1})],
            ["segment", "concepts"]))
    write(
        "shapes-concepts.test.json", "vision-shapes-concepts", "Drawn shapes, concept prompts",
        "Each drawn shape segmented by its name, one concept per case. For open-vocabulary "
        "segmenters (Meta sam-3.1).",
        concept_cases,
    )

    names = ", ".join(f'"{o["label"]}"' for o in truth["objects"])
    prompt = f"Locate these objects and label each region exactly as given: {names}."
    write(
        "shapes-locate.test.json", "vision-shapes-locate", "Drawn shapes, located by a describer",
        "Five labelled objects located by a describer asked for regions; boxes in source pixels.",
        [case("shapes_locate", "Locate the shapes and the two text lines",
              [turn("t0", [image_part(image), {"text": prompt}],
                    {"regions": [{"label": o["label"], "box": o["bbox"]} for o in truth["objects"]],
                     "count": len(truth["objects"])})], ["describe", "regions"])],
    )


def s5():
    truth = json.loads((FIXTURES / "s5/s5.json").read_text())
    targets = truth["targets"]
    names = ", ".join(t["name"] for t in targets)
    prompt = (
        f"This is a screenshot of a form. Locate each of these labelled widgets: {names}. "
        "Label each region with the widget's name exactly as given (for example `name-field`)."
    )
    expected = [{"label": t["name"], "box": t["box"]} for t in targets]
    write(
        "s5-grounding.test.json", "vision-s5-grounding", "S5 screen, twelve targets",
        "The 3024x1964 screenshot of a 1512x982-point display at device pixel ratio 2: every "
        "located widget's centre must fall inside its target (spec 3.7 S5).",
        [case("s5_targets", "Locate the twelve labelled widgets",
              [turn("t0", [image_part("../s5/s5-screen.png"), {"text": prompt}],
                    {"regions": expected, "targets": expected})], ["describe", "grounding"])],
    )


def page_text(objects):
    lines = [o for o in objects if o.get("text")]
    # Reading order: 12 px bands top to bottom, left to right within a band.
    lines.sort(key=lambda o: (o["bbox"][1] // 12, o["bbox"][0]))
    return "\n".join(o["text"] for o in lines)


def documents():
    cases = []
    for stem, eval_id, description in [
        ("doc-page-1000x1300", "report_page",
         "Read the report page: title, paragraph, table, chart labels, footer"),
        ("invoice-personal-1000x1300", "invoice",
         "Read the invoice and its labelled fields (invented personal data)"),
    ]:
        truth = json.loads((FIXTURES / f"documents/{stem}.groundtruth.json").read_text())
        expected = {"text": page_text(truth["objects"])}
        fields = {}
        for o in truth["objects"]:
            label, sep, value = (o.get("text") or "").partition(":")
            if sep and label.replace(" ", "").isalpha() and value.strip():
                fields[label.strip().lower()] = value.strip()
        if fields:
            expected["fields"] = fields
        cases.append(case(eval_id, description,
                          [turn("t0", [image_part(f"../documents/{stem}.png"),
                                       {"text": "Read this page."}], expected)],
                          ["read", "ocr"] + (["fields"] if fields else [])))
    write(
        "documents-read.test.json", "vision-documents-read", "Generated documents",
        "A report page and an invoice drawn by documents/generate.py; text in reading order "
        "(12 px bands, left to right) and the invoice's `Label: value` fields.",
        cases,
    )


def detect():
    truth = json.loads((HERE / "scene-1024x640.groundtruth.json").read_text())
    labels = [o["label"] for o in truth["objects"]]
    write(
        "generated-detect.test.json", "vision-generated-detect", "Drawn scene, COCO classes",
        "A stop sign and a wall clock drawn by scene.py at known boxes, for closed-set "
        "detectors trained on COCO (RF-DETR); the turn's text names the classes.",
        [case("scene_stop_sign_clock", "Detect the stop sign and the clock",
              [turn("t0", [image_part("scene-1024x640.png"), {"text": ", ".join(labels)}],
                    {"regions": [{"label": o["label"], "box": o["bbox"]} for o in truth["objects"]],
                     "count": len(labels)})], ["detect"])],
    )


if __name__ == "__main__":
    shapes()
    s5()
    documents()
    detect()
