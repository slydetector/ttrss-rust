#!/usr/bin/env python3
"""Verify the golden dataset is in sync with the current seed recipe.

The golden dataset (oracle/data/golden-db.sql + db-manifest.json) is
generated data (gitignored), produced by `make oracle-seed` +
`make oracle-snapshot`. It silently goes stale whenever seed.py or
feeds.txt change without a re-seed + re-snapshot — and every
reset/restore/harness/record run then carries that stale data (Sept 2026:
a 5-day-old dump with bare-word label_cache values cascaded SPA crashes
and corpus mismatches).

This gate is invoked by those consumers; it fails fast with the fix
command. It never re-seeds automatically — re-seeding fetches live feeds
and takes minutes, so it must be a deliberate `make oracle-seed`.
"""

import hashlib
import json
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
DATA = ROOT / "oracle" / "data"
MANIFEST = DATA / "db-manifest.json"


def sha256(p: Path) -> str:
    return hashlib.sha256(p.read_bytes()).hexdigest()


def main() -> int:
    if not MANIFEST.exists():
        print(
            "golden dataset missing: no oracle/data/db-manifest.json.\n"
            "Seed + snapshot first:\n"
            "  make oracle-up && make oracle-seed && make oracle-snapshot",
            file=sys.stderr,
        )
        return 1

    try:
        m = json.loads(MANIFEST.read_text())
    except (OSError, json.JSONDecodeError) as e:
        print(f"golden manifest unreadable: {e}", file=sys.stderr)
        return 1

    lineage = m.get("lineage") or {}
    checks = [
        ("seed_sha256", ROOT / "oracle/scripts/seed.py"),
        ("feeds_sha256", ROOT / "oracle/feeds.txt"),
        ("dump_sha256", DATA / "golden-db.sql"),
    ]

    bad = []
    for key, path in checks:
        want = lineage.get(key)
        if not path.exists():
            bad.append(f"{key}: {path.relative_to(ROOT)} is missing")
            continue
        if not want:
            bad.append(
                f"{key}: not recorded in manifest (manifest predates the lineage check)"
            )
            continue
        got = sha256(path)
        if got != want:
            bad.append(
                f"{key}: manifest {want[:12]}… != current {got[:12]}… "
                f"({path.relative_to(ROOT)})"
            )

    if bad:
        print(
            "golden dataset is OUT OF SYNC with the current seed recipe:",
            file=sys.stderr,
        )
        for b in bad:
            print(f"  - {b}", file=sys.stderr)
        print(
            "Fix:  make oracle-seed && make oracle-snapshot\n"
            "(fresh stack: `make oracle-up` first; the dump must also exist "
            "before `oracle-snapshot` records its hash)",
            file=sys.stderr,
        )
        return 1

    print(
        "golden dataset in sync "
        f"(seed {lineage['seed_sha256'][:12]}…, dump {lineage['dump_sha256'][:12]}…)"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
