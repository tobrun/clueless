#!/usr/bin/env python3
"""dep_check.py - repo-fitted dependency checker for the clueless workspace.

Reads the module rules from docs/dependencies.md (the fenced ```rules block)
and checks the real Cargo workspace against them:

1. Internal edges: for every member crate's [dependencies], any dependency
   that is itself a workspace crate must be covered by an `[allowed]` edge
   for the crate's module. Module membership comes from the `[modules]`
   globs, matched against each member's directory.
2. Purity rule (prose at the top of docs/dependencies.md): the six pure
   crates (types, segmenter, asr, llm, context, engine) must not depend on
   any macOS-only external crate.

Usage:
    python3 tools/harden/dep_check.py [docs/dependencies.md]

The rules file path defaults to docs/dependencies.md, resolved from the
workspace root. The workspace root is found by walking up from the current
directory to the nearest Cargo.toml containing a [workspace] table, so a
rules-file variant (e.g. in /tmp) can be checked against the repo the
command was run from.

Output: one line per violation, in the form
    <file> -> <file> (<module> -> <module>)
For purity violations the right-hand side is the offending external crate.

Exit codes: 0 = clean, 1 = violations found, 2 = usage or parse error.

Requires Python 3.11+ (stdlib tomllib). Rules-block parsing is plain string
handling; the block is not valid TOML (`x = crates/x/**` is unquoted glob
text).
"""

import fnmatch
import os
import sys

try:
    import tomllib
except ImportError:  # pragma: no cover
    sys.stderr.write("dep_check: python3.11+ required (stdlib tomllib missing)\n")
    sys.exit(2)

# Prose rule from the top of docs/dependencies.md: these module crates are
# pure and must not pull in macOS-only external crates.
PURE_MODULES = ["types", "segmenter", "asr", "llm", "context", "trace", "engine"]
MACOS_ONLY_CRATES = {
    "objc2",
    "objc2-app-kit",
    "objc2-foundation",
    "block2",
    "dispatch2",
    "cpal",
    "screencapturekit",
    "global-hotkey",
    "rtrb",
}


def die(msg):
    sys.stderr.write("dep_check: %s\n" % msg)
    sys.exit(2)


def find_workspace_root():
    d = os.getcwd()
    while True:
        cargo = os.path.join(d, "Cargo.toml")
        if os.path.isfile(cargo):
            try:
                with open(cargo, "rb") as f:
                    data = tomllib.load(f)
            except (OSError, tomllib.TOMLDecodeError):
                data = {}
            if "workspace" in data:
                return d
        parent = os.path.dirname(d)
        if parent == d:
            die("no Cargo.toml with a [workspace] table found from %s upward" % os.getcwd())
        d = parent


def parse_rules(path):
    """Parse the fenced ```rules block. Returns (modules, allowed).

    modules: {name: glob}
    allowed: {source_module: set(target_module)}
    """
    try:
        with open(path, encoding="utf-8") as f:
            lines = f.read().splitlines()
    except OSError as e:
        die("cannot read rules file %s: %s" % (path, e))

    # Locate the fenced rules block with plain string handling.
    start = end = None
    for i, line in enumerate(lines):
        stripped = line.strip()
        if start is None:
            if stripped == "```rules":
                start = i + 1
        else:
            if stripped == "```":
                end = i
                break
    if start is None:
        die("no fenced ```rules block found in %s" % path)
    if end is None:
        die("unterminated ```rules block in %s" % path)

    modules = {}
    allowed = {}
    section = None
    for lineno in range(start, end):
        raw = lines[lineno]
        line = raw.strip()
        if not line or line.startswith("#"):
            continue
        if line.startswith("[") and line.endswith("]"):
            section = line[1:-1]
            if section not in ("modules", "allowed"):
                die("%s:%d: unknown rules section [%s]" % (path, lineno + 1, section))
            continue
        if section == "modules":
            if "=" not in line:
                die("%s:%d: expected `name = glob` in [modules]: %r" % (path, lineno + 1, line))
            name, glob = (part.strip() for part in line.split("=", 1))
            if not name or not glob:
                die("%s:%d: empty module name or glob: %r" % (path, lineno + 1, line))
            modules[name] = glob
        elif section == "allowed":
            if "->" not in line:
                die("%s:%d: expected `module -> mod1, mod2` in [allowed]: %r" % (path, lineno + 1, line))
            src, targets = line.split("->", 1)
            src = src.strip()
            if src not in modules:
                die("%s:%d: allowed edge from unknown module %r" % (path, lineno + 1, src))
            dests = allowed.setdefault(src, set())
            for t in targets.split(","):
                t = t.strip()
                if not t:
                    continue
                if t not in modules:
                    die("%s:%d: allowed edge to unknown module %r" % (path, lineno + 1, t))
                dests.add(t)
        else:
            die("%s:%d: rule line outside any section: %r" % (path, lineno + 1, line))
    if not modules:
        die("rules block in %s declares no [modules]" % path)
    return modules, allowed


