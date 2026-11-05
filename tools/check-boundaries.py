#!/usr/bin/env python3
"""Check explicit application imports and the ownership of transport/remux code."""
from pathlib import Path
import re
import sys

from ndm_lng import is_test_source, without_tests

ROOT = Path(__file__).resolve().parents[1]
errors = []
for path in sorted((ROOT / "src").rglob("*.rs")):
    if is_test_source(path):
        continue
    source = without_tests(path.read_text(encoding="utf-8"))
    relative = path.relative_to(ROOT).as_posix()
    if re.search(r"\buse\s+(?:super|self|crate)(?:::\w+)*::\*\s*;", source):
        errors.append(f"{relative} must name the symbols imported from application modules")
    if relative.startswith(("src/transport", "src/remux")) and re.search(r"\bcrate::(?:app|core)\b", source):
        errors.append(f"{relative} must receive application policy through its interface")
if errors:
    sys.exit("\n".join(errors))
print("Application module boundaries are explicit")
