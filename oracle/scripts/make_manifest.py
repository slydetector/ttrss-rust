#!/usr/bin/env python3
"""Write oracle/data/db-manifest.json: row counts + key facts of the golden DB.

Also records the seed lineage (SHA-256 of seed.py, feeds.txt, and the
golden dump itself) so `check_sync.py` can detect a stale golden dataset —
the dump is generated data (gitignored) and silently goes stale whenever
the seed recipe changes without a re-seed + re-snapshot.
"""

import hashlib
import json
import subprocess
import sys
from datetime import datetime, timezone
from pathlib import Path


def main() -> int:
    data_dir = Path(sys.argv[1])
    base = sys.argv[2]
    data_dir.mkdir(parents=True, exist_ok=True)

    root = data_dir.parents[1]   # repo root (data_dir == <root>/oracle/data)

    def sha256(p: Path) -> str:
        return hashlib.sha256(p.read_bytes()).hexdigest()
    def rows(query: str) -> list[list[str]]:
        p = subprocess.run(
            ["docker", "compose", "exec", "-T", "db", "psql", "-U", "postgres",
             "-d", "postgres", "-t", "-A", "-F", "\t", "-c", query],
            capture_output=True, text=True, cwd=data_dir.parents[0])
        if p.returncode != 0:
            raise RuntimeError(f"SQL failed: {query}\n{p.stderr}")
        return [line.split("\t") for line in p.stdout.splitlines() if line.strip()]

    counts: dict[str, int] = {}
    tables = [r[0] for r in rows(
        "select tablename from pg_tables where schemaname = 'public' order by 1")]
    for t in tables:
        counts[t] = int(rows(f"select count(*) from {t}")[0][0])

    feeds = rows("""
        select feed_url, title,
               last_updated is null, last_error <> '',
               (select count(*) from ttrss_user_entries ue
                where ue.feed_id = f.id) as articles
        from ttrss_feeds f order by f.id""")

    manifest = {
        "generated": datetime.now(timezone.utc).isoformat(timespec="seconds"),
        "base_url": base,
        "source": "oracle docker stack (tt-rss @ ghcr.io/tt-rss/tt-rss:sha-2f7d831)",
        "table_counts": counts,
        "feeds": [
            {"url": f[0], "title": f[1], "not_updated": f[2] == "t",
             "has_error": f[3] == "t", "articles": int(f[4])}
            for f in feeds
        ],
        "totals": {
            "feeds": counts["ttrss_feeds"],
            "feeds_with_errors": sum(1 for f in feeds if f[3] == "t"),
            "articles": counts["ttrss_entries"],
            "per_user_entries": counts["ttrss_user_entries"],
            "enclosures": counts["ttrss_enclosures"],
            "categories": counts["ttrss_feed_categories"],
            "labels": counts["ttrss_labels2"],
            "filters": counts["ttrss_filters2"],
            "users": counts["ttrss_users"],
            "error_log_entries": counts["ttrss_error_log"],
        },
        "users": [
            {"login": r[0], "uid": int(r[1]), "access_level": int(r[2])}
            for r in rows(
                "select login, id, access_level from ttrss_users order by id")
        ],
        "lineage": {
            # What produced this dump; verified by check_sync.py before any
            # reset/restore/harness/record run uses the golden dataset.
            "seed_sha256": sha256(root / "oracle/scripts/seed.py"),
            "feeds_sha256": sha256(root / "oracle/feeds.txt"),
            "dump_sha256": sha256(data_dir / "golden-db.sql"),
        },
    }
    out = data_dir / "db-manifest.json"
    out.write_text(json.dumps(manifest, indent=2, ensure_ascii=False) + "\n")
    print(f"wrote {out}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
