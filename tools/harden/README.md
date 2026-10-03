# harden

Repo-fitted coverage checkers used by the ship gauntlet.

## coverage-weighted cyclomatic complexity (`complexity_cov.py`)

Scores every function defined in the given Rust files by combining cyclomatic
complexity `C` with test coverage `cov`:

```
score = C ** (1 + (1 - cov))
```

A fully covered function scores `C`; an uncovered one scores `C ** 2`. Default
threshold 6; functions with score > threshold are reported (scores rounded to
1 decimal). Functions with 0 coverage and `C <= 2` score <= 4 and pass the
default threshold.

### Usage

```
python3 tools/harden/complexity_cov.py [--threshold N] [--refresh] \
    <file.rs> [<file.rs> ...]
```

- Positional args scope the check to functions defined in the given files.
- `--threshold N` changes the report threshold (default 6).
- `--refresh` forces a fresh coverage export even if the cache looks current.
- Output per offender (sorted by score, descending):
  `file:function score S (complexity C, coverage P%)`
- Exit codes: `0` none over threshold, `1` some over threshold, `2` usage or
  execution error.

### Coverage source and caching

Coverage comes from the llvm-cov JSON export (`data[0].functions` records) at
`/tmp/ship-tools-llvm.json`, produced by:

```
cargo llvm-cov --workspace --json --output-path /tmp/ship-tools-llvm.json
```

The export is cached: it is only regenerated when the file is missing, older
than the newest `.rs` file in the workspace, or `--refresh` is passed. A fresh
full-workspace run takes a few minutes, so prefer batching files into one
invocation.

A function's coverage records are matched geometrically (record filename plus
region start line inside the function's source span), so duplicate records per
compilation unit (lib + test binaries) are merged by summing counts. Functions
with total count 0 get coverage 0. Where a function has branch regions,
coverage is executed branches / total branches. Note: stable rustc does not
emit branch records (`-Z coverage-options=branch` is nightly-only), so on this
toolchain coverage is effectively per-function; the branch path activates
automatically if branch data appears in the export.

### Complexity counting

Starts at 1 and counts, per function body (after blanking strings, char
literals and comments, including nested block comments): `if` (an `else if`
counts once, as its `if`), `for`, `while`, `loop`, `break`, `?`, `&&`, `||`,
and one per top-level `match` arm.

Documented limits of the simple token scanner: an empty-argument closure
(`move || ...`) counts its `||`; `break` counts regardless of what it breaks
out of; a nested item `fn` inside a body is counted into the enclosing
function; a `match` scrutinee containing `{` (a struct literal) mis-locates
the arm list.

### Example

```
$ python3 tools/harden/complexity_cov.py crates/overlay/src/mode.rs
crates/overlay/src/mode.rs:menu_titles score 8.0 (complexity 8, coverage 100%)
$ echo $?
1
```

## Workspace-wide dead code (`dead_code.py`)

Reports top-level Rust symbols defined in the given files that nothing else
in the workspace references. Python 3 stdlib only, no build or coverage run
required: it resolves references with a word-boundary grep over the workspace
source tree, not with rustc's type graph, so it is deliberately conservative
and never relies on `cargo` being able to compile the crate.

### Usage

```
python3 tools/harden/dead_code.py <file.rs> [<file.rs> ...]
```

Positional args are the files to audit (typically the changed files). The
reference search covers every `.rs` file and `Cargo.toml` under the workspace
root - the nearest ancestor `Cargo.toml` containing a `[workspace]` table,
falling back to the nearest `Cargo.toml` - skipping `.git`, `target`,
`node_modules` and `__pycache__` directories.

### Output

- Violations - a top-level `fn` / `struct` / `enum` / `const` / `static`
  defined in a listed file with no reference anywhere else in the workspace:

  `file:line: dead: <kind> <name>`
- Warnings - a symbol whose only references, besides the declaration itself,
  are tests inside the defining file (an inline `#[cfg(test)] mod` or
  `#[test]` fn bodies): the production code is exercised only by its own
  tests, so it is likely vestigial but not provably unreachable:

  `file:line: warn: <kind> <name> only used by its own tests`

Lines print sorted by argument order, then line number.

### Exit codes

- `0` clean (no violations; warnings alone still exit 0)
- `1` at least one `dead:` violation
- `2` usage error (no args, unknown option, non-`.rs` arg, missing file, or
  no workspace root found)

### Matching rules and deliberate conservatism

- A reference is any word-boundary occurrence of the identifier in the **raw
  text** of another indexed file, so usages hidden inside macros or strings
  count: objc2 `define_class!` `#[unsafe(name = "Foo")]` targets, selector
  strings, serde attributes (`deserialize_with = "de_backend"`), and names
  listed in `Cargo.toml` (bin/lib/test targets) all keep a symbol alive.
- The same rule means doc comments and prose that happen to use the symbol's
  name also keep it alive. A collision with an ordinary English word is the
  accepted false-negative cost; the tool errs toward silence, so a reported
  line is trustworthy without per-line macro knowledge.
- Declarations, by contrast, are found on a masked copy of the source
  (comments and string/char literals blanked with length preserved), so a
  commented-out `// fn foo()` never registers as a definition.
- A symbol referenced from its **own file** outside the declaration line and
  outside test regions is alive with no warning: private top-level items are
  legitimately used only in their own file, and rustc's own
  `dead_code` lint already covers them.
- Items at brace depth > 0 are never reported: trait impls, inherent
  methods, `define_class!` contents and ObjC `#[unsafe(method(..))]` fns are
  reachable through the trait or a selector.
- Skipped outright: `fn main`, `#[test]` / `#[bench]` / rstest / test_case
  functions, items carrying `#[allow(dead_code)]` (multi-line attrs
  handled), `#[no_mangle]`, `#[proc_macro*]`, `#[global_allocator]`, and any
  name starting with `_`.
- Files under a `tests/` directory are test files: references from them keep
  symbols alive but never trigger the "own tests" warning (cross-file test
  coverage is real coverage), and their `#[test]` functions are not reported.
- `lib.rs` / `mod.rs` module declarations (`mod x;`) are not a reportable
  kind; the module name still counts as a reference for a same-named symbol.

### Example

Output from the tool's self-test fixture (both report kinds shown; lines sort
by file order then line number, warnings never affect the exit code, any
`dead:` line makes it 1):

```
$ python3 tools/harden/dead_code.py crates/a/src/lib.rs
crates/a/src/lib.rs:17: dead: fn truly_dead
crates/a/src/lib.rs:19: warn: fn only_own_tests only used by its own tests
crates/a/src/lib.rs:58: dead: const DEAD_CONST
crates/a/src/lib.rs:69: warn: fn only_own_tests_2 only used by its own tests
$ echo $?
1
```
