#!/usr/bin/env python3
"""Re-baseline the golden manifest's lineage to the current recipe files.

`check_sync.py` fails whenever oracle/scripts/seed.py or oracle/feeds.txt
change, because the golden dump (oracle/data/golden-db.sql) is generated data
and any recipe edit means it may no longer match. The safe fix is a full
re-seed (`make oracle-seed && make oracle-snapshot`), but that re-fetches
live feeds and takes minutes -- overkill when the recipe change is cosmetic
(a comment, a tweak that doesn't alter the dump) or you've otherwise
confirmed the current dump is still the baseline you want.

`oracle-bless` takes the cheap path: it re-records the lineage hashes
(seed.py, feeds.txt, golden-db.sql) in db-manifest.json to match the files on
disk, WITHOUT re-running the seed or re-querying the live DB. It does not
touch the recorded row counts / feed list -- those still describe the
existing golden dump. Use it to acknowledge "the current golden dump is
canonical for the current recipe"; use `oracle-seed && oracle-snapshot` to
genuinely regenerate the dump.

Needs no docker stack / live DB -- it only hashes three local files.
"""

import hashlib
import json
import sys
from datetime import datetime, timezone
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
DATA = ROOT / "oracle" / "data"
MANIFEST = DATA / "db-manifest.json"

SOURCES = {
    "seed_sha256": ROOT / "oracle" / "scripts" / "seed.py",
    "feeds_sha256": ROOT / "oracle" / "feeds.txt",
    "dump_sha256": DATA / "golden-db.sql",
}


def sha256(p: Path) -> str:
    return hashlib.sha256(p.read_bytes()).hexdigest()


def main() -> int:
    if not MANIFEST.exists():
        print(
            "no golden manifest to bless: oracle/data/db-manifest.json is missing.\n"
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
    prev = m.get("lineage") or {}

    new_lineage: dict[str, str] = {}
    changed = []
    for key, path in SOURCES.items():
        if not path.exists():
            print(f"missing {key} source: {path.relative_to(ROOT)}", file=sys.stderr)
            return 1
        h = sha256(path)
        new_lineage[key] = h
        if prev.get(key) != h:
            old = prev.get(key)
            old_s = f"{old[:12]}\u2026" if old else "(unrecorded)"
            changed.append(f"{key}: {old_s} -> {h[:12]}\u2026")

    m["lineage"] = new_lineage
    m["blessed_at"] = datetime.now(timezone.utc).isoformat(timespec="seconds")
    MANIFEST.write_text(json.dumps(m, indent=2, ensure_ascii=False) + "\n")

    if not changed:
        print("golden manifest already matches the current files (nothing to bless).")
    else:
        print("re-baselined golden manifest lineage (no re-seed, live data untouched):")
        for c in changed:
            print(f"  - {c}")
    print(
        "note: recorded row counts / feed list are unchanged -- they still "
        "describe the existing golden dump."
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
