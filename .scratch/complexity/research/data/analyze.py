#!/usr/bin/env python3
"""Empirical backing for the length-threshold research.

Reads the TSVs produced by the measurement pass (see README in this directory)
and prints distributions, tail overlap, redundancy and correlations.
Nearest-rank percentiles, matching `qingluan_complexity::distribution`.
"""

import collections
import math
import statistics
from pathlib import Path

DATA = Path(__file__).parent
VENDORED = ("third-party", "third_party", "node_modules", "site-packages", "vendor/")
LANGUAGES = {
    "rs": "rust",
    "ts": "typescript",
    "tsx": "tsx",
    "js": "javascript",
    "jsx": "jsx",
    "py": "python",
    "pyi": "python",
    "go": "go",
    "java": "java",
}


def percentile(values, p):
    if not values:
        return 0
    ordered = sorted(values)
    return ordered[max(1, math.ceil(p * len(ordered) / 100)) - 1]


def spearman(pairs):
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
    numerator = sum((a - mx) * (b - my) for a, b in zip(rx, ry))
    denominator = (sum((a - mx) ** 2 for a in rx) * sum((b - my) ** 2 for b in ry)) ** 0.5
    return numerator / denominator if denominator else 0.0


def language_of(path):
    return LANGUAGES.get(path.rsplit(".", 1)[-1].lower(), "?")


def is_test(path):
    lowered = path.lower()
    name = lowered.rsplit("/", 1)[-1]
    return (
        "/test/" in lowered
        or "/tests/" in lowered
        or name.startswith("test_")
        or "_test." in name
        or ".test." in name
        or ".spec." in name
    )


def vendored(path):
    return any(marker in path for marker in VENDORED)


functions = []
for line in (DATA / "functions.tsv").read_text().splitlines():
    repo, relative, nloc, cc, cognitive = line.split("\t")
    relative = relative.removeprefix("./")
    if vendored(relative):
        continue
    functions.append(
        {
            "repo": repo,
            "rel": relative,
            "lang": language_of(relative),
            "nloc": int(nloc),
            "cc": int(cc),
            "cognitive": int(cognitive),
        }
    )

files = []
for line in (DATA / "files.tsv").read_text().splitlines():
    repo, relative, nloc, physical = line.split("\t")
    if vendored(relative):
        continue
    files.append(
        {
            "repo": repo,
            "rel": relative,
            "lang": language_of(relative),
            "nloc": int(nloc),
            "physical": int(physical),
        }
    )

nloc = [f["nloc"] for f in functions]
cc = [f["cc"] for f in functions]
cognitive = [f["cognitive"] for f in functions]
repo_counts = collections.Counter(f["repo"] for f in functions)

print(f"functions {len(functions)} in {len(files)} files, {len(repo_counts)} repos")
print(f"languages: {dict(collections.Counter(f['lang'] for f in functions))}")

print("\n== function nloc ==")
for p in (50, 75, 90, 95, 99, 99.9):
    print(f"  p{p}: {percentile(nloc, p)}", end="")
print(f"\n  max {max(nloc)}  mean {statistics.mean(nloc):.1f}")
for limit in (40, 50, 60, 80, 100, 150, 200, 300):
    over = sum(1 for v in nloc if v > limit)
    print(f"  > {limit:>3}: {over:>5} ({100 * over / len(nloc):5.2f}%)")

print("\n== file nloc ==")
fnloc = [f["nloc"] for f in files]
for p in (50, 75, 90, 95, 99, 99.9):
    print(f"  p{p}: {percentile(fnloc, p)}", end="")
print(f"\n  max {max(fnloc)}  mean {statistics.mean(fnloc):.1f}")
for limit in (200, 300, 500, 750, 1000, 2000):
    over = sum(1 for v in fnloc if v > limit)
    print(f"  > {limit:>4}: {over:>5} ({100 * over / len(fnloc):5.2f}%)")

