#!/usr/bin/env python3
"""Approximate file-level fan-in (how many other files import this file) and its
relationship to churn, across the local repo corpus.

Deliberately approximate, and the approximations are listed so the numbers can be
read honestly:

  * Java  — indexed by the path after the last `java/` segment; wildcard and
            same-package uses count, explicit `import a.b.C` resolves. Same-package
            references *without* an import are INVISIBLE (undercount).
  * Python— absolute module names are indexed relative to the repo root and to
            common source roots; relative imports (`from . import x`) resolve
            against the importing package. Dynamic imports (`importlib`) are
            INVISIBLE.
  * TS/JS — relative specifiers only; bare specifiers (node_modules) and tsconfig
            path aliases (`@/...`) are SKIPPED (undercount).
  * Go    — import path matched as a suffix against in-repo package directories.
  * Rust  — `crate::a::b` / `super::` / `self::` resolved to `a/b.rs` or
            `a/b/mod.rs`; macro-generated modules are INVISIBLE.

So every number here is a lower bound, and cross-language comparisons are not
meaningful. Within-language rankings are the useful part.
"""

import collections
import json
import math
import os
import re
import statistics
import subprocess
import sys
from pathlib import Path

SKIP_DIRS = {
    ".git", ".jj", "target", "node_modules", "vendor", "third_party", "third-party",
    "dist", "build", ".venv", "venv", "site-packages", "__pycache__", ".direnv",
    ".devenv", ".next", ".turbo", "open_source", ".mypy_cache", ".pytest_cache",
}
EXT = {
    ".rs": "rust", ".ts": "typescript", ".tsx": "typescript", ".mts": "typescript",
    ".js": "javascript", ".jsx": "javascript", ".mjs": "javascript", ".cjs": "javascript",
    ".py": "python", ".pyi": "python", ".go": "go", ".java": "java",
}
MAX_BYTES = 1_000_000
SOURCE_ROOTS = ("src", "lib", "python", "tests", "test")


def source_files(repo: Path):
    for root, dirs, files in os.walk(repo):
        dirs[:] = [d for d in dirs if d not in SKIP_DIRS and not d.startswith(".")]
        for name in files:
            ext = os.path.splitext(name)[1].lower()
            if ext in EXT:
                path = Path(root) / name
                try:
                    if path.stat().st_size <= MAX_BYTES:
                        yield path
                except OSError:
                    continue


def read(path: Path) -> str:
    try:
        return path.read_text(errors="ignore")
    except OSError:
        return ""


def strip_comment_lines(text: str) -> str:
    out = []
    for line in text.splitlines():
        stripped = line.lstrip()
        if stripped.startswith(("//", "#", "*", "/*")):
            continue
        out.append(line)
    return "\n".join(out)


# ---------------------------------------------------------------- java

JAVA_IMPORT = re.compile(r"^\s*import\s+(?:static\s+)?([\w.]+)(\.\*)?\s*;", re.M)


def java_index(files):
    index = {}
    for path in files:
        parts = path.parts
        for i in range(len(parts) - 1, -1, -1):
            if parts[i] == "java":
                fqcn = ".".join(parts[i + 1:]).removesuffix(".java")
                index[fqcn] = path
                break
    return index


def java_edges(path, text, index):
    package = None
    parts = path.parts
    for i in range(len(parts) - 1, -1, -1):
        if parts[i] == "java":
            package = ".".join(parts[i + 1:-1])
            break
    targets = []
    for base, star in JAVA_IMPORT.findall(text):
        if star:
            prefix = base + "."
            targets.extend(t for k, t in index.items() if k.startswith(prefix))
        else:
            if base in index:
                targets.append(index[base])
            elif package and package + "." + base in index:
                targets.append(index[package + "." + base])
    return targets


# ---------------------------------------------------------------- python

PY_FROM = re.compile(r"^\s*from\s+([\w.]+|\.+[\w.]*)\s+import\s+(.+)$", re.M)
PY_IMPORT = re.compile(r"^\s*import\s+([\w.]+(?:\s*,\s*[\w.]+)*)\s*$", re.M)


def python_index(files, repo):
    index = collections.defaultdict(list)
    for path in files:
        rel = path.relative_to(repo).with_suffix("")
        parts = list(rel.parts)
        if parts and parts[-1] == "__init__":
            parts = parts[:-1]
        if not parts:
            continue
        index[".".join(parts)].append(path)
        # also register under common source roots, so `import foo.bar` finds src/foo/bar.py
        if parts[0] in SOURCE_ROOTS and len(parts) > 1:
            index[".".join(parts[1:])].append(path)
    return index


