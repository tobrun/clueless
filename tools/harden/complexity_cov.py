#!/usr/bin/env python3
"""Coverage-weighted cyclomatic complexity checker for Rust files.

Score per function:  score = C ** (1 + (1 - cov))
  C    cyclomatic complexity (decision points + 1)
  cov  test coverage fraction in [0, 1]
A fully covered function scores C; an uncovered one scores C**2.
Functions scoring strictly above --threshold (default 6) are reported.

Usage:
    python3 tools/harden/complexity_cov.py [--threshold N] [--refresh] <file.rs> [...]

Coverage comes from `cargo llvm-cov --workspace --json --output-path
/tmp/ship-tools-llvm.json` (the llvm-cov export format, data[0].functions
records). The export is cached: it is only regenerated when the file is
missing, older than the newest .rs in the workspace, or --refresh is given.

Exit codes: 0 = no offenders, 1 = offenders reported, 2 = usage/execution error.

Known scanner limits (deliberately simple):
- Strings (incl. raw/byte strings), char literals and comments (incl. nested
  block comments) are blanked out before token counting, but exotic literals
  or macro-generated bodies can still confuse the token scan.
- `&&`/`||` are counted textually, so a `||` that starts an empty-argument
  closure (`move || ...`) counts as a decision point too.
- `break` is counted once per occurrence regardless of what it breaks out of;
  `else if` counts once (as the `if`).
- Items (e.g. a nested `fn`) defined inside a function body have their
  decision points counted into the enclosing function and their coverage
  records merged into it.
- match arms are counted by splitting the match body on top-level commas;
  a scrutinee containing a `{` (struct literal) mis-locates the arm list.
"""

import argparse
import json
import os
import re
import subprocess
import sys

DEFAULT_COVERAGE_PATH = "/tmp/ship-tools-llvm.json"


# --------------------------------------------------------------------------
# Source scanning
# --------------------------------------------------------------------------

def blank_source(src: str) -> str:
    """Return `src` with comments, strings, char literals and lifetimes
    replaced by spaces (newlines preserved, offsets unchanged)."""
    out = list(src)
    i, n = 0, len(src)

    def blank(a, b):
        for k in range(a, b):
            if out[k] != "\n":
                out[k] = " "

    while i < n:
        c = src[i]
        if c == "/" and src.startswith("//", i):
            j = src.find("\n", i)
            j = n if j < 0 else j
            blank(i, j)
            i = j
        elif c == "/" and src.startswith("/*", i):
            depth, j = 1, i + 2
            while j < n and depth:
                if src.startswith("/*", j):
                    depth += 1
                    j += 2
                elif src.startswith("*/", j):
                    depth -= 1
                    j += 2
                else:
                    j += 1
            blank(i, j)
            i = j
        elif (m := _RAW_STR_RE.match(src, i)):  # raw (byte) string: r".." br#".."#
            hashes, j = m.group(1), i + m.end()
            closer = '"' + hashes
            k = src.find(closer, j)
            j = n if k < 0 else k + len(closer)
            blank(i, j)
            i = j
        elif c == "b" and (src.startswith('b"', i) or src.startswith("b'", i)):
            j = _skip_quoted(src, i + 1, src[i + 1])
            blank(i, j)
            i = j
        elif c == '"':
            j = _skip_quoted(src, i, '"')
            blank(i, j)
            i = j
        elif c == "'":
            # char literal vs. lifetime: only skip if it closes.
            m = re.match(r"'(\\.|\\u\{[0-9a-fA-F]+\}|[^\\'\n])'", src[i:])
            if m:
                blank(i, i + m.end())
                i += m.end()
            else:
                i += 1  # lifetime, keep going
        else:
            i += 1
    return "".join(out)


def _skip_quoted(src: str, start: int, quote: str) -> int:
    """Index just past the quoted literal at `start` (handles \\ escapes)."""
    j = start + 1
    while j < len(src):
        if src[j] == "\\":
            j += 2
            continue
        if src[j] == quote or src[j] == "\n":
            return j + 1
        j += 1
    return j


FN_RE = re.compile(r"\bfn\s+([A-Za-z_][A-Za-z0-9_]*)")
_RAW_STR_RE = re.compile("b?r(#*)\"")


class RustFunction:
    def __init__(self, name, start_line, body_start, body_end, body_text):
        self.name = name
        self.start_line = start_line  # line of the `fn` keyword (1-based)
        self.body_start = body_start  # line of the body's opening brace
        self.body_end = body_end      # line of the body's closing brace
        self.body_text = body_text


