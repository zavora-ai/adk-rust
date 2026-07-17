#!/usr/bin/env python3
"""Validate user-facing Cargo examples in README and official docs.

This intentionally validates the things a reader can copy directly:
Cargo dependency snippets, feature names, package/example/bin references, and
standalone example manifest paths. It does not execute commands that require
API keys or external services; compile coverage is provided by the workspace
example and cargo-adk template checks.
"""

from __future__ import annotations

import json
import re
import shlex
import subprocess
import sys
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
DOC_PATHS = [ROOT / "README.md", *sorted((ROOT / "docs" / "official_docs").rglob("*.md"))]
IGNORED_EXTERNAL_CRATES = {
    "adk-studio",
    "adk-ui",
    "google-adk",
}
SKIPPED_CARGO_SUBCOMMANDS = {
    "add",
    "adk",
    "audit",
    "clippy",
    "fmt",
    "install",
    "new",
}


def cargo_metadata(*args: str, cwd: Path = ROOT) -> dict:
    proc = subprocess.run(
        ["cargo", "metadata", "--no-deps", "--format-version", "1", *args],
        cwd=cwd,
        check=True,
        text=True,
        stdout=subprocess.PIPE,
    )
    return json.loads(proc.stdout)


metadata = cargo_metadata("--manifest-path", str(ROOT / "Cargo.toml"))
packages = {package["name"]: package for package in metadata["packages"]}
workspace_version = packages["adk-rust"]["version"]
features_by_package = {
    name: set(package.get("features", {}).keys()) for name, package in packages.items()
}
targets_by_package: dict[str, dict[str, set[str]]] = {}
examples_by_name: dict[str, list[str]] = {}

for name, package in packages.items():
    targets: dict[str, set[str]] = {"bin": set(), "example": set(), "test": set()}
    for target in package["targets"]:
        for kind in target["kind"]:
            if kind in targets:
                targets[kind].add(target["name"])
                if kind == "example":
                    examples_by_name.setdefault(target["name"], []).append(name)
    targets_by_package[name] = targets

standalone_metadata_cache: dict[Path, dict] = {}
errors: list[str] = []


def rel(path: Path) -> str:
    try:
        return str(path.relative_to(ROOT))
    except ValueError:
        return str(path)


def add_error(path: Path, line: int, message: str) -> None:
    errors.append(f"{rel(path)}:{line}: {message}")


def strip_inline_comment(line: str) -> str:
    in_single = False
    in_double = False
    escaped = False
    out = []
    for char in line:
        if escaped:
            out.append(char)
            escaped = False
            continue
        if char == "\\":
            out.append(char)
            escaped = True
            continue
        if char == "'" and not in_double:
            in_single = not in_single
        elif char == '"' and not in_single:
            in_double = not in_double
        elif char == "#" and not in_single and not in_double:
            break
        out.append(char)
    return "".join(out).strip()


def shell_commands(block: str) -> list[str]:
    commands: list[str] = []
    # Join shell line-continuations (a trailing `\`) into one logical command
    # so `shlex.split` does not choke on a dangling backslash.
    logical_lines: list[str] = []
    buffer = ""
    for raw in block.splitlines():
        stripped = raw.rstrip()
        if stripped.endswith("\\") and not stripped.endswith("\\\\"):
            buffer += stripped[:-1].rstrip() + " "
            continue
        logical_lines.append((buffer + stripped).strip())
        buffer = ""
    if buffer.strip():
        logical_lines.append(buffer.strip())

    for line in logical_lines:
        if not line or line.startswith("#") or line.startswith("export ") or line.startswith("cp "):
            continue
        for part in re.split(r"\s+&&\s+", line):
            command = strip_inline_comment(part)
            if command:
                commands.append(command)
    return commands


def parse_feature_list(value: str) -> list[str]:
    return re.findall(r'"([^"]+)"', value)


def option_value(args: list[str], long_name: str, short_name: str | None = None) -> str | None:
    for index, arg in enumerate(args):
        if arg == long_name and index + 1 < len(args):
            return args[index + 1]
        if arg.startswith(f"{long_name}="):
            return arg.split("=", 1)[1]
        if short_name and arg == short_name and index + 1 < len(args):
            return args[index + 1]
    return None


def option_values(args: list[str], long_name: str) -> list[str]:
    values: list[str] = []
    for index, arg in enumerate(args):
        if arg == long_name and index + 1 < len(args):
            values.extend(args[index + 1].replace(",", " ").split())
        elif arg.startswith(f"{long_name}="):
            values.extend(arg.split("=", 1)[1].replace(",", " ").split())
    return [value for value in values if value]


def resolve_package_for_example(example: str, package: str | None, path: Path, line: int) -> str | None:
    if package:
        if package not in packages:
            add_error(path, line, f"unknown package `{package}`")
            return None
        return package

    owners = examples_by_name.get(example, [])
    if not owners:
        add_error(path, line, f"unknown workspace example `{example}`; add `-p <package>` or update the docs")
        return None
    if len(owners) > 1:
        add_error(
            path,
            line,
            f"ambiguous example `{example}` belongs to {', '.join(owners)}; document it with `-p`",
        )
        return None
    return owners[0]


def validate_features(package: str, features: list[str], path: Path, line: int) -> None:
    known = features_by_package.get(package, set())
    for feature in features:
        if feature not in known:
            add_error(path, line, f"package `{package}` has no feature `{feature}`")


def manifest_metadata(manifest: Path) -> dict | None:
    manifest = manifest.resolve()
    if manifest not in standalone_metadata_cache:
        try:
            standalone_metadata_cache[manifest] = cargo_metadata("--manifest-path", str(manifest))
        except subprocess.CalledProcessError as exc:
            errors.append(f"{rel(manifest)}:1: cargo metadata failed for documented manifest ({exc})")
            return None
    return standalone_metadata_cache[manifest]