def python_edges(path, text, index, repo):
    rel = path.relative_to(repo).with_suffix("")
    package = list(rel.parts[:-1])
    if package and package[-1] == "__init__":
        package = package[:-1]
    targets = []

    def add(name):
        for candidate in index.get(name, []):
            targets.append(candidate)

    for module, names in PY_FROM.findall(text):
        if module.startswith("."):
            dots = len(module) - len(module.lstrip("."))
            rest = module.lstrip(".")
            base = package[: len(package) - (dots - 1)] if dots > 1 else package
            name = ".".join(base + ([rest] if rest else []))
            add(name)
            for item in names.replace("(", " ").replace(")", " ").split(","):
                item = item.strip().split(" as ")[0].strip()
                if item and item != "*":
                    add(name + "." + item)
        else:
            add(module)
            for item in names.replace("(", " ").replace(")", " ").split(","):
                item = item.strip().split(" as ")[0].strip()
                if item and item != "*":
                    add(module + "." + item)
    for modules in PY_IMPORT.findall(text):
        for module in modules.split(","):
            add(module.strip())
    return targets


# ---------------------------------------------------------------- ts/js

JS_IMPORT = re.compile(
    r"""(?:import\s[^'"]*?from\s*|import\s*|require\s*\(\s*|import\s*\(\s*)['"]([^'"]+)['"]""",
    re.M,
)
JS_EXTS = ("", ".ts", ".tsx", ".js", ".jsx", ".mjs", ".cjs", ".d.ts",
           "/index.ts", "/index.tsx", "/index.js", "/index.jsx")


def js_edges(path, text, repo, known):
    targets = []
    for spec in JS_IMPORT.findall(text):
        if not spec.startswith("."):
            continue  # bare specifier / alias: not resolved
        base = (path.parent / spec).resolve()
        for suffix in JS_EXTS:
            candidate = Path(str(base) + suffix)
            if candidate in known:
                targets.append(candidate)
                break
    return targets


# ---------------------------------------------------------------- go

GO_IMPORT = re.compile(r'^\s*(?:[\w.]+\s+)?"([^"]+)"', re.M)


def go_edges(path, text, repo, dirs):
    targets = []
    block = re.search(r"import\s*\(([^)]*)\)", text, re.S)
    specs = GO_IMPORT.findall(block.group(1)) if block else []
    specs += re.findall(r'^import\s+"([^"]+)"', text, re.M)
    for spec in specs:
        for suffix in (spec, "/".join(spec.split("/")[1:]), "/".join(spec.split("/")[2:])):
            if suffix in dirs:
                targets.extend(dirs[suffix])
                break
    return targets


# ---------------------------------------------------------------- rust

RUST_USE = re.compile(r"^\s*use\s+((?:crate|super|self)(?:::\w+)*)", re.M)


def rust_edges(path, text, repo, known):
    targets = []
    rel = path.relative_to(repo)
    parts = list(rel.parts)
    in_src = "src" in parts
    for prefix in RUST_USE.findall(text):
        segments = prefix.split("::")
        head = segments[0]
        if head == "crate":
            base = ["src"]
            segments = segments[1:]
        elif head == "super":
            base = parts[:-1]
            while segments and segments[0] in ("super", "self"):
                if segments[0] == "super" and base:
                    base = base[:-1]
                segments = segments[1:]
            if not in_src:
                continue
        else:
            base = parts[:-1]
            segments = segments[1:]
        for cut in range(len(segments), 0, -1):
            cand = repo.joinpath(*base, *segments[:cut])
            for suffix in (".rs", "/mod.rs"):
                target = Path(str(cand) + suffix)
                if target in known:
                    targets.append(target)
                    break
            else:
                continue
            break
    return targets


# ---------------------------------------------------------------- driver

def churn(repo: Path):
    try:
        out = subprocess.run(
            ["git", "-C", str(repo), "log", "--name-only", "--pretty=format:"],
            capture_output=True, text=True, timeout=300,
        ).stdout
    except Exception:
        return {}
    counts = collections.Counter()
    for line in out.splitlines():
        line = line.strip()
        if line:
            counts[line] += 1
    return counts


def spearman(pairs):
    if len(pairs) < 3:
        return 0.0
    xs = [a for a, _ in pairs]
    ys = [b for _, b in pairs]

    def ranks(values):
        order = sorted(range(len(values)), key=lambda i: values[i])
        out = [0.0] * len(values)
        i = 0
        while i < len(order):
            j = i
            while j + 1 < len(order) and values[order[j + 1]] == values[order[i]]:
                j += 1
            for k in range(i, j + 1):
                out[order[k]] = (i + j) / 2 + 1
            i = j + 1
        return out

    rx, ry = ranks(xs), ranks(ys)
    mx, my = statistics.mean(rx), statistics.mean(ry)
    num = sum((a - mx) * (b - my) for a, b in zip(rx, ry))
    den = (sum((a - mx) ** 2 for a in rx) * sum((b - my) ** 2 for b in ry)) ** 0.5
    return num / den if den else 0.0