def _line_of(text: str, pos: int) -> int:
    return text.count("\n", 0, pos) + 1


def _match_brace(text: str, open_pos: int) -> int:
    """Position of the `}` closing the `{` at open_pos, or len(text)."""
    depth = 0
    for k in range(open_pos, len(text)):
        if text[k] == "{":
            depth += 1
        elif text[k] == "}":
            depth -= 1
            if depth == 0:
                return k
    return len(text)


def find_functions(src: str):
    """Every `fn` with a body, in definition order."""
    blanked = blank_source(src)
    fns = []
    for m in FN_RE.finditer(blanked):
        body_open = blanked.find("{", m.end())
        if body_open < 0:
            continue
        body_close = _match_brace(blanked, body_open)
        # a declaration without a body (trait method) ends with ';' first
        semi = blanked.find(";", m.end())
        if semi != -1 and semi < body_open:
            continue
        fns.append(
            RustFunction(
                name=m.group(1),
                start_line=_line_of(blanked, m.start()),
                body_start=_line_of(blanked, body_open),
                body_end=_line_of(blanked, body_close),
                body_text=blanked[body_open + 1 : body_close],
            )
        )
    return fns


MATCH_RE = re.compile(r"\bmatch\b")


def count_match_arms(body: str) -> int:
    """One per top-level arm of every `match { ... }` in the body."""
    arms = 0
    for m in MATCH_RE.finditer(body):
        open_pos = body.find("{", m.end())
        if open_pos < 0:
            continue
        close_pos = _match_brace(body, open_pos)
        depth, seg, seg_nonempty = 0, 0, False
        for k in range(open_pos + 1, close_pos):
            ch = body[k]
            if ch in "([{":
                depth += 1
            elif ch in ")]}":
                depth -= 1
            elif ch == "," and depth == 0:
                if seg_nonempty:
                    arms += 1
                seg_nonempty = False
            elif not ch.isspace():
                seg_nonempty = True
        if seg_nonempty:  # last arm without trailing comma
            arms += 1
    return arms


def cyclomatic_complexity(body: str) -> int:
    c = 1
    c += len(re.findall(r"\bif\b", body))
    c += len(re.findall(r"\bfor\b", body))
    c += len(re.findall(r"\bwhile\b", body))
    c += len(re.findall(r"\bloop\b", body))
    c += len(re.findall(r"\bbreak\b", body))
    c += body.count("?")
    c += body.count("&&")
    c += body.count("||")
    c += count_match_arms(body)
    return c


# --------------------------------------------------------------------------
# Coverage
# --------------------------------------------------------------------------

def workspace_root(path: str) -> str:
    d = os.path.dirname(os.path.abspath(path))
    while True:
        if os.path.exists(os.path.join(d, "Cargo.lock")):
            return d
        parent = os.path.dirname(d)
        if parent == d:
            return os.path.dirname(os.path.abspath(path))
        d = parent


def newest_rs_mtime(root: str) -> float:
    newest = 0.0
    for dirpath, dirnames, filenames in os.walk(root):
        dirnames[:] = [d for d in dirnames if d not in ("target", ".git", "node_modules")]
        for f in filenames:
            if f.endswith(".rs"):
                mt = os.path.getmtime(os.path.join(dirpath, f))
                if mt > newest:
                    newest = mt
    return newest


def ensure_coverage(files, coverage_path: str, refresh: bool) -> bool:
    """Regenerate the llvm-cov json export unless the cache is fresh."""
    if not refresh and os.path.exists(coverage_path):
        export_mt = os.path.getmtime(coverage_path)
        newest = max(newest_rs_mtime(workspace_root(f)) for f in {workspace_root(f) for f in files})
        if export_mt >= newest:
            print(f"using cached coverage export {coverage_path}", file=sys.stderr)
            return True
    root = workspace_root(files[0])
    cmd = [
        "cargo", "llvm-cov", "--workspace", "--json",
        "--output-path", coverage_path,
    ]
    print(f"running: (cd {root} && {' '.join(cmd)})", file=sys.stderr)
    rc = subprocess.call(cmd, cwd=root)
    return rc == 0 and os.path.exists(coverage_path)


def _region_start_line(regions) -> int:
    return min(r[0] for r in regions) if regions else 0


def _region_end_line(regions) -> int:
    return max(r[2] for r in regions) if regions else 0


