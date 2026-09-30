#!/usr/bin/env python3
"""Join fan-in, age-normalised change rate and complexity for the hotspot question.

Churn alone is confounded by file age (a file added yesterday has churn 1 by
construction), so this recomputes a change *rate*: changes per month since the
file's first commit. Complexity per file comes from the real engine
(`qingluan complexity --json`, max cognitive / max cc / summed nloc per file).

Usage: python3 hotspots.py <repo> [<repo>...]
"""

import collections
import json
import statistics
import subprocess
import sys
from pathlib import Path

REPO_ROOT = Path("/home/sikongjueluo/Projects")
BIN = "/home/sikongjueluo/Projects/qingluan/target/debug/qingluan"
COUPLING = Path("/home/sikongjueluo/Projects/qingluan/.scratch/complexity/research/coupling")
NOW = 1787000000  # rough "now" in epoch seconds; only used for ratios
MONTH = 30.44 * 86400


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


def history(repo: Path):
    out = subprocess.run(
        ["git", "-C", str(repo), "log", "--name-only", "--pretty=format:%ct"],
        capture_output=True, text=True, timeout=600,
    ).stdout
    first, count = {}, collections.Counter()
    stamp = None
    for line in out.splitlines():
        line = line.strip()
        if not line:
            continue
        if line.isdigit():
            stamp = int(line)
            continue
        count[line] += 1
        if line not in first and stamp is not None:
            first[line] = stamp
    return first, count


def complexity(repo: Path):
    out = subprocess.run([BIN, "complexity", "--json", str(repo)],
                         capture_output=True, text=True, timeout=900).stdout
    if not out.strip():
        return {}
    data = json.loads(out)
    per_file = collections.defaultdict(lambda: {"max_cog": 0, "max_cc": 0, "nloc": 0, "fns": 0})
    for fn in data["functions"]:
        rel = fn["path"].removeprefix("./")
        if rel.startswith("/"):
            try:
                rel = str(Path(rel).relative_to(repo))
            except ValueError:
                continue
        entry = per_file[rel]
        entry["max_cog"] = max(entry["max_cog"], fn["cognitive"])
        entry["max_cc"] = max(entry["max_cc"], fn["cc"])
        entry["nloc"] += fn["nloc"]
        entry["fns"] += 1
    return per_file


def main():
    fan_in = json.loads((COUPLING / "fan_in.json").read_text())
    rows = []
    for repo_name in sys.argv[1:]:
        repo = REPO_ROOT / repo_name
        first, count = history(repo)
        if first:
            import datetime
            span = (datetime.date.fromtimestamp(min(first.values())),
                    datetime.date.fromtimestamp(max(first.values())))
            print(f"# {repo_name}: first file commit {span[0]}, latest {span[1]}", file=sys.stderr)
        per_file = complexity(repo)
        for row in fan_in:
            if row["repo"] != repo_name:
                continue
            path = row["path"]
            key = next((k for k in (path, path.split("/", 1)[-1]) if k in count), None)
            if key is None:
                continue
            age_months = max(0.5, (NOW - first.get(key, NOW)) / MONTH)
            rate = count[key] / age_months
            metrics = per_file.get(path) or per_file.get(path.split("/", 1)[-1]) or {}
            rows.append({
                **row,
                "changes": count[key],
                "age_months": round(age_months, 1),
                "rate": round(rate, 3),
                "max_cog": metrics.get("max_cog", 0),
                "max_cc": metrics.get("max_cc", 0),
                "nloc": metrics.get("nloc", 0),
            })

    print(f"files joined: {len(rows)}  (repos: {', '.join(sys.argv[1:])})")
    for label, key in (("changes", "changes"), ("change rate (per month)", "rate")):
        print(f"  spearman(fan_in, {label}) = {spearman([(r['fan_in'], r[key]) for r in rows]):.3f}")
    print(f"  spearman(fan_in, max cognitive) = {spearman([(r['fan_in'], r['max_cog']) for r in rows]):.3f}")
    print(f"  spearman(fan_in, file nloc)     = {spearman([(r['fan_in'], r['nloc']) for r in rows]):.3f}")
    print(f"  spearman(change rate, file nloc)= {spearman([(r['rate'], r['nloc']) for r in rows]):.3f}")

    driven = [r for r in rows if r["fan_in"] >= 3]
    if driven:
        rates = sorted(r["rate"] for r in rows)
        p90 = rates[int(0.9 * (len(rates) - 1))]
        hot = sorted([r for r in driven if r["rate"] >= p90],
                     key=lambda r: -(r["fan_in"] * r["rate"]))
        print(f"\nfan_in>=3: {len(driven)}; change rate in top decile (>= {p90:.2f}/mo): {len(hot)}")
        print("  fan_in x rate, with complexity alongside:")
        for r in hot[:18]:
            print(f"    fan_in {r['fan_in']:>3}  rate {r['rate']:>5.2f}/mo  n={r['changes']:>3}"
                  f"  maxCog {r['max_cog']:>3}  nloc {r['nloc']:>4}"
                  f"  {r['repo']}/{r['path'].split('/')[-1]}")

    print("\n  most depended-upon files (is wide fan-in a risk on its own?):")
    for r in sorted(rows, key=lambda r: (-r["fan_in"], r["path"]))[:12]:
        print(f"    fan_in {r['fan_in']:>3}  rate {r['rate']:>5.2f}/mo  n={r['changes']:>3}"
              f"  maxCog {r['max_cog']:>3}  {r['repo']}/{r['path'].split('/')[-1]}")

    (COUPLING / "hotspots.json").write_text(json.dumps(rows, indent=1))


main()
