"""Writes the annotated viewer's fixture answer records (spec 3.7 §8.2, X12).

Each record is what `adk-vision`'s toolset saves as `vision-answer-<call>.json` beside the
image it answered about, with the boards' fixture content (board choice S6): Northwind
Packaging's invoice INV-2291, the Inspector's line 2 stills and a warehouse aisle. Records
for the Inspector and the video carry what a phase-B feature adds (`measurements`, `tracks`,
`video`, `display`). Run from this folder: `python3 generate.py`.
"""

import hashlib
import json


def sha(seed):
    return "sha256:" + hashlib.sha256(seed.encode()).hexdigest()


PAGE = sha("source:INV-2291.pdf:p1:200dpi")
FRAME = sha("evidence:line-2:10:41:07")
FRAME_OK = sha("evidence:line-2:11:02:44")
VFRAME = sha("video:warehouse-2026-11-12.mp4:19023")
CROP_PO = sha("egress:inv-2291:po")
RELEASED_OK = sha("egress:line-2:11:02:44")

INV = (1654, 2339)
# reading order, kind, box, score, text
INV_BLOCKS = [
    (1, "title", [1100, 150, 1524, 262], 0.99, "INVOICE"),
    (2, "text", [124, 140, 900, 400], 0.97, "Northwind Packaging Ltd · Unit 7, Harbour Road · Port Examplia EX4 2PL"),
    (3, "text", [1000, 296, 1524, 704], 0.93, "Invoice no. INV-2291 · Invoice date 06.11.2026 · PO number · Due date 06.12.2026 · Currency EUR"),
    (4, "seal", [1232, 332, 1430, 528], 0.88, "RECEIVED 07 NOV 2026"),
    (5, "handwriting", [1250, 466, 1532, 540], 0.58, "PO-7781S"),
    (6, "text", [120, 520, 770, 760], 0.96, "Bill to · Mara Kestrel · 14 Ferry Lane · Port Examplia EX9 1QT"),
    (7, "table", [124, 816, 1530, 1322], 0.95, "4 rows × 4 columns"),
    (8, "text", [1000, 1356, 1530, 1572], 0.94, "Subtotal 970.60 · VAT 20% 194.12 · Total due € 1,164.72"),
    (9, "checkbox", [120, 1376, 560, 1436], 0.81, "☑ Paid by bank transfer"),
    (10, "handwriting", [120, 1488, 700, 1604], 0.47, "Approved — J.T. 12/11"),
    (11, "text", [120, 2076, 1534, 2214], 0.90, "Bank: Examplia Savings · IBAN EX12 3456 7890 1234 56 · BIC EXSVEX22"),
]
# The blocks are listed out of reading order, as a layout model emits them.
EMITTED = [0, 1, 3, 2, 4, 5, 6, 8, 7, 9, 10]

LETTERBOX_PAGE = {
    "frame": {"kind": "modelInput", "width": 960, "height": 960, "letterbox": "center"},
    "chain": {
        "sourceWidth": INV[0],
        "sourceHeight": INV[1],
        "steps": [
            {"op": "resize", "width": 679, "height": 960},
            {"op": "pad", "left": 140, "top": 0, "right": 141, "bottom": 0},
        ],
    },
}


def box(b):
    return {"kind": "box", "x0": b[0], "y0": b[1], "x1": b[2], "y1": b[3]}


def origin(backend, manifest, frame_chain, **extra):
    out = {"backend": backend, "manifest": manifest, **frame_chain}
    out.update(extra)
    return out


def page_record(call, at, backend, manifest, blocks, calls, models, *, escalate=None, agent="desk"):
    """A `vision_read` record of INV-2291 page 1: `blocks` as (order, kind, box, score, text)."""
    regions, page_blocks = [], []
    for order, kind, b, score, text in blocks:
        extra = {}
        if escalate and order == escalate["order"]:
            extra["escalatedFrom"] = {
                "backend": manifest,
                "reason": {"kind": "lowConfidence", "threshold": 0.6},
                "score": score,
                "text": text,
            }
            text = escalate["text"]
        regions.append({
            "image": PAGE,
            "shape": box(b),
            "label": kind,
            "score": score,
            "origin": origin(backend, manifest, LETTERBOX_PAGE, **extra),
        })
        page_blocks.append({"kind": kind, "text": text, "confidence": score})
    order = sorted(range(len(blocks)), key=lambda i: blocks[i][0])
    return {
        "kind": "adkVisionAnswer",
        "version": 1,
        "tool": "vision_read",
        "callId": call,
        "invocationId": "inv-northwind-batch",
        "agent": agent,
        "at": at,
        "image": {
            "digest": PAGE,
            "width": INV[0],
            "height": INV[1],
            "mimeType": "image/png",
            "artifact": "vision-" + PAGE[7:] + ".png",
            "name": "INV-2291.pdf",
            "provenance": {"kind": "documentPage", "document": sha("INV-2291.pdf"), "page": 1, "dpi": 200},
        },
        "args": {"image": "INV-2291.pdf"},
        "regionSet": {"image": PAGE, "width": INV[0], "height": INV[1], "regions": regions},
        "page": {"blocks": page_blocks, "readingOrder": order},
        "calls": calls,
        "models": models,
        "display": {"run": "Document desk · Northwind batch · 10:31"},
    }


