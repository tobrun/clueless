#!/usr/bin/env python3
"""dead_code.py - workspace-wide dead symbol detector for Rust files.

Invocation:

    python3 tools/harden/dead_code.py <file.rs> [<file.rs> ...]

Reports two things, one line each, for the given Rust files:

  1. violations  `file:line: dead: <kind> <name>`
     A top-level `fn`, `struct`, `enum`, `const` or `static` defined in a
     listed file that nothing in the workspace references. The reference
     search is a word-boundary grep of the identifier over the raw text of
     every `.rs` file and `Cargo.toml` under the workspace root (excluding
     the defining file itself), so string-literal usage produced by macros
     (`define_class!` `#[name = "..."]`, selector strings, serde attribute
     names like `deserialize_with = "de_backend"`) counts as a reference.
  2. warnings    `file:line: warn: <kind> <name> only used by its own tests`
     A symbol whose only workspace references are tests inside the defining
     file itself (an inline `#[cfg(test)] mod tests` or `#[test]` functions)
     and none elsewhere: the code is only exercised by its own tests.
     Warnings never affect the exit code.

A symbol also stays alive when the defining file itself references it
outside the declaration line and outside test regions (private top-level
items are legitimately used only in their own file; rustc already lint
checks them). Files under a `tests/` directory are test files: references
from their test functions keep symbols alive without a warning, and `#[test]`
functions are never reported.

Handled deliberately:
  - trait impls / inherent methods / ObjC methods live inside `impl` or
    `define_class!` bodies (brace depth > 0) and are not treated as
    top-level symbols: they are reachable via the trait or a selector.
  - `fn main`, `#[test]`/`#[tokio::test]` items, and items carrying
    `#[allow(dead_code)]`, `#[no_mangle]`, `#[proc_macro*]` or
    `#[global_allocator]` are skipped.
  - `lib.rs` module declarations (`mod x;` / `pub mod x;`) are not symbol
    kinds; the module name still counts as a reference for same-named
    symbols.
  - strings and comments are blanked before finding declarations, so a
    commented-out `// pub fn foo()` never registers; references however are
    matched on raw text (conservative: mentions in docs/comments elsewhere
    keep a symbol alive).

Exit codes: 0 clean, 1 at least one `dead:` violation, 2 usage error.
"""

import os
import re
import sys

USAGE = "usage: dead_code.py <file.rs> [<file.rs> ...]"

SKIP_DIRS = {".git", "target", "node_modules", "__pycache__", ".venv"}

VIS_RE = re.compile(r"^pub(?:\s*\([^()]*\))?\s+")
QUAL_RE = re.compile(r"^(?:unsafe|async|const|auto|default|extern|\"C\"|C)\s+")
KIND_RE = re.compile(
    r"^(?P<kind>fn|const|static(?:\s+mut)?|struct|enum)\s+(?P<name>[A-Za-z_]\w*)\b"
)
MOD_RE = re.compile(r"^(?:pub(?:\s*\([^()]*\))?\s+)?mod\s+([A-Za-z_]\w*)")
ATTR_TEST_RE = re.compile(r"^#\[\s*(?:(?:\w+)::)*(?:test|bench)\s*\]")
ATTR_TEST_GEN_RE = re.compile(r"^#\[\s*(?:(?:\w+)::)*(?:rstest|test_case)\b")
ATTR_ALLOW_DEAD_RE = re.compile(r"allow\s*\([^)]*\bdead_code\b")
ATTR_SKIP_RE = re.compile(r"^#\[\s*(?:no_mangle|proc_macro\w*|global_allocator)\b")
ATTR_CFG_TEST_RE = re.compile(r"cfg\s*\(\s*test\s*\)")

CHAR_LIT_RE = re.compile(r"'(?:\\.|[^\\'])'")
RAW_STR_RE = re.compile(r"[bC]?r(#*)\"")
WS_ONLY = re.compile(r"^\s*$")


