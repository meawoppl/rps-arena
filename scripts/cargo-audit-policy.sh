#!/usr/bin/env bash
set -euo pipefail

report="$(mktemp)"
trap 'rm -f "$report"' EXIT

cargo audit --json > "$report"

python3 - "$report" <<'PY'
import json
import sys

report_path = sys.argv[1]
with open(report_path, "r", encoding="utf-8") as f:
    report = json.load(f)

vulnerabilities = report.get("vulnerabilities", {})
if vulnerabilities.get("found"):
    for vuln in vulnerabilities.get("list", []):
        advisory = vuln.get("advisory", {})
        package = vuln.get("package", {})
        print(
            f"vulnerability: {advisory.get('id', '<unknown>')} "
            f"{package.get('name', '<unknown>')} {package.get('version', '')}: "
            f"{advisory.get('title', '<no title>')}",
            file=sys.stderr,
        )
    sys.exit(1)

# These are tracked warnings from current frontend/transitive dependencies.
# New warning IDs or yanked packages must fail CI until they are fixed or added
# here with a specific rationale.
allowed_warning_advisories = {
    # Pulled through gloo/yew; no direct backend use.
    "RUSTSEC-2025-0141",
    # Pulled through yew-macro; build-time only.
    "RUSTSEC-2024-0370",
    # SQLite-only Diesel advisory; this service uses PgConnection.
    "RUSTSEC-2026-0172",
    # Transitive rand 0.8 warning; no custom logger calls rand::thread_rng.
    "RUSTSEC-2026-0097",
}

allowed_yanked = {
    ("js-sys", "0.3.88"),
    ("wasm-bindgen", "0.2.111"),
}

unknown = []
warnings = report.get("warnings", {})
for category, entries in warnings.items():
    for entry in entries:
        package = entry.get("package", {})
        advisory = entry.get("advisory") or {}
        advisory_id = advisory.get("id")

        if advisory_id:
            if advisory_id not in allowed_warning_advisories:
                unknown.append(
                    f"{category}: {advisory_id} "
                    f"{package.get('name', '<unknown>')} {package.get('version', '')}"
                )
            continue

        package_key = (package.get("name"), package.get("version"))
        if category == "yanked" and package_key in allowed_yanked:
            continue

        unknown.append(
            f"{category}: {package.get('name', '<unknown>')} {package.get('version', '')}"
        )

if unknown:
    print("cargo audit reported unapproved warnings:", file=sys.stderr)
    for item in unknown:
        print(f"  - {item}", file=sys.stderr)
    sys.exit(1)

print("cargo audit policy passed")
PY
