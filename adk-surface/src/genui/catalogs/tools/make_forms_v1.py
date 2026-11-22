"""Derive genui's forms 1.0 catalog, as an A2UI 1.0 catalog document, from the pinned A2UI 1.0
basic catalog: the forms components and functions, Text's heading variants kept."""
import json, sys, copy

basic_path, out_path, common_out, meta_out, common_src, meta_src, sha = sys.argv[1:8]
basic = json.load(open(basic_path))
FORMS = ["Text", "Row", "Column", "List", "Card", "Tabs", "Divider", "Button", "TextField",
         "CheckBox", "ChoicePicker", "Slider", "DateTimeInput"]
FUNCTIONS = ["required", "regex", "length", "numeric", "email", "formatString", "pluralize",
             "and", "or", "not"]
ID = "https://adk-rust.com/a2ui/catalogs/forms/1.0/catalog.json"

doc = {
    "$schema": basic["$schema"],
    "$id": ID,
    "protocolVersion": "1.0",
    "title": "forms 1.0",
    "description": (
        "The A2UI 1.0 basic catalog without Image, Icon, Video, AudioPlayer and Modal, the openUrl, "
        "formatNumber, formatCurrency and formatDate functions and the obscured TextField, with Text's "
        "h1 to h5 variants: what "
        "an ADK-Rust agent draws in (spec 3.3 §6.1 genui). Derived from a2ui-project/a2ui@" + sha + "."
    ),
    "catalogId": ID,
    "instructions": (
        "Draw with the components below. A Button dispatches only an event named after one of the "
        "drawing's declared actions; pressing it asks the person before any tool runs, on the "
        "home's own card. A drawing never shows a decision's words (Approve, Allow once, Deny), "
        "secrets or links, and never speaks as the person (no userMessage).\n\n"
        + basic["instructions"].split("## Examples")[0].rstrip()
    ),
    "components": {name: copy.deepcopy(basic["components"][name]) for name in FORMS},
    "functions": {name: copy.deepcopy(basic["functions"][name]) for name in FUNCTIONS},
    "$defs": {},
}
variant = doc["components"]["Text"]["properties"]["variant"]
variant["enum"] = ["h1", "h2", "h3", "h4", "h5"] + [v for v in variant["enum"] if v not in ("h1", "h2", "h3", "h4", "h5")]
# A drawn form never asks for a secret: no obscured text field.
def drop_obscured(node):
    if isinstance(node, dict):
        if node.get("enum") and "obscured" in node["enum"]:
            node["enum"] = [v for v in node["enum"] if v != "obscured"]
        for value in node.values():
            drop_obscured(value)
    elif isinstance(node, list):
        for item in node:
            drop_obscured(item)
drop_obscured(doc["components"]["TextField"])
assert "obscured" not in json.dumps(doc["components"]["TextField"])
# anyComponent / anyFunction over what is kept; Weight as basic has it.
any_component = copy.deepcopy(basic["$defs"]["anyComponent"])
def keep_refs(node, names, prefix):
    if isinstance(node, dict):
        for key in ("oneOf", "anyOf"):
            if key in node:
                node[key] = [item for item in node[key]
                             if not (isinstance(item, dict) and "$ref" in item and item["$ref"].startswith(prefix)
                                     and item["$ref"][len(prefix):] not in names)]
                for item in node[key]:
                    keep_refs(item, names, prefix)
        for key, value in node.items():
            if key not in ("oneOf", "anyOf"):
                keep_refs(value, names, prefix)
    elif isinstance(node, list):
        for item in node:
            keep_refs(item, names, prefix)
keep_refs(any_component, set(FORMS), "#/components/")
any_function = copy.deepcopy(basic["$defs"]["anyFunction"])
keep_refs(any_function, set(FUNCTIONS), "#/functions/")
doc["$defs"] = {"anyComponent": any_component, "anyFunction": any_function, "Weight": basic["$defs"]["Weight"]}
# No reference may point at something left out.
text = json.dumps(doc)
for left in ["Image", "Icon", "Video", "AudioPlayer", "Modal", "openUrl", "formatNumber", "formatCurrency", "formatDate"]:
    assert f"#/components/{left}\"" not in text and f"#/functions/{left}\"" not in text, left
json.dump(doc, open(out_path, "w"), indent=2, ensure_ascii=False)
open(out_path, "a").write("\n")
for src, dst in ((common_src, common_out), (meta_src, meta_out)):
    open(dst, "w").write(open(src).read())
print("components", len(doc["components"]), "functions", len(doc["functions"]))
print("anyComponent refs", text.count("#/components/"))