def local_call(task, backend, ms, regions):
    return {
        "task": task,
        "backend": backend,
        "locality": {"kind": "inProcess"},
        "images": [],
        "units": {},
        "costMicroUsd": 0,
        "regions": regions,
        "durationMs": ms,
    }


PP = {"pp-ocrv6-small": {"revision": "3f2a9c1e5b7d40a8c2f61e9d3b8a7c5e4f1d2a6b", "licence": "Apache-2.0", "builtIn": True, "name": "PP-OCRv6 small"}}
VL = {"paddleocr-vl-1.6": {"revision": "a91c03d7e2b84f56c1d0e9a8b7c6d5e4f3a2b1c0", "licence": "Apache-2.0", "builtIn": True, "name": "PaddleOCR-VL-1.6"}}

blocks_a = [INV_BLOCKS[i] for i in EMITTED]
document_a = page_record(
    "c-31",
    "2026-11-20T07:31:00Z",
    "pp-ocrv6-small",
    "pp-ocrv6-small",
    blocks_a,
    [
        local_call("read", "pp-ocrv6-small", 1100, 11),
        {
            "task": "read",
            "backend": "gemini-3.8-flash",
            "locality": {"kind": "remote", "hosts": ["generativelanguage.googleapis.com"]},
            "images": [PAGE],
            "egress": [CROP_PO],
            "units": {},
            "regions": 1,
            "durationMs": 2300,
            "escalation": {"kind": "lowConfidence", "threshold": 0.6},
        },
    ],
    PP,
    escalate={"order": 5, "text": "PO-77815"},
)

# The comparison: PP-OCRv6 misreads the date under the stamp and the PO number; PaddleOCR-VL
# reads both and also finds the page number.
compare_blocks_a = []
for order, kind, b, score, text in blocks_a:
    if order == 3:
        compare_blocks_a.append((3, "text", [1188, 330, 1478, 380], 0.71, "06.11.2028"))
    else:
        compare_blocks_a.append((order, kind, b, score, text))
compare_a = page_record("c-31", "2026-11-20T07:31:00Z", "pp-ocrv6-small", "pp-ocrv6-small", compare_blocks_a, [local_call("read", "pp-ocrv6-small", 1100, 11)], PP)

compare_blocks_b = []
for order, kind, b, score, text in compare_blocks_a:
    # PaddleOCR-VL's boxes sit a few pixels off PP-OCRv6's: mean IoU about 0.94.
    w, h = b[2] - b[0], b[3] - b[1]
    dx, dy = round(w * 0.015), round(h * 0.016)
    nb = [b[0] + dx, b[1] + dy, b[2] + dx, b[3] + dy]
    if order == 3:
        compare_blocks_b.append((3, "text", nb, 0.95, "06.11.2026"))
    elif order == 5:
        compare_blocks_b.append((5, "handwriting", nb, 0.83, "PO-77815"))
    else:
        compare_blocks_b.append((order, kind, nb, min(0.99, score + 0.01), text))
compare_blocks_b.append((12, "text", [1300, 2236, 1524, 2288], 0.86, "Page 1 of 1"))
compare_b = page_record("c-34", "2026-11-20T07:33:00Z", "paddleocr-vl-1.6", "paddleocr-vl-1.6", compare_blocks_b, [local_call("read", "paddleocr-vl-1.6", 14800, 12)], VL)

# ---------------------------------------------------------------- the Inspector's still

CONV = (1920, 1080)
PARTS = [[330, 300, 630, 530, 0.96], [840, 400, 1140, 630, 0.97], [1290, 250, 1590, 480, 0.95], [1720, 470, 1920, 700, 0.88]]
FID = [40, 660, 285, 905]
TEAR = [[926, 494], [950, 502], [962, 494], [979, 505], [995, 498], [1010, 508], [1020, 508], [1018, 518], [1000, 532], [958, 532], [948, 512], [930, 504]]