def mask_code(text):
    """Return text with comments and string/char literals blanked out
    (length preserved, newlines kept) so structure can be scanned safely."""
    out = []
    i, n = 0, len(text)

    def blank(seg):
        out.append("".join("\n" if c == "\n" else " " for c in seg))

    while i < n:
        c = text[i]
        if c == "/" and text.startswith("//", i):
            j = text.find("\n", i)
            j = n if j < 0 else j
            blank(text[i:j])
            i = j
        elif c == "/" and text.startswith("/*", i):
            j, depth = i + 2, 1
            while j < n and depth:
                if text.startswith("/*", j):
                    depth += 1
                    j += 2
                elif text.startswith("*/", j):
                    depth -= 1
                    j += 2
                else:
                    j += 1
            blank(text[i:j])
            i = j
        elif c == '"':
            j = i + 1
            while j < n:
                if text[j] == "\\":
                    j += 2
                elif text[j] == '"':
                    j += 1
                    break
                else:
                    j += 1
            blank(text[i:j])
            i = j
        elif c == "'":
            m = CHAR_LIT_RE.match(text, i)
            if m:
                blank(m.group(0))
                i = m.end()
            else:  # lifetime
                out.append(c)
                i += 1
        elif (
            c in "rbC"
            and (i == 0 or not (text[i - 1].isalnum() or text[i - 1] == "_"))
            and RAW_STR_RE.match(text, i)
        ):
            m = RAW_STR_RE.match(text, i)
            close = "\"" + m.group(1)
            j = text.find(close, m.end())
            j = n if j < 0 else j + len(close)
            blank(text[i:j])
            i = j
        else:
            out.append(c)
            i += 1
    return "".join(out)


def parse_decl(stripped):
    """Match a top-level item declaration line; return (kind, name) or None."""
    rest = stripped
    m = VIS_RE.match(rest)
    if m:
        rest = rest[m.end():]
    for _ in range(4):
        m = KIND_RE.match(rest)
        if m:
            kind = m.group("kind")
            return kind, m.group("name")
        m = QUAL_RE.match(rest)
        if not m:
            return None
        rest = rest[m.end():]
    return None


def parse_file(path):
    """Scan one Rust file. Returns (decls, test_lines, raw_lines).

    decls: list of dicts {line (1-based), kind, name, test (bool)} for
    top-level fn/struct/enum/const/static items.
    test_lines: set of 0-based line indices that belong to test code
    (inline #[cfg(test)] mods, bodies of #[test] fns).
    """
    with open(path, "r", encoding="utf-8", errors="replace") as fh:
        text = fh.read()
    raw_lines = text.split("\n")
    masked_lines = mask_code(text).split("\n")

    decls = []
    test_lines = set()
    depth = 0
    pending_attrs = []
    attr_buf = []
    attr_open = False  # inside a multi-line attribute
    attr_paren = 0
    test_mod_base = None   # brace depth enclosing an open #[cfg(test)] mod
    skip_body_base = None  # brace depth enclosing a skipped #[test] fn

    for idx, mline in enumerate(masked_lines):
        code = mline.strip()
        if test_mod_base is not None or skip_body_base is not None:
            test_lines.add(idx)

        if WS_ONLY.match(code):
            continue

        if attr_open:
            attr_buf.append(code)
            attr_paren += code.count("(") + code.count("[")
            attr_paren -= code.count(")") + code.count("]")
            if attr_paren <= 0:
                pending_attrs.append(" ".join(attr_buf))
                attr_buf, attr_open, attr_paren = [], False, 0
            continue

        if code.startswith("#"):
            opens = code.count("(") + code.count("[")
            closes = code.count(")") + code.count("]")
            if opens > closes:
                attr_buf, attr_open, attr_paren = [code], True, opens - closes
            else:
                pending_attrs.append(code)
            continue

        # A code line: attributes above it belong to it.
        attrs = pending_attrs
        pending_attrs = []

        depth_before = depth
        is_mod = bool(MOD_RE.match(code))
        decl = parse_decl(code) if depth == 0 and not is_mod else None
        kind = name = None
        if decl:
            kind, name = decl

        attr_text = attrs
        is_test_item = any(
            ATTR_TEST_RE.match(a) or ATTR_TEST_GEN_RE.match(a) for a in attrs
        )

        if decl:
            skipped = (
                (kind == "fn" and name == "main")
                or is_test_item
                or any(ATTR_ALLOW_DEAD_RE.search(a) or ATTR_SKIP_RE.match(a) for a in attrs)
                or name.startswith("_")
            )
            if not skipped:
                decls.append({"line": idx + 1, "kind": kind.split()[0], "name": name})
            elif is_test_item and kind == "fn":
                skip_body_base = depth_before

        # #[cfg(test)] mod ... opens a test region.
        if (
            depth == 0
            and is_mod
            and any(ATTR_CFG_TEST_RE.search(a) for a in attrs)
            and "{" in mline
        ):
            test_mod_base = depth_before
            test_lines.add(idx)

        # Apply brace delta of the masked line.
        depth += mline.count("{") - mline.count("}")
        if depth < 0:
            depth = 0

        if test_mod_base is not None and depth <= test_mod_base:
            test_mod_base = None
        if skip_body_base is not None and depth <= skip_body_base:
            skip_body_base = None

    return decls, test_lines, raw_lines


