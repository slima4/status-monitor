#!/usr/bin/env python3
"""Guard the GitHub issue forms against silent drift.

Catches the four cross-boundary contracts that no YAML linter sees:
  1. self-referencing github.com URLs that no longer point at this repo
     (stale after a rename -> security reports route to a 404)
  2. `labels:` strings that don't exist as repo labels
     (GitHub silently skips them -> issues never get auto-labeled)
  3. `area` dropdown options with no mapped repo label
     (a dropdown choice does NOT auto-apply a label -> unrouteable issues)
  4. issue-form schema violations GitHub rejects only at render time
     (e.g. `validations` on a markdown block -> the whole form 404s, and
      with blank issues disabled, NO issue can be filed)

Usage: validate-issue-templates.py <owner/repo> <labels.json>
  labels.json is the output of: gh label list --limit 200 --json name
"""
import json
import re
import sys
from pathlib import Path

import yaml

TPL_DIR = Path(".github/ISSUE_TEMPLATE")
AREA_MAP_FILE = Path(".github/issue-area-labels.yml")

VALID_TYPES = {"markdown", "textarea", "input", "dropdown", "checkboxes"}
NEEDS_OPTIONS = {"dropdown", "checkboxes"}
VALIDATIONS_OK = {"input", "textarea", "dropdown"}
# Options that intentionally map to no label — triage decides.
AREA_NO_LABEL = {"Not sure", "Other"}
# Path prefixes that are self-references to this repo (vs. external links).
SELF_REF_SEGMENTS = {"security", "discussions", "issues", "pull", "blob", "tree", "wiki"}

URL_RE = re.compile(r"https://github\.com/([\w.-]+/[\w.-]+)/([\w.-]+)")


def main() -> int:
    if len(sys.argv) != 3:
        print("usage: validate-issue-templates.py <owner/repo> <labels.json>", file=sys.stderr)
        return 2
    repo = sys.argv[1]
    labels = {l["name"] for l in json.loads(Path(sys.argv[2]).read_text())}
    area_map = yaml.safe_load(AREA_MAP_FILE.read_text()) or {}

    errors: list[str] = []

    # --- check 3 prep: mapped labels must exist; map must be complete both ways
    for opt, label in area_map.items():
        if label not in labels:
            errors.append(f"{AREA_MAP_FILE}: option {opt!r} maps to label {label!r} which does not exist in repo")
    dropdown_area_options: set[str] = set()

    for path in sorted(TPL_DIR.glob("*.yml")):
        text = path.read_text()
        doc = yaml.safe_load(text)

        # --- check 1: self-referencing URLs point at the current repo
        for slug, seg in URL_RE.findall(text):
            if seg in SELF_REF_SEGMENTS and slug != repo:
                errors.append(f"{path.name}: URL github.com/{slug}/{seg}... does not match repo {repo!r} (stale after rename?)")

        if path.name == "config.yml":
            continue  # config has no body/labels/areas to schema-check

        # --- check 2: top-level labels exist
        for k in ("name", "description", "body"):
            if k not in doc:
                errors.append(f"{path.name}: missing required top-level key {k!r}")
        for label in doc.get("labels", []):
            if label not in labels:
                errors.append(f"{path.name}: auto-label {label!r} does not exist in repo (GitHub would silently skip it)")

        # --- check 4: per-field schema
        seen_ids: set[str] = set()
        for i, item in enumerate(doc.get("body", [])):
            where = f"{path.name} body[{i}]"
            t = item.get("type")
            if t not in VALID_TYPES:
                errors.append(f"{where}: invalid type {t!r}")
                continue
            iid = item.get("id")
            if iid is not None:
                if iid in seen_ids:
                    errors.append(f"{where}: duplicate id {iid!r} within this form")
                seen_ids.add(iid)
            if "validations" in item and t not in VALIDATIONS_OK:
                errors.append(f"{where}: type {t!r} must not carry 'validations' (GitHub rejects the whole form)")
            attrs = item.get("attributes", {})
            if t in NEEDS_OPTIONS and not attrs.get("options"):
                errors.append(f"{where}: type {t!r} requires non-empty 'options'")
            if t == "markdown" and not attrs.get("value"):
                errors.append(f"{where}: markdown block requires 'value'")

            # --- check 3: collect area dropdown options
            if t == "dropdown" and iid == "area":
                for opt in attrs.get("options", []):
                    dropdown_area_options.add(opt)
                    if opt in AREA_NO_LABEL:
                        continue
                    if opt not in area_map:
                        errors.append(f"{where}: area option {opt!r} has no entry in {AREA_MAP_FILE.name} (add it + create the label)")

    # --- check 3 reverse: no stale map entries pointing at removed options
    for opt in area_map:
        if opt not in dropdown_area_options:
            errors.append(f"{AREA_MAP_FILE}: entry {opt!r} matches no 'area' dropdown option (stale after a form edit?)")

    if errors:
        print(f"issue-template guard: {len(errors)} problem(s)\n", file=sys.stderr)
        for e in errors:
            print(f"  ✗ {e}", file=sys.stderr)
        return 1
    print("issue-template guard: all checks passed")
    return 0


if __name__ == "__main__":
    sys.exit(main())
