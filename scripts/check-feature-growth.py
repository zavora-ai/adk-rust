#!/usr/bin/env python3
"""Gate the growth of the `adk-rust` feature surface.

`adk-rust` is both a re-export facade and a dependency menu, so every new
capability arrives as a new top-level feature. At 143 features the menu is
longer than the documentation that explains it, and the axes it grew along
(`graph-*`, `action-*`, `vertex-*`, `cli-*`, `audio-*`) are enforced nowhere.

A feature earns its place when turning it off changes what links. Turning off
a feature that enables no dependency of its own changes nothing at build time —
it is a runtime choice spelled as a compile-time switch, and it belongs in
configuration. This check resolves each feature to the set of optional
dependencies it activates and fails the build when a new feature adds no
dependency without being explicitly grandfathered.

Two rules:

  R1  A feature that activates no optional dependency is grandfathered or
      rejected. Grandfathered entries live in `ALIASES` with a reason and a
      replacement; a new entry is a runtime choice that should be configuration.
  R2  A feature that is an alias of another feature (identical dependency
      effect) must be grandfathered with a reason, so the duplication is a
      recorded decision rather than an accident.

Usage:

    scripts/check-feature-growth.py            # gate, exit 1 on violation
    scripts/check-feature-growth.py --report   # print the surface by axis
    scripts/check-feature-growth.py --write-baseline   # regenerate ALIASES
"""

from __future__ import annotations

import argparse
import collections
import pathlib
import re
import sys
import tomllib

REPO_ROOT = pathlib.Path(__file__).resolve().parents[1]
UMBRELLA = "adk-rust"

# Decision record for features that share a dependency effect with a sibling.
#
# Identical dependency effect is not automatically a defect. Four LLM providers
# may link the same client libraries and still be four distinct clients. What
# matters is that the duplication is a decision someone wrote down, so a new
# duplicate cannot arrive by accident.
#
# kind is one of:
#   canonical  - the member of the group that keeps the axis name
#   collapse   - a convenience alias; fold into the canonical at the next major
#   keep       - a distinct capability that happens to link the same crates
#
# Entries are (kind, reason, replacement).
ALIASES: dict[str, tuple[str, str, str]] = {
    # --- OpenAI-compatible presets: one client, one wire format. -------------
    "openai": ("canonical", "the OpenAI client every preset builds on", "openai"),
    "cerebras": ("collapse", "preset constructor", "OpenAICompatibleConfig::cerebras()"),
    "fireworks": ("collapse", "preset constructor", "OpenAICompatibleConfig::fireworks()"),
    "mistral": ("collapse", "preset constructor", "OpenAICompatibleConfig::mistral()"),
    "perplexity": ("collapse", "preset constructor", "OpenAICompatibleConfig::perplexity()"),
    "sambanova": ("collapse", "preset constructor", "OpenAICompatibleConfig::sambanova()"),
    "together": ("collapse", "preset constructor", "OpenAICompatibleConfig::together()"),
    # --- ONNX checkpoints: one `ort` runtime, model chosen at run time. -------
    "audio-onnx": ("canonical", "links the ort runtime every model needs", "audio-onnx"),
    "whisper-onnx": ("collapse", "checkpoint, not a build", "audio-onnx"),
    "distil-whisper": ("collapse", "checkpoint, not a build", "audio-onnx"),
    "moonshine": ("collapse", "checkpoint, not a build", "audio-onnx"),
    "chatterbox": ("collapse", "checkpoint, not a build", "audio-onnx"),
    # --- Action node kinds adding nothing over `action`. ---------------------
    "action": ("canonical", "the action node family", "action"),
    "action-code": ("collapse", "no dependency beyond `action`", "action"),
    "action-db": ("collapse", "no dependency beyond `action`", "action"),
    "action-email": ("collapse", "no dependency beyond `action`", "action"),
    # --- Surface served by adk-server. ---------------------------------------
    "server": ("canonical", "the Axum server and its protocol surface", "server"),
    "a2a": ("collapse", "A2A needs no dependency beyond adk-server", "server"),
    "agent-registry": ("collapse", "YAML registry needs no dependency beyond adk-server", "server"),
    # --- Agent modes over one crate. -----------------------------------------
    "agents": ("canonical", "the agent crate", "agents"),
    "codeact": ("keep", "a distinct agent mode that links only adk-agent", "codeact"),
    "record-payloads": ("keep", "behavioural switch, no new dependency", "record-payloads"),
    # --- Distinct providers over one client library. --------------------------
    "azure-ai": ("keep", "distinct client, shares the reqwest stack", "azure-ai"),
    "deepseek": ("keep", "distinct client, shares the reqwest stack", "deepseek"),
    "groq": ("keep", "distinct client, shares the reqwest stack", "groq"),
    "openrouter": ("keep", "distinct client with routing and credits APIs", "openrouter"),
    # --- Adjacent surfaces reached through one crate. ------------------------
    "default": ("canonical", "Cargo's implicit entry point; resolves to minimal", "minimal"),
    "minimal": ("canonical", "the default tier", "minimal"),
    "action-http": ("canonical", "links reqwest; action-rss builds on it", "action-http"),
    "action-rss": ("collapse", "an HTTP node reading a feed", "action-http"),
    "audio": ("canonical", "the audio crate", "audio"),
    "audio-fx": ("keep", "mixing and effects, no dependency beyond adk-audio", "audio-fx"),
    "cli": ("canonical", "the CLI and its providers", "cli"),
    "optimize": ("keep", "prompt optimizer, links the CLI only", "optimize"),
    "cli-deepseek": ("collapse", "CLI provider read from the environment", "cli"),
    "cli-groq": ("collapse", "CLI provider read from the environment", "cli"),
    "eval": ("canonical", "the evaluation crate", "eval"),
    "personas": ("collapse", "persona fixtures, links adk-eval only", "eval"),
    "mcp": ("canonical", "the MCP transport", "mcp"),
    "mcp-sampling": ("keep", "sampling needs nothing beyond rmcp", "mcp"),
    "gemini": ("canonical", "the Gemini client", "gemini"),
    "gemini-interactions": ("keep", "a distinct Beta transport on the same client", "gemini-interactions"),
    "graph": ("canonical", "the graph crate", "graph"),
    "graph-time-travel": ("keep", "pure logic, no dependency", "graph-time-travel"),
    "sqlite-memory": ("canonical", "the default memory backend", "sqlite-memory"),
    "memory": ("keep", "the memory trait surface, links the default backend", "memory"),
    "sessions": ("keep", "the session trait surface, links the default backend", "sessions"),
    "postgres-session": ("keep", "a distinct database client", "postgres-session"),
    "realtime": ("canonical", "the realtime crate", "realtime"),
    "video-avatar": ("keep", "avatar providers over the realtime transports", "video-avatar"),
    "example-store": ("keep", "few-shot store, distinct from the Agent Registry", "example-store"),
    "vertex-agent-registry": ("keep", "Agent Registry, distinct from the Example Store", "vertex-agent-registry"),
    "vertex-rag": ("keep", "RAG Engine retrieval", "vertex-rag"),
    "agent-retrieval": ("keep", "managed VectorStore, distinct from RAG Engine", "agent-retrieval"),
}

