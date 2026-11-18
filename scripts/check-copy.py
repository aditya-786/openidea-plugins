#!/usr/bin/env python3
"""Copy consistency checks for plugin command titles and Rust message strings."""
import json
import re
import sys
import tomllib
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent

# plugin id suffix -> extra title prefixes it may use besides one dominant prefix
# one structure window bundles three Spring products, each keeping its own prefix
ALLOWED_EXTRA_PREFIXES: dict[str, set[str]] = {
    "spring-integration": {"Spring Batch", "Spring Integration", "Spring Modulith"},
}

ALLOWED_EXTRA_CATEGORIES: dict[str, set[str]] = {
    "spring-integration": {"Spring Batch", "Spring Integration", "Spring Modulith"},
}

WORDS = r"at most|up to|exceeds|than|max|the|Java|Quarkus|provisioned|within|its|most|to|of|about|or|depth|Python"
UNITS = {"KiB", "MiB", "GiB", "TiB", "KB", "MB", "GB", "TB"}
GLUED_WORD = re.compile(rf"\b(?:{WORDS})\d")
GLUED_UNIT = re.compile(r"[A-Za-z]\d+(?:KiB|MiB|bytes|lines|minutes|seconds)\b")
GLUED_SPACED_UNIT = re.compile(r"[a-z]{3,}\d{2,} (?:seconds|minutes|bytes|lines|KiB|MiB)\b")
DIGITS_THEN_WORD = re.compile(r"(?<=\s)\d+([A-Za-z]{3,})")


def glued(literal: str) -> bool:
    if GLUED_WORD.search(literal) or GLUED_UNIT.search(literal) or GLUED_SPACED_UNIT.search(literal):
        return True
    if literal.count(" ") < 3:
        return False
    return any(m.group(1) not in UNITS for m in DIGITS_THEN_WORD.finditer(literal))

STRING = re.compile(r'"(?:[^"\\\n]|\\.)*"')

problems: list[str] = []

MAX_CHANGE_NOTES = 1200
ONLY = set(sys.argv[2].split(",")) if len(sys.argv) > 2 and sys.argv[1] == "--plugins" else None

for toml in sorted(ROOT.glob("plugins/*/plugin.toml")):
    name = toml.parent.name
    if ONLY is not None and name not in ONLY:
        continue
    manifest = tomllib.loads(toml.read_text())
    notes = manifest.get("change-notes", "")
    if len(notes) > MAX_CHANGE_NOTES:
        problems.append(f"{name}: change-notes is {len(notes)} chars (max {MAX_CHANGE_NOTES})")
    if "Unreleased:" in notes:
        problems.append(f"{name}: change-notes contains 'Unreleased:' (use 'Next release:')")
    next_release = re.match(r"Next release: (.*?)(?:\. \d+\.\d+\.\d+:|\.?$)", notes, re.S)
    if next_release and len(next_release.group(1).split("; ")) > 5:
        problems.append(f"{name}: 'Next release' has more than 5 items")
    commands = manifest.get("contributes", {}).get("commands", [])
    prefixes: dict[str, list[str]] = {}
    for c in commands:
        title = c.get("title", "")
        if "..." in title:
            problems.append(f"{name}: '...' in title {title!r} (use …)")
        if not c.get("category"):
            problems.append(f"{name}: command {c.get('id')!r} has no category")
        prefix = title.split(": ", 1)[0] if ": " in title else "(none)"
        prefixes.setdefault(prefix, []).append(title)
    categories = {c.get("category") for c in commands if c.get("category")}
    if len(categories - ALLOWED_EXTRA_CATEGORIES.get(name, set())) > 1:
        problems.append(f"{name}: multiple command categories {sorted(categories)}")
    titles: dict[str, list[str]] = {}
    for c in commands:
        titles.setdefault(c.get("title", ""), []).append(c.get("id", ""))
    for title, ids in titles.items():
        if len(ids) > 1:
            problems.append(f"{name}: commands {ids} share the title {title!r}")
    allowed = ALLOWED_EXTRA_PREFIXES.get(name, set())
    found = {p for p in prefixes if p not in allowed}
    if len(found) > 1:
        listing = "; ".join(f"{p}: {len(t)}" for p, t in prefixes.items())
        problems.append(f"{name}: multiple title prefixes ({listing})")

MAX_SETTING_TITLE = 120


def leaf_properties(properties: dict):
    for key, value in properties.items():
        if "properties" in value:
            yield from leaf_properties(value["properties"])
        else:
            yield key, value


for schema in sorted(ROOT.glob("plugins/*/settings*.schema.json")):
    for key, value in leaf_properties(json.loads(schema.read_text()).get("properties", {})):
        title = value.get("title")
        if not title:
            problems.append(f"{schema.parent.name}: setting {key!r} has no title")
        elif len(title) > MAX_SETTING_TITLE:
            problems.append(f"{schema.parent.name}: title of {key!r} is {len(title)} chars (max {MAX_SETTING_TITLE}); move detail to description")

rust = list(ROOT.glob("plugins/*/src/**/*.rs")) + list(ROOT.glob("crates/ui-kit/src/**/*.rs"))
for path in sorted(rust):
    for n, line in enumerate(path.read_text().splitlines(), 1):
        for lit in STRING.findall(line):
            if glued(lit):
                problems.append(f"{path.relative_to(ROOT)}:{n}: glued digit in {lit[:90]}")

if problems:
    print("check-copy FAIL")
    print("\n".join(problems))
    sys.exit(1)
print("check-copy PASS")