def expand_members(root):
    """Resolve workspace member directories (supports glob patterns)."""
    with open(os.path.join(root, "Cargo.toml"), "rb") as f:
        data = tomllib.load(f)
    members = data.get("workspace", {}).get("members", [])
    if not isinstance(members, list) or not members:
        die("no workspace members found in %s" % os.path.join(root, "Cargo.toml"))
    result = []
    for m in members:
        if any(ch in m for ch in "*?["):
            # glob pattern: expand to directories containing a Cargo.toml
            base = m.split("*")[0].rsplit("/", 1)[0] if "/" in m else "."
            base_dir = os.path.join(root, base)
            for dirpath, _dirnames, filenames in os.walk(base_dir):
                if "Cargo.toml" in filenames:
                    result.append(os.path.relpath(dirpath, root))
        else:
            result.append(m)
    return sorted(set(result))


def glob_matches(glob, rel_dir):
    """Match a module glob like `crates/types/**` against a member dir."""
    candidates = [rel_dir, rel_dir + "/.", rel_dir + "/x"]
    return any(fnmatch.fnmatch(c, glob) for c in candidates)


def main(argv):
    if len(argv) > 2:
        sys.stderr.write("usage: dep_check.py [docs/dependencies.md]\n")
        return 2
    root = find_workspace_root()
    rules_path = argv[1] if len(argv) > 1 else os.path.join(root, "docs", "dependencies.md")

    try:
        modules, allowed = parse_rules(rules_path)
    except tomllib.TOMLDecodeError as e:  # defensive; rules are plain text
        die("cannot parse %s: %s" % (rules_path, e))

    # crate name -> (member dir, Cargo.toml path, parsed manifest)
    crates = {}
    for member in expand_members(root):
        manifest = os.path.join(member, "Cargo.toml")
        manifest_path = os.path.join(root, manifest)
        try:
            with open(manifest_path, "rb") as f:
                data = tomllib.load(f)
        except OSError as e:
            die("cannot read %s: %s" % (manifest_path, e))
        except tomllib.TOMLDecodeError as e:
            die("cannot parse %s: %s" % (manifest_path, e))
        name = data.get("package", {}).get("name")
        if not name:
            die("%s has no package.name" % manifest)
        crates[name] = (member, manifest, data)

    # member dir -> module name via the [modules] globs
    def module_of(rel_dir):
        for mod, glob in modules.items():
            if glob_matches(glob, rel_dir):
                return mod
        return None

    def collect_dependency_names(manifest_data):
        """Top-level and target-specific dependency tables (not dev/build deps)."""
        names = set()
        deps = manifest_data.get("dependencies")
        if isinstance(deps, dict):
            names.update(deps.keys())
        target = manifest_data.get("target")
        if isinstance(target, dict):
            for cfg in target.values():
                if isinstance(cfg, dict):
                    for key in ("dependencies",):
                        td = cfg.get(key)
                        if isinstance(td, dict):
                            names.update(td.keys())
        return names

    violations = []

    # Rule 1: internal workspace edges must be allowed.
    for name, (member, manifest, data) in sorted(crates.items()):
        src_mod = module_of(member)
        if src_mod is None:
            sys.stderr.write("dep_check: warning: crate %r (%s) matches no [modules] glob; "
                             "its edges are not checked\n" % (name, manifest))
            continue
        for dep_name in sorted(collect_dependency_names(data)):
            if dep_name == name or dep_name not in crates:
                continue  # external crate
            dst_dir, dst_manifest, _ = crates[dep_name]
            dst_mod = module_of(dst_dir)
            if dst_mod is None:
                sys.stderr.write("dep_check: warning: dependency %r (%s) matches no "
                                 "[modules] glob\n" % (dep_name, dst_manifest))
                continue
            if dst_mod not in allowed.get(src_mod, set()):
                violations.append("%s -> %s (%s -> %s)"
                                  % (manifest, dst_manifest, src_mod, dst_mod))

    # Rule 2: pure crates must not depend on macOS-only external crates.
    for name, (member, manifest, data) in sorted(crates.items()):
        src_mod = module_of(member)
        if src_mod not in PURE_MODULES:
            continue
        for dep_name in sorted(collect_dependency_names(data)):
            if dep_name in MACOS_ONLY_CRATES:
                violations.append("%s -> %s (%s -> macos-only)"
                                  % (manifest, dep_name, src_mod))

    for v in violations:
        print(v)
    if violations:
        print("dep_check: %d violation(s)" % len(violations), file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