# Axes, by feature prefix. A new feature is expected to extend an existing axis
# or open a new one here rather than land as an ungrouped top-level flag.
AXES: dict[str, str] = {
    "cli": "cli",
    "graph": "graph",
    "action": "action",
    "vertex": "gcp",
    "gcp": "gcp",
    "audio": "media",
    "whisper": "media",
    "distil": "media",
    "moonshine": "media",
    "kokoro": "media",
    "chatterbox": "media",
    "qwen3": "media",
    "all-onnx": "media",
    "mcp": "mcp",
}

TIERS = {
    "default",
    "minimal",
    "standard",
    "enterprise",
    "full",
    "gemini-agent-platform",
    "gemini-agent-platform-full",
}

CHOOSE_ONE = re.compile(
    r"^(gemini|gemini-vertex|gemini-interactions|openai|anthropic|anthropic-client"
    r"|deepseek|groq|ollama|bedrock|azure-ai|postgres-session|redis-session"
    r"|mongodb-session|firestore-session|neo4j-session|sqlite-memory|database-memory"
    r"|redis-memory|mongodb-memory|neo4j-memory|auth-bridge|openai-realtime"
    r"|vertex-live|livekit|openai-webrtc)$"
)


class Workspace:
    """Resolves umbrella features to the optional dependencies they activate."""

    def __init__(self, root: pathlib.Path) -> None:
        self.root = root
        self._manifests: dict[str, dict] = {}

    def manifest(self, crate: str) -> dict:
        if crate not in self._manifests:
            path = self.root / crate / "Cargo.toml"
            self._manifests[crate] = (
                tomllib.load(open(path, "rb")) if path.exists() else {}
            )
        return self._manifests[crate]

    def features(self, crate: str) -> dict:
        return self.manifest(crate).get("features", {})

    def optional_deps(self, crate: str) -> set[str]:
        manifest = self.manifest(crate)
        names: set[str] = set()
        for section in ("dependencies", "build-dependencies"):
            for name, spec in (manifest.get(section) or {}).items():
                if isinstance(spec, dict) and spec.get("optional"):
                    names.add(name)
        return names

    def deps_of(
        self, crate: str, feature: str, stack: tuple = ()
    ) -> frozenset[str]:
        """Optional dependencies a feature activates, following local and
        `dep/feature` edges. Cycles terminate."""
        if (crate, feature) in stack:
            return frozenset()
        inner = stack + ((crate, feature),)
        activated: set[str] = set()
        features = self.features(crate)
        for item in features.get(feature, []):
            if item.startswith("dep:"):
                activated.add(item[4:])
            elif "/" in item:
                target, target_feature = item.split("/", 1)
                if target == "dep":
                    target = crate
                activated |= self.deps_of(target, target_feature, inner)
            elif item in self.optional_deps(crate):
                activated.add(item)
            elif item in features:
                activated |= self.deps_of(crate, item, inner)
        return frozenset(activated)