print("\n== length vs complexity ==")
print(f"  spearman(nloc, cognitive)      {spearman(list(zip(nloc, cognitive))):.3f}")
print(f"  spearman(nloc, cc)             {spearman(list(zip(nloc, cc))):.3f}")
print(f"  spearman(cc, cognitive)        {spearman(list(zip(cc, cognitive))):.3f}")
long_flat = [f for f in functions if f["nloc"] > 80 and f["cc"] <= 10]
short_hairy = [f for f in functions if f["nloc"] <= 40 and f["cc"] > 10]
print(f"  long and flat   (nloc>80, cc<=10): {len(long_flat)}")
print(f"  short and hairy (nloc<=40, cc>10): {len(short_hairy)}")
worst = collections.defaultdict(lambda: (0, 0, 0))
for f in functions:
    key = (f["repo"], f["rel"])
    cog, ccmax, count = worst[key]
    worst[key] = (max(cog, f["cognitive"]), max(ccmax, f["cc"]), count + 1)
pairs = [(f["nloc"], worst[(f["repo"], f["rel"])][0]) for f in files]
print(f"  spearman(file nloc, worst cognitive in file) {spearman(pairs):.3f}")

print("\n== does file length add anything? ==")
for limit in (300, 500, 750, 1000):
    long_files = [f for f in files if f["nloc"] > limit]
    flagged = [
        f
        for f in long_files
        if worst[(f["repo"], f["rel"])][0] > 15 or worst[(f["repo"], f["rel"])][1] > 10
    ]
    print(
        f"  files > {limit:>4}: {len(long_files):>3}  already flagged by a function metric {len(flagged):>3}"
        f" ({100 * len(flagged) / len(long_files):5.1f}%)  silently long {len(long_files) - len(flagged):>3}"
    )

print("\n== test vs production ==")
for label, rows in (
    ("functions, non-test", [f for f in functions if not is_test(f["rel"])]),
    ("functions, tests", [f for f in functions if is_test(f["rel"])]),
):
    values = [f["nloc"] for f in rows]
    print(
        f"  {label:<20} n={len(values):>5} p90 {percentile(values, 90):>3} p95 {percentile(values, 95):>3}"
        f" p99 {percentile(values, 99):>4}  >80 {100 * sum(1 for v in values if v > 80) / len(values):5.2f}%"
    )
for label, rows in (
    ("files, non-test", [f for f in files if not is_test(f["rel"])]),
    ("files, tests", [f for f in files if is_test(f["rel"])]),
):
    values = [f["nloc"] for f in rows]
    print(
        f"  {label:<20} n={len(values):>5} p50 {percentile(values, 50):>4} p90 {percentile(values, 90):>4}"
        f"  >750 {sum(1 for v in values if v > 750):>3}"
    )

print("\n== per language: function nloc ==")
for lang in sorted({f["lang"] for f in functions}):
    values = [f["nloc"] for f in functions if f["lang"] == lang]
    if len(values) < 50:
        continue
    print(
        f"  {lang:<12} n={len(values):>5} p50 {percentile(values, 50):>3} p90 {percentile(values, 90):>3}"
        f" p95 {percentile(values, 95):>3} p99 {percentile(values, 99):>4}"
        f"  >80 {100 * sum(1 for v in values if v > 80) / len(values):5.2f}%"
    )

print("\n== per repo (>=200 functions) ==")
for repo, count in repo_counts.most_common():
    if count < 200:
        continue
    values = [f["nloc"] for f in functions if f["repo"] == repo]
    print(
        f"  {repo:<20} n={count:>5} p90 {percentile(values, 90):>4} p99 {percentile(values, 99):>4}"
        f" max {max(values):>4}  >50 {100 * sum(1 for v in values if v > 50) / count:5.2f}%"
        f"  >80 {100 * sum(1 for v in values if v > 80) / count:5.2f}%"
    )

print("\n== worst functions by nloc (top 10) ==")
for f in sorted(functions, key=lambda f: (-f["nloc"], f["rel"]))[:10]:
    print(f"  nloc {f['nloc']:>4} cc {f['cc']:>3} cog {f['cognitive']:>3}  {f['repo']}/{f['rel']}")

print("\n== longest files (top 10) ==")
for f in sorted(files, key=lambda f: (-f["nloc"], f["rel"]))[:10]:
    cog, ccmax, count = worst[(f["repo"], f["rel"])]
    print(
        f"  nloc {f['nloc']:>5} functions {count:>4} worst cog {cog:>3} worst cc {ccmax:>3}"
        f"  {f['repo']}/{f['rel']}"
    )