def find_workspace_root(paths):
    """Nearest ancestor of any input file whose Cargo.toml contains
    [workspace]; else the nearest Cargo.toml; else the file's directory."""
    nearest = None
    for p in paths:
        d = os.path.dirname(os.path.realpath(p))
        cur = d
        while True:
            cargo = os.path.join(cur, "Cargo.toml")
            if os.path.isfile(cargo):
                try:
                    with open(cargo, "r", encoding="utf-8", errors="replace") as fh:
                        head = fh.read(8192)
                    if re.search(r"(?m)^\s*\[workspace\]\s*$", head):
                        return cur
                except OSError:
                    pass
                if nearest is None or len(cur) > len(nearest):
                    nearest = cur
            parent = os.path.dirname(cur)
            if parent == cur:
                break
            cur = parent
    return nearest


def build_index(root):
    """All .rs files and Cargo.toml files under root (skipping target/.git)."""
    files = []
    for dirpath, dirnames, filenames in os.walk(root):
        dirnames[:] = sorted(d for d in dirnames if d not in SKIP_DIRS)
        for fn in sorted(filenames):
            if fn.endswith(".rs") or fn == "Cargo.toml":
                files.append(os.path.realpath(os.path.join(dirpath, fn)))
    return files


def read_lines(path):
    try:
        with open(path, "r", encoding="utf-8", errors="replace") as fh:
            return fh.read().split("\n")
    except OSError:
        return []


def main(argv):
    args = argv[1:]
    if any(a in ("-h", "--help") for a in args):
        print(USAGE)
        return 0
    if not args:
        print(USAGE, file=sys.stderr)
        return 2
    if any(a.startswith("-") for a in args):
        print(f"dead_code: unknown option: {args[0]}", file=sys.stderr)
        print(USAGE, file=sys.stderr)
        return 2
    for a in args:
        if not a.endswith(".rs"):
            print(f"dead_code: not a Rust file: {a}", file=sys.stderr)
            return 2
        if not os.path.isfile(a):
            print(f"dead_code: no such file: {a}", file=sys.stderr)
            return 2

    root = find_workspace_root(args)
    if root is None:
        print("dead_code: could not locate the workspace root", file=sys.stderr)
        return 2

    index = build_index(root)
    line_cache = {p: read_lines(p) for p in index}

    reports = []  # (file order, line, is_violation, text)
    for order, arg in enumerate(args):
        rpath = os.path.realpath(arg)
        decls, test_lines, raw_lines = parse_file(arg)
        is_test_file = os.sep + "tests" + os.sep in rpath or os.path.basename(
            os.path.dirname(rpath)
        ) == "tests"
        for d in decls:
            name_re = re.compile(r"\b" + re.escape(d["name"]) + r"\b")
            outside = False
            for p in index:
                if p == rpath:
                    continue
                if any(name_re.search(ln) for ln in line_cache[p]):
                    outside = True
                    break
            if outside:
                continue
            own_test_ref = False
            own_prod_ref = False
            for i, ln in enumerate(raw_lines):
                if i == d["line"] - 1:  # the declaration line itself
                    continue
                if name_re.search(ln):
                    if i in test_lines and not is_test_file:
                        own_test_ref = True
                    else:
                        own_prod_ref = True
            if own_prod_ref:
                continue
            if own_test_ref:
                reports.append(
                    (order, d["line"], False,
                     f"{arg}:{d['line']}: warn: {d['kind']} {d['name']} "
                     f"only used by its own tests")
                )
            else:
                reports.append(
                    (order, d["line"], True,
                     f"{arg}:{d['line']}: dead: {d['kind']} {d['name']}")
                )

    reports.sort(key=lambda r: (r[0], r[1]))
    violations = 0
    for _, _, is_violation, text in reports:
        print(text)
        if is_violation:
            violations += 1
    return 1 if violations else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