def load_function_records(coverage_path: str):
    """Flat list of function records from data[0].functions of the export."""
    with open(coverage_path) as fh:
        doc = json.load(fh)
    functions = doc["data"][0]["functions"]
    return list(functions.values()) if isinstance(functions, dict) else functions


def branch_is_executed(entry) -> bool:
    """True if an llvm-cov export branch record was executed.

    Export formats seen in the wild:
      v2: [line_start, col_start, line_end, col_end, file_id, block_id,
           line_end2, has_count, count, value]
      v3 (--branch): [line_start, line_end, col_start, col_end, file_id,
           expansion_file_id, has_count, count, value] or with fewer trailing
           fields; the count is the last field before `value`.
    Fall back to "last integer field" heuristics only as far as documented.
    """
    if not isinstance(entry, list):
        return False
    for idx in (8, 7, 6):
        if len(entry) > idx and isinstance(entry[idx], int):
            return entry[idx] > 0
    return False


def coverage_for(fn: RustFunction, path: str, records):
    """(cov, matched_record_count) for one source function.

    Coverage records are matched geometrically: a record belongs to `fn` when
    its primary filename is `path` and its region start line falls inside the
    function's span; the innermost span wins (closures and nested items
    inherit into their enclosing function). Duplicate records of the same
    function (lib + test compilation units) are merged by summing counts and
    pooling branches.
    """
    abspath = os.path.abspath(path)
    best = None  # (span_width, record)
    spans = []
    for rec in records:
        if not any(os.path.abspath(f) == abspath for f in rec.get("filenames", [])):
            continue
        regions = rec.get("regions", [])
        if not regions:
            continue
        rs, re_ = _region_start_line(regions), _region_end_line(regions)
        if fn.start_line <= rs <= fn.body_end:
            spans.append((re_ - rs, rec))
    total_count = 0
    branches = {}  # dedupe by position across compilation units
    for _, rec in spans:
        total_count += rec.get("count", 0)
        for b in rec.get("branches", []) or []:
            if isinstance(b, list) and len(b) >= 4:
                branches.setdefault((b[0], b[1], b[2], b[3]), b)
    if total_count == 0:
        return 0.0, len(spans)
    if branches:
        executed = sum(1 for b in branches.values() if branch_is_executed(b))
        return executed / len(branches), len(spans)
    return 1.0, len(spans)


# --------------------------------------------------------------------------
# Main
# --------------------------------------------------------------------------

def main(argv=None) -> int:
    ap = argparse.ArgumentParser(
        prog="complexity_cov.py",
        description="Report Rust functions whose coverage-weighted "
        "cyclomatic complexity C**(2-cov) exceeds a threshold.",
    )
    ap.add_argument("--threshold", type=float, default=6.0,
                    help="report functions with score > threshold (default 6)")
    ap.add_argument("--refresh", action="store_true",
                    help="force a fresh cargo llvm-cov export run")
    ap.add_argument("--coverage-path", default=DEFAULT_COVERAGE_PATH,
                    help=f"path of the llvm-cov json export "
                    f"(default {DEFAULT_COVERAGE_PATH})")
    ap.add_argument("files", nargs="+", metavar="<file.rs>",
                    help="Rust source files to check")
    args = ap.parse_args(argv)

    for f in args.files:
        if not os.path.isfile(f):
            print(f"error: no such file: {f}", file=sys.stderr)
            return 2

    if not ensure_coverage(args.files, args.coverage_path, args.refresh):
        print("error: cargo llvm-cov export failed", file=sys.stderr)
        return 2

    try:
        records = load_function_records(args.coverage_path)
    except (OSError, KeyError, ValueError) as e:
        print(f"error: cannot read coverage export {args.coverage_path}: {e}",
              file=sys.stderr)
        return 2

    offenders = []
    for path in args.files:
        with open(path, encoding="utf-8") as fh:
            src = fh.read()
        for fn in find_functions(src):
            c = cyclomatic_complexity(fn.body_text)
            cov, _ = coverage_for(fn, path, records)
            score = c ** (1 + (1 - cov))
            if score > args.threshold:
                offenders.append(
                    (score, path, fn.name, c, cov)
                )

    offenders.sort(key=lambda o: (-o[0], o[1], o[2]))
    for score, path, name, c, cov in offenders:
        print(f"{path}:{name} score {round(score, 1)} "
              f"(complexity {c}, coverage {round(cov * 100)}%)")
    return 1 if offenders else 0


if __name__ == "__main__":
    sys.exit(main())