LETTERBOX_FRAME = {
    "frame": {"kind": "modelInput", "width": 1024, "height": 1024, "letterbox": "center"},
    "chain": {
        "sourceWidth": CONV[0],
        "sourceHeight": CONV[1],
        "steps": [
            {"op": "resize", "width": 1024, "height": 576},
            {"op": "pad", "left": 0, "top": 224, "right": 0, "bottom": 224},
        ],
    },
}


def rle(width, height, inside):
    """COCO run-length encoding, column-major, unset run first, of the pixels `inside` sets."""
    counts, current, run = [], 0, 0
    for x in range(width):
        for y in range(height):
            bit = 1 if inside(x + 0.5, y + 0.5) else 0
            if bit == current:
                run += 1
            else:
                counts.append(run)
                current, run = bit, 1
    counts.append(run)
    return {"kind": "mask", "width": width, "height": height, "counts": counts}


def in_circle(cx, cy, r):
    return lambda x, y: (x - cx) ** 2 + (y - cy) ** 2 <= r * r


def in_polygon(points):
    xs = [p[0] for p in points]
    ys = [p[1] for p in points]
    x0, x1, y0, y1 = min(xs), max(xs), min(ys), max(ys)

    def inside(x, y):
        if x < x0 or x > x1 or y < y0 or y > y1:
            return False
        hit = False
        j = len(points) - 1
        for i in range(len(points)):
            (xi, yi), (xj, yj) = points[i], points[j]
            if (yi > y) != (yj > y) and x < (xj - xi) * (y - yi) / (yj - yi) + xi:
                hit = not hit
            j = i
        return hit

    return inside


def still_record(call, at, digest, regions, calls, models, display, *, args, measurements=None):
    record = {
        "kind": "adkVisionAnswer",
        "version": 1,
        "tool": "vision_segment",
        "callId": call,
        "invocationId": "inv-line-2-" + call,
        "agent": "inspector",
        "at": at,
        "image": {
            "digest": digest,
            "width": CONV[0],
            "height": CONV[1],
            "mimeType": "image/png",
            "artifact": "vision-" + digest[7:] + ".png",
            "name": digest,
            "provenance": {"kind": "camera", "source": "line-2"},
        },
        "args": args,
        "regionSet": {"image": digest, "width": CONV[0], "height": CONV[1], "regions": regions},
        "calls": calls,
        "models": models,
        "display": display,
    }
    if measurements:
        record["measurements"] = measurements
    return record


def det(b, label, score):
    return {"image": FRAME, "shape": box(b), "label": label, "score": score, "origin": origin("inspector-rfdetr-s-v3", "inspector-rfdetr-s-v3", LETTERBOX_FRAME)}


def seg(shape, label, score):
    return {"image": FRAME, "shape": shape, "label": label, "score": score, "origin": origin("sam2.1-tiny", "sam2.1-tiny", LETTERBOX_FRAME)}


photo_regions = [det(p[:4], "part", p[4]) for p in PARTS]
photo_regions.append(seg(rle(*CONV, in_circle(990, 515, 84)), "seal", 0.94))
photo_regions.append(seg(rle(*CONV, in_polygon(TEAR)), "seal tear", 0.91))
photo_regions.append(det(FID, "fiducial", 0.99))
segment_call = local_call("segment", "sam2.1-tiny", 240, 2)
segment_call["prompted"] = "from the box"
photo = still_record(
    "c-2207",
    "2026-11-20T07:41:07Z",
    FRAME,
    photo_regions,
    [local_call("detect", "inspector-rfdetr-s-v3", 61, 5), segment_call],
    {
        "inspector-rfdetr-s-v3": {"builtIn": False, "name": "RF-DETR S", "note": "your fine-tune"},
        "sam2.1-tiny": {"revision": "8c1e2f04b9a7d3c5e1f2a0b9c8d7e6f5a4b3c2d1", "licence": "Apache-2.0", "builtIn": True, "name": "SAM 2.1 Tiny"},
    },
    {
        "title": "Line 2 · part T43",
        "meta": "1920 × 1080 · line 2 camera, 10:41:07 · kept as evidence until 18 Dec",
        "run": "Inspector · woke on line 2 at 10:41:07",
        "caption": "A still the Inspector kept as evidence when it woke. Frames it did not keep are never stored. The white line is the longest length it measured.",
    },
    args={"image": FRAME, "boxes": [PARTS[1][:4]]},
    measurements=[{
        "region": 5,
        "call": "c-2208",
        "tool": "vision_measure",
        "longestMm": 14.3,
        "longestUncertaintyMm": 0.3,
        "areaMm2": 9.8,
        "areaUncertaintyMm2": 0.6,
        "line": [[930, 472], [1016, 486]],
        "calibration": {"referenceMm": 40.0, "referencePx": 245, "plane": "the belt's plane", "from": "the fiducial", "region": 6},
    }],
)

