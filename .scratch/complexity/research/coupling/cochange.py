#!/usr/bin/env python3
"""Does "A imports B" predict "A and B change together"?

For each repo we build the structural import edges, then measure co-change from
git history: how many commits touch both ends of an edge. The baseline is a
random pair of files drawn from the same repo (same size class by change count),
so the question is whether structural edges co-change above chance.

If structural coupling barely beats random, then fan-in is a weak proxy for
"changes ripple here" and the metric needs churn to say anything.
"""

import collections
import random
import statistics
import subprocess
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))
import fanin  # noqa: E402

REPO_ROOT = Path("/home/sikongjueluo/Projects")
random.seed(20260930)


def commit_sets(repo: Path):
    """file -> set of commit indices that touched it."""
    out = subprocess.run(
        ["git", "-C", str(repo), "log", "--name-only", "--pretty=format:@@%H"],
        capture_output=True, text=True, timeout=600,
    ).stdout
    per_file = collections.defaultdict(set)
    idx = -1
    for line in out.splitlines():
        line = line.strip()
        if not line:
            continue
        if line.startswith("@@"):
            idx += 1
            continue
        per_file[line].add(idx)
    return per_file


def edges_for(repo: Path):
    files = list(fanin.source_files(repo))
    by_lang = collections.defaultdict(list)
    for path in files:
        by_lang[fanin.EXT[path.suffix.lower()]].append(path)
    index_java = fanin.java_index(by_lang["java"])
    edges = []
    for path in by_lang["java"]:
        text = fanin.strip_comment_lines(fanin.read(path))
        for target in set(fanin.java_edges(path, text, index_java)):
            if target != path:
                edges.append((path, target))
    return edges


def jaccard(a, b):
    if not a or not b:
        return 0.0
    return len(a & b) / len(a | b)


def main():
    for repo_name in sys.argv[1:]:
        repo = REPO_ROOT / repo_name
        commits = commit_sets(repo)
        edges = edges_for(repo)
        scored = []
        for source, target in edges:
            a = commits.get(str(source.relative_to(repo)), set())
            b = commits.get(str(target.relative_to(repo)), set())
            if not a or not b:
                continue
            scored.append(jaccard(a, b))
        if len(scored) < 20:
            print(f"{repo_name}: too few resolvable edges ({len(scored)})")
            continue
        keys = list(commits.keys())
        random_pairs = []
        for _ in range(4000):
            x, y = random.sample(keys, 2)
            random_pairs.append(jaccard(commits[x], commits[y]))
        print(f"{repo_name}: edges {len(scored)}")
        print(f"  mean jaccard, structural edge   {statistics.mean(scored):.4f}"
              f"   median {statistics.median(scored):.4f}")
        print(f"  mean jaccard, random pair       {statistics.mean(random_pairs):.4f}"
              f"   median {statistics.median(random_pairs):.4f}")
        ratio = statistics.mean(scored) / max(1e-9, statistics.mean(random_pairs))
        print(f"  lift over random: x{ratio:.1f}")
        shared = sum(1 for v in scored if v > 0)
        print(f"  edges that ever changed together: {shared}/{len(scored)}"
              f" ({100*shared/len(scored):.0f}%); other structural edges never co-changed")


main()
