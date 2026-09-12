#!/usr/bin/env python3
"""diff_rpc_surface.py — find (op, method) the app can send but the backend
doesn't handle, without clicking every path.

The "app surface" is the frozen frontend (`tt-rss/js/*.js` call literals + the
`hidden_tag` form fields in `tt-rss/**/*.php`). The "backend surface" is parsed
live from the Rust dispatch (`crates/ttrss/src/rpc.rs`) so it stays in sync
with the port. A gap means the SPA would receive an unknown-method error for
that call.

Usage:
    python3 browser/diff_rpc_surface.py [--root DIR] [--verbose]

Exit 0 if no gaps, 1 if any are found.
"""

import argparse
import glob
import os
import re
import sys

# Frontend ops that redirect to public.php rather than being dispatched.
SHIM_OPS = {"rss", "share", "getprofiles"}


def _read(path):
    """Read a text file, exiting cleanly if it can't be opened."""
    try:
        with open(path, encoding="utf-8", errors="ignore") as f:
            return f.read()
    except OSError as e:
        sys.exit(f"cannot read {path}: {e}")


def parse_backend_arms(src_path):
    """Parse `match op { "<OP>" => match method { "<m>" => ... } }`.

    Returns (arms, special): arms maps op (as written) -> set of method
    strings; special are ops routed to a catch-all handler (e.g. "RPC" ->
    rpc_handler), whose methods are all considered handled.
    """
    text = _read(src_path)
    arms, special = {}, set()

    for m in re.finditer(r'"([A-Za-z0-9_]+)"\s*=>\s*match\s+method\s*\{', text):
        op = m.group(1)
        i, depth = m.end(), 1
        while i < len(text) and depth:
            if text[i] == "{":
                depth += 1
            elif text[i] == "}":
                depth -= 1
            i += 1
        block = text[m.end() : i - 1]
        # a method arm is a top-level string literal followed by `=>`
        arms[op] = set(re.findall(r'^\s*"([A-Za-z0-9_]+)"\s*=>', block, re.M))
        if re.search(r"rpc_handler\s*\(", block):
            special.add(op)

    return arms, special


def extract_app_surface(root):
    """Collect {(op, method_lower): set(source)} the frontend will send."""
    pairs = {}

    def add(op, meth, src):
        if not op or not meth or op.lower() in SHIM_OPS:
            return
        pairs.setdefault((op, meth.lower()), set()).add(src)

    # JS: `op: "X"` ... `method: "Y"` within a nearby window.
    op_re = re.compile(r"""op\s*[:=]\s*['"]([A-Za-z0-9_]+)['"]""")
    m_re = re.compile(r"""method\s*[:=]\s*['"]([A-Za-z0-9_]+)['"]""")
    for path in glob.glob(os.path.join(root, "tt-rss/js/*.js")):
        txt = _read(path)
        src = os.path.basename(path)
        toks = [(m.start(), "op", m.group(1)) for m in op_re.finditer(txt)]
        toks += [(m.start(), "m", m.group(1)) for m in m_re.finditer(txt)]
        toks.sort()
        for pos, kind, val in toks:
            if kind == "op":
                for pos2, kind2, val2 in toks:
                    if kind2 == "m" and 0 <= pos2 - pos < 400:
                        add(val, val2, src)
                        break

    # PHP forms: hidden_tag("op","X") near hidden_tag("method","Y").
    op_tag = re.compile(r"""hidden_tag\(\s*['"]op['"]\s*,\s*['"]([A-Za-z0-9_]+)['"]""")
    m_tag = re.compile(
        r"""hidden_tag\(\s*['"]method['"]\s*,\s*['"]([A-Za-z0-9_]+)['"]"""
    )
    for path in glob.glob(os.path.join(root, "tt-rss/**/*.php"), recursive=True):
        if "/tests" in path or "tests_" in path:
            continue
        txt = _read(path)
        src = os.path.basename(path)
        o_toks = [(m.start(), m.group(1)) for m in op_tag.finditer(txt)]
        m_toks = [(m.start(), m.group(1)) for m in m_tag.finditer(txt)]
        for po, vo in o_toks:
            for pm, vm in m_toks:
                if 0 <= pm - po < 400:
                    add(vo, vm, f"{src}(form)")
                    break

    return pairs


def main():
    ap = argparse.ArgumentParser(
        description="Diff the app's RPC surface vs the backend."
    )
    ap.add_argument("--root", default=".", help="repo root (default: cwd)")
    ap.add_argument("--verbose", action="store_true", help="list every (op,method)")
    args = ap.parse_args()
    root = os.path.abspath(args.root)

    src = os.path.join(root, "crates/ttrss/src/rpc.rs")
    if not os.path.exists(src):
        sys.exit(f"backend source not found: {src}")
    arms, special = parse_backend_arms(src)
    pairs = extract_app_surface(root)

    gaps = []
    for (op, meth), srcs in sorted(pairs.items()):
        if op in special:
            continue
        if op not in arms:
            gaps.append((op, meth, "op-not-dispatched", ", ".join(sorted(srcs))))
        elif meth not in arms[op]:
            gaps.append((op, meth, "method-missing", ", ".join(sorted(srcs))))

    if args.verbose:
        print(f"backend ops: {sorted(arms)}")
        print(f"special (catch-all): {sorted(special)}")
        print(f"app surface: {len(pairs)} distinct (op,method)\n")

    if gaps:
        print("GAPS — the app can send these, but the backend has no arm:\n")
        for op, meth, why, srcs in gaps:
            print(f"  op={op:12} method={meth:22} [{why:18}] {srcs}")
        print(f"\n{len(gaps)} gap(s).")
        return 1
    print("No gaps — every (op,method) the app sends is handled.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