empty_call = {
    "task": "segment",
    "backend": "sam-3.1",
    "locality": {"kind": "remote", "hosts": ["api.meta.ai"]},
    "images": [FRAME_OK],
    "egress": [RELEASED_OK],
    "units": {"requests": 1},
    "costMicroUsd": 2500,
    "regions": 0,
    "durationMs": 1800,
}
empty = still_record(
    "c-2311",
    "2026-11-20T08:02:44Z",
    FRAME_OK,
    [],
    [empty_call],
    {},
    {
        "title": "Line 2 · 11:02:44",
        "meta": "1920 × 1080 · line 2 camera · kept as evidence until 18 Dec",
        "run": "Inspector · line 2 · 11:02:44",
        "caption": "The line supervisor asked the Inspector to check this frame again with a cloud model.",
        "covered": "nothing needed covering",
    },
    args={"image": FRAME_OK, "concepts": ["torn seal"], "backend": "sam-3.1"},
)

# ---------------------------------------------------------------- a video frame with tracks

WH = (1920, 1080)
LETTERBOX_VIDEO = {
    "frame": {"kind": "modelInput", "width": 640, "height": 640, "letterbox": "center"},
    "chain": {
        "sourceWidth": WH[0],
        "sourceHeight": WH[1],
        "steps": [
            {"op": "resize", "width": 640, "height": 360},
            {"op": "pad", "left": 0, "top": 140, "right": 0, "bottom": 140},
        ],
    },
}
TRACKS = [
    (4, "forklift", [570, 556, 1000, 920], 0.94, 139, [[612.2, 640.0], [998, 1200]], [[180, 960], [300, 930], [420, 900]], False),
    (9, "pallet", [1176, 636, 1404, 836], 0.91, 1200, [[0, 1200]], [], False),
    (11, "pallet", [1070, 536, 1190, 606], 0.83, 980, [[300, 1200]], [], False),
    (2, "person", [1456, 436, 1604, 824], 0.90, 230, [[540, 700], [1010, 1080]], [[1610, 655], [1560, 650]], True),
]
video = {
    "kind": "adkVisionAnswer",
    "version": 1,
    "tool": "vision_detect",
    "callId": "c-881",
    "invocationId": "inv-research-forklift",
    "agent": "research",
    "at": "2026-11-20T06:02:00Z",
    "image": {
        "digest": VFRAME,
        "width": WH[0],
        "height": WH[1],
        "mimeType": "image/png",
        "artifact": "vision-" + VFRAME[7:] + ".png",
        "name": "warehouse-2026-11-12.mp4",
        "provenance": {"kind": "derived", "from": sha("warehouse-2026-11-12.mp4"), "operation": sha("frame:19023")},
    },
    "args": {"image": "warehouse-2026-11-12.mp4", "classes": ["forklift", "pallet", "person"]},
    "regionSet": {
        "image": VFRAME,
        "width": WH[0],
        "height": WH[1],
        "regions": [
            {"image": VFRAME, "shape": box(b), "label": label, "score": score, "track": tid, "origin": origin("warehouse-rfdetr-m", "warehouse-rfdetr-m", LETTERBOX_VIDEO)}
            for tid, label, b, score, *_ in TRACKS
        ],
    },
    "calls": [
        local_call("detect", "warehouse-rfdetr-m", 48, 4),
        {**local_call("track", "bytetrack", 2, 4), "library": "trackforge 0.5"},
    ],
    "models": {"warehouse-rfdetr-m": {"builtIn": False, "name": "RF-DETR M"}},
    "tracks": [
        {"id": tid, "label": label, "frames": frames, "spans": spans, "trail": trail, "blurred": blurred}
        for tid, label, _b, _s, frames, spans, trail, blurred in TRACKS
    ],
    "video": {"frame": 19023, "atS": 634.1, "lengthS": 1200, "sampledPerS": 5},
    "display": {
        "title": "Warehouse aisle 3 · 10:34.1",
        "meta": "warehouse-2026-11-12.mp4 · frame 19,023 · 1920 × 1080",
        "run": "Research · “When does the forklift enter the aisle?” · 09:02",
        "minScore": 0.6,
    },
}

for name, record in [
    ("document-a", document_a),
    ("compare-a", compare_a),
    ("compare-b", compare_b),
    ("photo", photo),
    ("empty", empty),
    ("video", video),
]:
    with open(f"{name}.answer.json", "w", encoding="utf-8") as out:
        json.dump(record, out, ensure_ascii=False, indent=1)
        out.write("\n")