def main():
    repos = [Path(p) for p in sys.argv[1:]]
    rows = []
    for repo in repos:
        files = list(source_files(repo))
        by_lang = collections.defaultdict(list)
        for path in files:
            by_lang[EXT[path.suffix.lower()]].append(path)
        known = set(files)
        index_java = java_index(by_lang["java"])
        index_py = python_index(by_lang["python"], repo)
        go_dirs = collections.defaultdict(list)
        for path in by_lang["go"]:
            go_dirs[str(path.parent.relative_to(repo))].append(path)
        fan_in = collections.Counter()
        for path in files:
            ext = path.suffix.lower()
            text = strip_comment_lines(read(path))
            try:
                if ext == ".java":
                    targets = java_edges(path, text, index_java)
                elif ext in (".py", ".pyi"):
                    targets = python_edges(path, text, index_py, repo)
                elif ext in (".ts", ".tsx", ".mts", ".js", ".jsx", ".mjs", ".cjs"):
                    targets = js_edges(path, text, repo, known)
                elif ext == ".go":
                    targets = go_edges(path, text, repo, go_dirs)
                elif ext == ".rs":
                    targets = rust_edges(path, text, repo, known)
                else:
                    targets = []
            except Exception:
                targets = []
            for target in set(targets):
                if target != path:
                    fan_in[target] += 1
        churn_counts = churn(repo)
        if not churn_counts:
            continue
        for path in files:
            rel = str(path.relative_to(repo))
            keys = [rel]
            # git paths for src-layout repos sometimes carry a leading segment; try tails
            parts = rel.split("/")
            keys += ["/".join(parts[i:]) for i in range(1, min(3, len(parts)))]
            c = next((churn_counts[k] for k in keys if k in churn_counts), None)
            if c is None:
                continue
            rows.append({
                "repo": repo.name,
                "path": rel,
                "lang": EXT[path.suffix.lower()],
                "fan_in": fan_in.get(path, 0),
                "churn": c,
            })
        print(f"# {repo.name}: {len(files)} files, {len([r for r in rows if r['repo'] == repo.name])} with churn",
              file=sys.stderr)

    rows = [r for r in rows if r["churn"] > 0]
    print(f"files with churn>0: {len(rows)}")
    print(f"files with fan_in>0: {sum(1 for r in rows if r['fan_in'] > 0)}")
    print(f"spearman(fan_in, churn), all files: {spearman([(r['fan_in'], r['churn']) for r in rows]):.3f}")
    for lang in ("java", "typescript", "python", "go", "rust", "javascript"):
        sub = [r for r in rows if r["lang"] == lang]
        if len(sub) < 30:
            continue
        print(f"  {lang:<11} n={len(sub):>6} fan_in>0 {100*sum(1 for r in sub if r['fan_in']>0)/len(sub):5.1f}%"
              f"  max {max(r['fan_in'] for r in sub):>4}"
              f"  spearman(fan_in, churn) {spearman([(r['fan_in'], r['churn']) for r in sub]):.3f}")

    driven = [r for r in rows if r["fan_in"] >= 3]
    if driven:
        churns = sorted(r["churn"] for r in rows)
        p90 = churns[int(0.9 * (len(churns) - 1))]
        hot = [r for r in driven if r["churn"] >= p90]
        print(f"\nfiles with fan_in>=3: {len(driven)}; of those, churn in top decile (>= {p90}): {len(hot)}")
        print("  fan_in>=3 and high churn (the 'hotspot' candidates):")
        for r in sorted(hot, key=lambda r: (-r["fan_in"] * r["churn"]))[:15]:
            print(f"    fan_in {r['fan_in']:>3}  churn {r['churn']:>3}  {r['repo']}/{r['path']}")

    print("\n  top fan_in overall (is a widely-imported file usually stable?):")
    for r in sorted(rows, key=lambda r: (-r["fan_in"], r["path"]))[:20]:
        churns = sorted(x["churn"] for x in rows)
        rank = sum(1 for c in churns if c <= r["churn"]) / len(churns)
        print(f"    fan_in {r['fan_in']:>3}  churn {r['churn']:>3} (p{100*rank:>4.0f})  {r['repo']}/{r['path']}")

    Path(".scratch/complexity/research/coupling/fan_in.json").write_text(
        json.dumps(rows, indent=1)
    )


if __name__ == "__main__":
    main()