def axis_of(feature: str) -> str:
    if feature in TIERS:
        return "tier"
    for prefix, name in AXES.items():
        if feature.startswith(prefix):
            return name
    if CHOOSE_ONE.match(feature):
        return "choose-one"
    return "capability"


def analyse(workspace: Workspace) -> dict:
    features = workspace.features(UMBRELLA)
    deps = {name: workspace.deps_of(UMBRELLA, name) for name in features}

    by_effect: dict[frozenset, list[str]] = collections.defaultdict(list)
    for name, effect in deps.items():
        by_effect[effect].append(name)

    aliases: dict[str, list[str]] = {}
    for effect, names in by_effect.items():
        if len(names) > 1:
            for name in names:
                aliases[name] = sorted(n for n in names if n != name)

    axes: dict[str, list[str]] = collections.defaultdict(list)
    for name in features:
        axes[axis_of(name)].append(name)

    return {
        "features": features,
        "deps": deps,
        "aliases": aliases,
        "axes": axes,
        "by_effect": by_effect,
    }


def report(result: dict) -> None:
    total = len(result["features"])
    print(f"{UMBRELLA} feature surface: {total} features\n")
    for axis in sorted(result["axes"], key=lambda a: (-len(result["axes"][a]), a)):
        names = sorted(result["axes"][axis])
        print(f"  {axis:<12} {len(names):>3}  {', '.join(names)}")

    dead = sorted(n for n, d in result["deps"].items() if not d)
    dupes = {k: v for k, v in result["aliases"].items() if len(v) > 1}
    print(f"\nno dependency effect: {len(dead)}  {dead}")
    print(f"shared dependency effect: {len(dupes)} of {total}")
    plan(result)


def plan(result: dict) -> None:
    """The reduction the decision record implies."""
    kinds = {name: entry[0] for name, entry in ALIASES.items()}
    groups: dict[frozenset, list[str]] = collections.defaultdict(list)
    for name, twins in result["aliases"].items():
        if len(twins) > 1:
            groups[frozenset([name, *twins])].append(name)

    collapse = sorted(
        (name, ALIASES[name][2])
        for name, kind in kinds.items()
        if kind == "collapse" and name in result["features"]
    )
    keep = sorted(name for name, kind in kinds.items() if kind == "keep")

    print(f"\ncollapse at the next major: {len(collapse)}")
    for name, replacement in collapse:
        print(f"    {name:<18} -> {replacement}")
    print(f"\nkept despite a shared dependency effect: {len(keep)}")
    for name in keep:
        print(f"    {name:<18} {ALIASES[name][1]}")


def check(result: dict) -> int:
    """Returns the number of violations."""
    violations: list[str] = []

    for name in sorted(result["aliases"]):
        if name in ALIASES:
            continue
        twins = result["aliases"][name]
        violations.append(
            f"R2 {name}: same dependency effect as {', '.join(twins)}. "
            "Record the decision: if it is a convenience alias, add it to "
            "ALIASES with kind='collapse' and a replacement; if it is a distinct "
            "capability, add it with kind='keep' and a reason."
        )

    for name, effect in sorted(result["deps"].items()):
        if effect or name in ALIASES or name in TIERS:
            continue
        violations.append(
            f"R1 {name}: enables no optional dependency. A feature should exist "
            "only if turning it off changes what links. Move it to configuration, "
            "or add it to ALIASES if it is a deliberate behavioural switch."
        )

    for name in sorted(ALIASES):
        if name not in result["features"]:
            violations.append(
                f"R3 ALIASES lists {name}, which is no longer a feature. "
                "Remove the entry."
            )

    for violation in violations:
        print(violation, file=sys.stderr)

    if violations:
        print(f"\n{len(violations)} violation(s).", file=sys.stderr)
        return len(violations)
    return 0


def write_baseline(result: dict) -> int:
    """Emits ALIASES entries for every current duplicate, for the removal PR."""
    for name in sorted(result["aliases"]):
        kind, reason, replacement = ALIASES.get(name, ("keep", "<reason>", "<replacement>"))
        twins = ", ".join(result["aliases"][name])
        print(
            f'    "{name}": ("{kind}", "{reason}; shares an effect with {twins}", '
            f'"{replacement}"),'
        )
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--report", action="store_true", help="print the surface")
    parser.add_argument(
        "--write-baseline",
        action="store_true",
        help="emit ALIASES entries for the current surface",
    )
    args = parser.parse_args()

    workspace = Workspace(REPO_ROOT)
    if not workspace.manifest(UMBRELLA):
        print(f"{UMBRELLA}/Cargo.toml not found under {REPO_ROOT}", file=sys.stderr)
        return 2

    result = analyse(workspace)
    if args.write_baseline:
        return write_baseline(result)
    if args.report:
        report(result)
        return 0
    return 1 if check(result) else 0


if __name__ == "__main__":
    sys.exit(main())
