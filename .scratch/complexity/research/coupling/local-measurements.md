# Local measurement: is file-level fan-in a useful risk signal?

Scripts: `fanin.py` (import graph + churn), `hotspots.py` (fan-in × age-normalised change
rate × real engine complexity), `cochange.py` (does an import edge predict co-change?).
Each script writes its own raw JSON next to itself (`fan_in.json` ~300 KB,
`hotspots.json` ~280 KB); those are **not committed** — regenerate them by running the
scripts (same convention as the length study's TSVs). Run with `python3 <script> <repo>...`;
`repos.txt` lists the corpus.

Corpus: 33 local repos for the fan-in pass (1935 files with churn), 3 repos with deep
enough history for the hotspot/co-change passes (BaseUI 427 commits, PlayerSync_hfc 329,
Buddycards-Core 253).

## Resolution quality (read this before quoting any number)

Import resolution is approximate and language-dependent:

| Language | files where at least one importer was found | why it is a lower bound |
| --- | --- | --- |
| Java | 53.5% | same-package references need no import and are invisible |
| Python | 41.3% | `importlib` / dynamic imports invisible |
| TypeScript/JS | 23.3% | bare specifiers and tsconfig path aliases (`@/…`) skipped |
| Rust | **2.7%** | `crate::`/`super::` resolution too naive here — **Rust numbers are unusable** |

So: Java and Python rankings are usable, TS is a heavy undercount, Rust is broken. No
cross-language comparison is meaningful.

## Finding 1 — fan-in is anti-correlated with churn (the naive hypothesis fails)

Across 1935 files with churn: `spearman(fan_in, churn) = -0.119` (Java alone: `-0.200`).
On the 3 deep-history repos and with an **age-normalised** change rate
(changes per month since the file's first commit), it gets stronger:

| pair | spearman |
| --- | --- |
| fan_in ↔ number of changes | **-0.227** |
| fan_in ↔ change rate (per month) | **-0.311** |
| fan_in ↔ max cognitive in file | **+0.025** |
| fan_in ↔ file nloc | **-0.250** |
| change rate ↔ file nloc | **+0.399** |

Read: **the more files depend on a file, the less it changes, the smaller it is, and it has
nothing to do with its complexity.** The most depended-upon file in the whole corpus is
`ComponentTypeId.java` (188 importers, 1 commit ever, max cognitive 4); then `NodeId.java`
(170, 1 commit), `PropertyKey.java` (121, 1 commit). Raw fan-in ranks the *least* risky code
at the top. That is exactly what the Stable Dependencies Principle wants: what everyone
depends on should be stable.

Caveat: BaseUI's history starts 2026-03, so many files are young and churn 1 partly means
"added recently". The age-normalised rate restores the sign (it becomes *more* negative),
so the direction is robust even if the magnitudes are corpus-specific.

## Finding 2 — fan-in × change rate is a small, sharp, intuitive set

Files with `fan_in >= 3` **and** change rate in the top decile: **18 of 1014**. They look
like what a reviewer would call risky, and the complexity axis often agrees:

| fan_in | changes | per month | file nloc | max cognitive | file |
| --- | --- | --- | --- | --- | --- |
| 21 | 89 | 10.6 | 1150 | **573** | `Buddycards-Core/.../registries/BuddycardsItems.java` |
| 29 | 58 | 116.0 | 227 | 12 | `PlayerSync_hfc/.../PlayerSync.java` |
| 11 | 30 | 35.4 | **1323** | 50 | `PlayerSync_hfc/.../database/PlayerSyncRepository.java` |
| 9 | 33 | 39.0 | **2202** | 43 | `PlayerSync_hfc/.../database/TransferRequestRepository.java` |
| 66 | 7 | 14.0 | 544 | 17 | `BaseUI/.../client/core/UIWorldContext.java` |
| 21 | 22 | 44.0 | 112 | 0 | `PlayerSync_hfc/.../config/JdbcConfig.java` |

`BuddycardsItems.java` is the same file that topped the earlier complexity scan
(cognitive 573, cc 250) **and** the length scan (1132 nloc) **and** its repo's churn (89
commits). It is the single clearest example in the corpus of "everything at once".

## Finding 3 — an import edge predicts co-change only sometimes

Co-change measured as Jaccard over the set of commits touching each end of an import
edge, versus 4000 random pairs from the same repo:

| repo | structural edges | mean Jaccard (edge) | mean Jaccard (random) | lift |
| --- | --- | --- | --- | --- |
| BaseUI | 5078 | 0.612 | 0.109 | **×5.6** |
| PlayerSync_hfc | 109 | 0.100 | 0.057 | ×1.8 |
| Buddycards-Core | 337 | 0.116 | 0.176 | **×0.7 (below random)** |

So structural coupling sometimes mirrors historical coupling and sometimes does not — the
known "logical vs structural coupling" gap. In Buddycards-Core the random baseline is
itself high (0.176), i.e. commits touch many unrelated files, which is the usual
confounder: commit granularity, not structure, drives co-change there.

**Consequence: "imports this file a lot" is a design fact, not a defect prediction.** It
needs churn alongside it to say anything about risk, and even then it is repo-dependent.
