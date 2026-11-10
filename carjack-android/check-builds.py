#!/usr/bin/env python3
"""Checks what compiling Slipstream and Motopod won't catch: the activities and services in
app/src, which both apps share, declared in both manifests (left out of one, it crashes only when
started), and every directory of sources in app/ built by one of the apps. Exits 1, listing what's
wrong."""

import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent
APP = ROOT / "app"
problems = []

built = set()
for script in (ROOT / "gradle").glob("*/build.gradle.kts"):
    built |= {d.split("/")[0] for d in re.findall(r'app\.resolve\("([^"]+)"\)', script.read_text())}
for d in sorted(APP.iterdir()):
    if d.is_dir() and d.name not in built and any(d.rglob("*.kt")):
        problems.append(f"app/{d.name} has Kotlin sources neither app builds")

manifests = {"Motopod": APP / "AndroidManifest.xml", "Slipstream": APP / "generic/AndroidManifest.xml"}
declared = {
    app: set(re.findall(r'<(?:activity|service|receiver|provider)\s[^>]*?android:name="[^"]*?\.?(\w+)"',
                        path.read_text(), re.S))
    for app, path in manifests.items()
}
for source in sorted((APP / "src").rglob("*.kt")):
    for component in re.findall(r"^class (\w+)\s*:\s*\w*(?:Activity|Service|Receiver|Provider)\(",
                                source.read_text(), re.M):
        for app, names in declared.items():
            if component not in names:
                problems.append(f"{component} (app/src) isn't in {app}'s manifest")

for problem in problems:
    print(problem, file=sys.stderr)
sys.exit(1 if problems else 0)
