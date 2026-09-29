# Length measurement data

Evidence for `docs/research/code-length-metrics.md`. This directory keeps the exact
analysis (`analyze.py`) and its saved output (`summary.txt`); the raw rows are
regenerable and deliberately not committed.

## Provenance of the numbers

Per-function rows (`functions.tsv`, not committed: path, nloc, cc, cognitive) came from
the engine's own JSON, one repo at a time:

```bash
# From the qingluan repo root. Bare `cargo` is a broken rustup proxy on this machine,
# so go through direnv; cargo also needs to write ~/.cargo, outside the file sandbox.
direnv exec . cargo build -p qingluan-cli
BIN=$PWD/target/debug/qingluan
for d in $HOME/Projects/*/; do
  (cd "$d" && "$BIN" complexity --json .) \
    | python3 -c 'import json,sys; d=json.load(sys.stdin); [print(f"{f[\"path\"]}\t{f[\"nloc\"]}\t{f[\"cc\"]}\t{f[\"cognitive\"]}") for f in d["functions"]]'
done > functions.tsv
```

Per-file rows (`files.tsv`: repo, path, file nloc, physical lines) were produced by a
throwaway `examples/measure.rs` helper that applied the kernel's own non-blank /
non-comment line rule to whole files. It was deleted after the measurement because it
duplicated the kernel's grammar/comment-kind knowledge — if the file-length axis lands
in the crate, file `nloc` becomes a first-class value and the helper is unnecessary.

Filtering, as `analyze.py` applies it: drop `third-party`, `third_party`,
`node_modules`, `site-packages`, `vendor/` (a convenience sample of one developer's
repos, not a corpus).

## Reproduce

```bash
python3 analyze.py > summary.txt   # needs functions.tsv + files.tsv in this directory
```
