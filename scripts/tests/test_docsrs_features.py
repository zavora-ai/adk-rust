"""Every feature a crate's docs.rs table names exists, so a renamed or removed feature
cannot break the crate's documentation build after it is published."""

from __future__ import annotations

import tomllib
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]


def dependencies(manifest: dict) -> dict[str, dict | str]:
    """Normal and build dependencies, including target-specific ones."""
    found: dict[str, dict | str] = {}
    tables = [manifest] + list(manifest.get("target", {}).values())
    for table in tables:
        for section in ("dependencies", "build-dependencies"):
            found.update(table.get(section, {}))
    return found


def unknown_docsrs_features(manifest: dict) -> list[str]:
    """The docs.rs features that name neither a feature nor an implicit optional-dependency feature."""
    listed = manifest.get("package", {}).get("metadata", {}).get("docs", {}).get("rs", {})
    features = manifest.get("features", {})
    deps = dependencies(manifest)
    explicit = {item[4:] for values in features.values() for item in values if item.startswith("dep:")}
    implicit = {
        name
        for name, spec in deps.items()
        if isinstance(spec, dict) and spec.get("optional") and name not in explicit
    }
    unknown = []
    for feature in listed.get("features", []):
        if "/" in feature:
            if feature.split("/", 1)[0].rstrip("?") not in deps:
                unknown.append(feature)
        elif feature not in features and feature not in implicit:
            unknown.append(feature)
    return unknown


class DocsRsFeatureTests(unittest.TestCase):
    def test_every_listed_feature_exists(self) -> None:
        problems = {}
        for path in sorted(ROOT.glob("*/Cargo.toml")):
            unknown = unknown_docsrs_features(tomllib.loads(path.read_text()))
            if unknown:
                problems[str(path.relative_to(ROOT))] = unknown
        self.assertEqual(problems, {})

    def test_a_missing_feature_is_reported(self) -> None:
        manifest = tomllib.loads(
            """
            [package.metadata.docs.rs]
            features = ["web", "gone", "serde/derive", "absent/feature", "extra"]

            [features]
            web = ["dep:axum"]

            [dependencies]
            axum = { version = "0.8", optional = true }
            serde = "1"
            extra = { version = "1", optional = true }
            """
        )
        self.assertEqual(unknown_docsrs_features(manifest), ["gone", "absent/feature"])


if __name__ == "__main__":
    unittest.main()