def validate_manifest_target(manifest: Path, bin_name: str | None, path: Path, line: int) -> None:
    if not manifest.exists():
        add_error(path, line, f"documented manifest does not exist: `{rel(manifest)}`")
        return
    if not bin_name:
        return
    doc_metadata = manifest_metadata(manifest)
    if doc_metadata is None:
        return
    bins = {
        target["name"]
        for package in doc_metadata["packages"]
        for target in package["targets"]
        if "bin" in target["kind"]
    }
    if bin_name not in bins:
        add_error(path, line, f"manifest `{rel(manifest)}` has no bin target `{bin_name}`")


def validate_cargo_command(command: str, cwd: Path, path: Path, line: int) -> None:
    if not command.startswith("cargo "):
        return

    try:
        args = shlex.split(command)
    except ValueError as exc:
        add_error(path, line, f"cannot parse shell command `{command}`: {exc}")
        return

    if len(args) < 2 or args[0] != "cargo":
        return

    subcommand = args[1]
    if subcommand in SKIPPED_CARGO_SUBCOMMANDS:
        return
    if subcommand == "nextest" and len(args) > 2:
        subcommand = f"nextest {args[2]}"

    package = option_value(args, "--package", "-p")
    manifest_arg = option_value(args, "--manifest-path")
    example = option_value(args, "--example")
    bin_name = option_value(args, "--bin")
    test_name = option_value(args, "--test")
    features = option_values(args, "--features")

    if manifest_arg:
        manifest = (cwd / manifest_arg).resolve()
        validate_manifest_target(manifest, bin_name, path, line)
        return

    if example:
        owner = resolve_package_for_example(example, package, path, line)
        if owner is None:
            return
        if example not in targets_by_package[owner]["example"]:
            add_error(path, line, f"package `{owner}` has no example target `{example}`")
        validate_features(owner, features, path, line)
        return

    if package:
        if package in IGNORED_EXTERNAL_CRATES:
            return
        if package not in packages:
            add_error(path, line, f"unknown package `{package}`")
            return
        validate_features(package, features, path, line)
        if bin_name and bin_name not in targets_by_package[package]["bin"]:
            add_error(path, line, f"package `{package}` has no bin target `{bin_name}`")
        if test_name and test_name not in targets_by_package[package]["test"]:
            add_error(path, line, f"package `{package}` has no test target `{test_name}`")
        return

    if bin_name:
        manifest = cwd / "Cargo.toml"
        validate_manifest_target(manifest, bin_name, path, line)
        return

    if subcommand == "run":
        manifest = cwd / "Cargo.toml"
        if manifest.exists():
            validate_manifest_target(manifest, None, path, line)


def validate_dependency_line(path: Path, line_no: int, line: str) -> None:
    dep_match = re.match(r"\s*([A-Za-z0-9_-]+)\s*=\s*(.+)", line)
    if not dep_match:
        return
    crate = dep_match.group(1).replace("_", "-")
    if not crate.startswith("adk-") and crate not in {"awp-types", "cargo-adk"}:
        return
    if crate in IGNORED_EXTERNAL_CRATES:
        return
    if crate not in packages:
        return

    value = dep_match.group(2)
    version_match = re.search(r'version\s*=\s*"([^"]+)"', value) or re.search(r'^"([^"]+)"', value)
    if version_match and version_match.group(1) != workspace_version:
        add_error(
            path,
            line_no,
            f"`{crate}` dependency uses version `{version_match.group(1)}`; expected `{workspace_version}`",
        )

    features_match = re.search(r"features\s*=\s*\[([^\]]*)\]", value)
    if features_match:
        validate_features(crate, parse_feature_list(features_match.group(1)), path, line_no)


def validate_doc(path: Path) -> None:
    text = path.read_text()
    if "official_docs_examples" in text:
        for index, line in enumerate(text.splitlines(), start=1):
            if "official_docs_examples" in line:
                add_error(path, index, "references missing `official_docs_examples`; use validated repo examples instead")

    fence_re = re.compile(r"^```([A-Za-z0-9_-]*)[^\n]*\n(.*?)^```", re.M | re.S)
    for match in fence_re.finditer(text):
        language = match.group(1)
        block = match.group(2)
        start_line = text[: match.start()].count("\n") + 1

        if language == "toml":
            for offset, line in enumerate(block.splitlines(), start=1):
                validate_dependency_line(path, start_line + offset, line)

        if language in {"bash", "sh", "shell"}:
            cwd = ROOT
            created_dirs: set[str] = set()
            for offset, command in enumerate(shell_commands(block), start=1):
                line = start_line + offset
                if command.startswith("cd "):
                    target = shlex.split(command)[1]
                    if target in created_dirs:
                        cwd = ROOT / target
                    else:
                        cwd = (cwd / target).resolve()
                    continue
                if command.startswith("cargo new "):
                    created_dirs.add(shlex.split(command)[-1])
                    continue
                if command.startswith("cargo adk new "):
                    parts = shlex.split(command)
                    if len(parts) >= 4:
                        created_dirs.add(parts[3])
                    continue
                validate_cargo_command(command, cwd, path, line)


for doc_path in DOC_PATHS:
    validate_doc(doc_path)

if errors:
    print("documented example validation failed:", file=sys.stderr)
    for error in errors:
        print(f"  {error}", file=sys.stderr)
    sys.exit(1)

print(f"validated documented Cargo examples for {len(DOC_PATHS)} markdown files")
