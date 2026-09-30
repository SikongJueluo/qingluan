# Martin's component/package coupling principles — verified primary-source quotations

Scope: exact, quotable statements of **SDP / SAP / ADP** as written by Robert C. Martin, plus the
origin of the term **"morning after syndrome"**. All quotes below were read out of the actual
document text (PDF → `pdftotext`, or archived publisher page HTML); nothing is reconstructed from
memory.

All URLs accessed **2026-09-30**.

Confidence vocabulary used below (as requested):

| marker | meaning |
| --- | --- |
| `verified-in-full-text` | I downloaded/opened the primary document and read the sentence verbatim. |
| `verified-via-publisher-preview` | Exact book sentence read from the publisher's own public "content preview" (not a snippet engine). |
| `verified-via-google-books-snippet` | (used only if it applies — it does **not** here; Google Books blocked this session) |
| `secondary-only` | Quote exists only in a third-party document that cites Martin; not read in Martin's own text. |
| `not-verified` | I could not obtain the sentence. Do not quote it as a quotation. |

---

## Network/method note (important for reproducing this)

`web.archive.org` is **unreachable through the mihomo proxy** (TLS handshake to
`web.archive.org:443` times out; plain HTTP returns `502` from the proxy). It **does** work with the
proxy bypassed:

```bash
curl -sL --noproxy '*' 'https://web.archive.org/web/20030405064407id_/http://www.objectmentor.com/resources/articles/granularity.pdf'
```

Everything else (`archive.org`, `books.google.com`, publisher sites) went through the normal proxy.
`/tmp` is per-command ephemeral, so every download was followed by its extraction in the same shell
invocation.

---

## 1. Stable Dependencies Principle (SDP)

### 1a. Authoritative statement — Martin's own C++ Report column (primary, full text)

> **THE DEPENDENCIES BETWEEN PACKAGES IN A DESIGN SHOULD BE IN THE DIRECTION OF THE STABILITY OF
> THE PACKAGES. A PACKAGE SHOULD ONLY DEPEND UPON PACKAGES THAT ARE MORE STABLE THAT IT IS.**

- Source: Robert C. Martin, **"Stability"**, *The C++ Report*, "Engineering Notebook" column #6
  (the article itself only says "the sixth of my Engineering Notebook columns for The C++ Report";
  the issue month is **not stated in the PDF**. The preceding column, "Granularity", is dated
  Nov/Dec 1996 in this article's own reference list, so "Stability" follows that).
  PDF page 8 of the archived file.
- Author's own copy (PDF, archived): <https://web.archive.org/web/20030405111751/http://www.objectmentor.com/resources/articles/stability.pdf>
  (raw file: `https://web.archive.org/web/20030405111751id_/http://www.objectmentor.com/resources/articles/stability.pdf`,
  38 142 bytes, 15 pages)
- Original URL (dead): `http://www.objectmentor.com/resources/articles/stability.pdf`
- **Verbatim caveat:** the printed text really does read "MORE STABLE **THAT** IT IS" — a typo for
  "than". Reproduce it exactly if you are quoting; add `[sic]` if you prefer.
- Confidence: **`verified-in-full-text`**

Same section, the operational restatement (PDF p. 10):

> **The SDP says that the I metric of a package should be larger than the I metrics of the packages
> that it depends upon. i.e. I metrics should decrease in the direction of dependency.**

### 1b. Martin's one-line canonical form (primary, author's own website)

> **Depend in the direction of stability.**

- Source: Robert C. Martin, "Principles of OOD" (page, section **SDP — The Stable Dependencies
  Principle**)
- URL: <http://www.butunclebob.com/ArticleS.UncleBob.PrinciplesOfOod> (HTTP 200, fetched 2026-09-30)
- Confidence: **`verified-in-full-text`**

### 1c. *Clean Architecture* (2017) ch. 14 — NOT VERIFIED

The candidate formulation **"The dependencies between components must be in the direction of
stability."** could **not** be confirmed against the book: see "Dead ends" — only the first
paragraph of ch. 14 is exposed by the publisher preview. Do not present it as a verbatim book quote
without further verification.

- Confidence: **`not-verified`** (the sentence is plausible and consistent with 1a/1b, but that is
  not verification).

---

## 2. Stable Abstractions Principle (SAP)

### 2a. Authoritative statement — Martin's own C++ Report column (primary, full text)

> **PACKAGES THAT ARE MAXIMALLY STABLE SHOULD BE MAXIMALLY ABSTRACT. INSTABLE PACKAGES SHOULD BE
> CONCRETE. THE ABSTRACTION OF A PACKAGE SHOULD BE IN PROPORTION TO ITS STABILITY.**

- Source: Robert C. Martin, **"Stability"**, *The C++ Report*, Engineering Notebook column #6,
  section "The Stable Abstractions Principle (SAP)", PDF page 11.
- URL: <https://web.archive.org/web/20030405111751/http://www.objectmentor.com/resources/articles/stability.pdf>
- Confidence: **`verified-in-full-text`**

Immediately following, the relation to SDP (same section, PDF p. 11):

> **The SAP and the SDP combined amount to the Dependency Inversion Principle for Packages.**

### 2b. Martin's one-line canonical form (primary, author's own website)

> **Abstractness increases with stability.**

- Source: Robert C. Martin, "Principles of OOD", section **SAP — The Stable Abstractions Principle**
- URL: <http://www.butunclebob.com/ArticleS.UncleBob.PrinciplesOfOod>
- Confidence: **`verified-in-full-text`**

### 2c. The other candidate — "A component should be as abstract as it is stable."

Not found in any Martin primary text I could open. It appears verbatim in a 2020 TU Wien diploma
thesis, cited to Martin's *Agile Software Development: Principles, Patterns, and Practices* (PPP):

- Oberweger, Roland, *An open-source tool for detecting violations of design principles* (TU Wien,
  2020), §4.15: `"A component should be as abstract as it is stable." [29]`, where `[29]` = Robert
  Cecil Martin, *Agile Software Development: Principles, Patterns, and Practices*, Prentice Hall
  PTR, 2003, ISBN 0135974445.
- URL: <https://repositum.tuwien.at/bitstream/20.500.12708/1097/2/Oberweger%20Roland%20-%202020%20-%20An%20open-source%20tool%20for%20detecting%20violations%20of...pdf>
- Confidence: **`secondary-only`** (third-party thesis quoting PPP; I could not open PPP itself).

Same thesis (§4.14) quotes SDP as `"Depend in the direction of stability." [29]` — consistent with
Martin's own page (1b), which is the primary confirmation.

---

## 3. Acyclic Dependencies Principle (ADP)

### 3a. Authoritative statement — Martin's own C++ Report column (primary, full text)

> **THE DEPENDENCY STRUCTURE BETWEEN PACKAGES MUST BE A DIRECTED ACYCLIC GRAPH (DAG). THAT IS,
> THERE MUST BE NO CYCLES IN THE DEPENDENCY STRUCTURE.**

- Source: Robert C. Martin, **"Granularity"**, *The C++ Report*, Engineering Notebook column #5,
  section "The Acyclic Dependencies Principle (ADP)", PDF page 6.
- Author's own copy (PDF, archived): <https://web.archive.org/web/20030405064407/http://www.objectmentor.com/resources/articles/granularity.pdf>
  (raw: `https://web.archive.org/web/20030405064407id_/http://www.objectmentor.com/resources/articles/granularity.pdf`,
  39 854 bytes, 12 pages)
- Original URL (dead): `http://www.objectmentor.com/resources/articles/granularity.pdf`
- The article's own reference list (reproduced in "Stability") dates **ADP to Nov/Dec 1996**.
- Confidence: **`verified-in-full-text`**

The same statement is restated in the reference list near the front of `stability.pdf` (PDF p. 3,
item 8). It does **not** appear in `granularity.pdf`'s own reference list — that list stops at item
4, because ADP is introduced in that very article:

> **8. The Acyclic Dependencies Principle (ADP) Nov/Dec, 1996.The dependency structure between
> packages must be a Directed Acyclic Graph (DAG). That is, there must be no cycles in the
> dependency structure.**

### 3b. Martin's one-line canonical form (primary, author's own website)

> **The dependency graph of packages must have no cycles.**

- Source: Robert C. Martin, "Principles of OOD", section **ADP — The Acyclic Dependencies
  Principle**
- URL: <http://www.butunclebob.com/ArticleS.UncleBob.PrinciplesOfOod>
- Confidence: **`verified-in-full-text`**

### 3c. *Clean Architecture* (2017) ch. 14 (primary, publisher's own content preview)

> **Allow no cycles in the component dependency graph.**

- Source: Robert C. Martin, *Clean Architecture: A Craftsman's Guide to Software Structure and
  Design*, Chapter 14 "Component Coupling", first principle heading "THE ACYCLIC DEPENDENCIES
  PRINCIPLE".
- URL of the publisher page: <https://www.oreilly.com/library/view/clean-architecture-a/9780134494272/ch14.xhtml>
  (live page returns **403** to curl). Read from the Wayback snapshot of that same publisher page:
  <https://web.archive.org/web/20250831114934/https://www.oreilly.com/library/view/clean-architecture-a/9780134494272/ch14.xhtml>
  (section "Content preview from Clean Architecture…", 14 COMPONENT COUPLING).
- ISBN: 9780134494272 (Pearson/O'Reilly).
- Confidence: **`verified-via-publisher-preview`** — this is the publisher's public preview text,
  not a general web snippet. The preview truncates immediately after the ADP paragraph, which is why
  SDP/SAP book wording remains unverified.

---

## 4. "Morning after syndrome"

**Which primary text introduces it:** Martin's **"Granularity"** column, *The C++ Report*,
Nov/Dec 1996 (Engineering Notebook column #5), in the ADP section — i.e. the same article that
introduces ADP. This is the earliest text I could verify, and it is Martin's own published text.

### 4a. First use / definition (primary, full text)

> **Have you ever worked all day, gotten some stuff working and then gone home; only to arrive the
> next morning at to find that your stuff no longer works? Why doesn't it work? Because somebody
> stayed later than you! I call this: "the morning after syndrome".**

> **The "morning after syndrome" occurs in development environments where many developers are
> modifying the same source files. In relatively small projects with just a few developers, it isn't
> too big a problem. But as the size of the project and the development team grows, the mornings
> after can get pretty nightmarish. It is not uncommon for weeks to go by without being able to
> build a stable version of the project. Instead, everyone keeps on changing and changing their code
> trying to make it work with the last changes that someone else made.**

- Source: Robert C. Martin, "Granularity", *The C++ Report*, column #5, ADP section, PDF page 6
  (second paragraph continues onto PDF page 7).
- URL: <https://web.archive.org/web/20030405064407/http://www.objectmentor.com/resources/articles/granularity.pdf>
- **Verbatim caveat:** the printed sentence reads "arrive the next morning **at to** find" — the
  stray "at" is in the original. Reproduce exactly or mark `[sic]`.

And, tying the term to cycles in the dependency graph (PDF p. 7):

> **If there are cycles in the dependency structure then the "morning after syndrome" cannot be
> avoided.**

And again, on the consequence of a cycle (PDF p. 9):

> **…any of those packages will experience "the morning after syndrome" once again.**

- Confidence: **`verified-in-full-text`** — **YES, the term is confirmed in a primary text.**

### 4b. *Clean Architecture* (2017) ch. 14 (primary, publisher's own content preview, truncated)

> **Have you ever worked all day, gotten some stuff working, and then gone home, only to arrive the
> next morning to find that your stuff no longer works? Why doesn't it work? Because somebody stayed
> later than you and changed something you depend on! I call this "the morning after …**

- Source: *Clean Architecture*, ch. 14, under "THE ACYCLIC DEPENDENCIES PRINCIPLE". The publisher
  preview cuts off mid-sentence at "the morning after …" (paywall boundary).
- URL: <https://web.archive.org/web/20250831114934/https://www.oreilly.com/library/view/clean-architecture-a/9780134494272/ch14.xhtml>
- Confidence: **`verified-via-publisher-preview`** (partial). The book clearly reuses the term; the
  remainder of the book's definition is not visible in the preview.

### 4c. PPP (2002/2003)

Not directly readable this session (see Dead ends). The term is nonetheless established in the
original 1996 column (4a), which is the primary origin, so PPP adds nothing needed here.

- Confidence: **`not-verified`** for PPP specifically.

---

## 5. Dead ends (do not repeat these)

| # | What was tried | Result |
| --- | --- | --- |
| 1 | `web.archive.org` through the mihomo proxy (HTTPS) | TLS handshake timeout (30 s), `000`; plain HTTP gets `502 Bad Gateway` from the proxy. **Workaround: `curl --noproxy '*'` succeeds.** |
| 2 | `web.archive.org/web/2018…/objectmentor.com/resources/articles/sdp.pdf` and `sap.pdf` | HTTP 200 but **1197-byte HTML parking pages** ("www.objectmentor.com", `pageok` markers) — the 404-stub problem the task warned about is real. Same for `adp.pdf` (CDX shows only 3 snapshots, all `text/html`). |
| 3 | Wayback CDX for `objectmentor.com/resources/articles*` | Works (with `--noproxy '*'`); the principles actually live in **`granularity.pdf`** (ADP + morning-after) and **`stability.pdf`** (SDP + SAP), not in `adp/sdp/sap.pdf`. |
| 4 | `https://www.oreilly.com/library/view/clean-architecture-a/9780134494272/ch14.xhtml` (live) | **403** to curl. |
| 5 | Same URL via Wayback (5 snapshots checked) | Works, but each exposes only the **"Content preview"** — front matter + the ADP heading, statement and first paragraph; truncates at `I call this "the morning after …`. No SDP/SAP book text. |
| 6 | `https://www.googleapis.com/books/v1/volumes?q=…` | `429 RESOURCE_EXHAUSTED`; anonymous per-day quota is literally **0** for this environment. Won't work without an API key. |
| 7 | `books.google.com/books?id=…&q=…` and the `jscmd=SearchWithinVolume` JSON endpoint | Either the anti-bot interstitial (`429`, "Our systems have detected unusual traffic", IP 203.175.14.58) or a redirect to `books.google.cn`'s "moved" page. Tried `books.google.com/.de/.co.uk`, with and without the proxy, with browser UA. **No Google Books snippet was obtainable.** |
| 8 | Volume IDs | Print/no-preview edition `8ngAkAEACAAJ`; ebook-ish edition `uGE1DwAAQBAJ` (both from the ISBN-9780134494166 landing page). Neither could be queried. |
| 9 | Internet Archive lending copy `cleanarchitectur0000mart` | Metadata OK; `access-restricted-item: true`. `_djvu.txt` download → **`401 Authorization Required`**. Not attempted any further — borrowing restrictions left intact. |
| 10 | IA search-inside APIs (`ia-fts.archive.org`, `ia-pub-fts-api.archive.org`, `api.archivelab.org`, `dn760101.eu.archive.org/BookReader/BookReaderSearch.php`, `…/fulltext/inside.php`) | All unreachable (`000`, SSL connect error) or `404`. `ia-fts.*` resolves to **IPv6-only** `2001:480:abcd::81` with a bogus A record `28.0.0.129` via the local resolver. |
| 11 | `https://openlibrary.org/search/inside?q=…` and `/search/inside.json?q=…` | Works (JSON, 164 hits for "morning after syndrome"), **but** the index does **not** contain *Clean Architecture*: `"Screaming Architecture"` → 0 hits, `"Allow no cycles in the component dependency graph"` → 0 hits. Fine for public-domain/other works, useless for this book. |
| 12 | `https://archive.org/search?query=…&sin=TXT` equivalents | No public JSON backend was reachable. |
| 13 | `https://wwwzb.fz-juelich.de/contentenrichment/inhaltsverzeichnisse/2019/9780134494166.pdf` and `https://vlb-content.vorarlberg.at/fhbscan1/330900104500.pdf` | Both legitimately public, but they are only **cover + table of contents** (the vlb scan is 9 pages), not the chapter. |
| 14 | IA item `pearson.-agile.-software.-development.-principles.-patterns.and.-practices.www.-ebooks-world.ir` | Looks like an unauthorized upload of PPP. **Not used.** |
| 15 | 1994 paper "OO Design Quality Metrics" — <https://web.archive.org/web/20080514133459id_/http://www.objectmentor.com/resources/articles/oodmetrc.pdf> | Accessible (8 pp., works; the `web/2007id_/` URL the task gave also resolves), but it predates SDP/SAP/ADP. It only contains the precursor idea: "a 'Good Dependency' is a dependency upon something that is very stable." **No SDP/SAP/ADP wording here.** |

### Still-open lead for the book wording

*Clean Architecture* ch. 14 SDP/SAP sentences (pp. ~120 and ~126 in the printed book) remain
**unverified**. The only routes not exhausted are Google Books snippet view (blocked by captcha this
session) and the IA lending copy (restricted). A fresh session from a different egress IP, or a
borrow of the IA copy, should close it.

---

## Bottom line

| Principle | Exact quote to use | Source | Confidence |
| --- | --- | --- | --- |
| **SDP** | "THE DEPENDENCIES BETWEEN PACKAGES IN A DESIGN SHOULD BE IN THE DIRECTION OF THE STABILITY OF THE PACKAGES. A PACKAGE SHOULD ONLY DEPEND UPON PACKAGES THAT ARE MORE STABLE THAT IT IS." | Martin, "Stability", *The C++ Report* col. #6, p. 8 | **verified-in-full-text** |
| **SDP** (short form) | "Depend in the direction of stability." | Martin, "Principles of OOD" | **verified-in-full-text** |
| **SAP** | "PACKAGES THAT ARE MAXIMALLY STABLE SHOULD BE MAXIMALLY ABSTRACT. INSTABLE PACKAGES SHOULD BE CONCRETE. THE ABSTRACTION OF A PACKAGE SHOULD BE IN PROPORTION TO ITS STABILITY." | Martin, "Stability", *The C++ Report* col. #6, p. 11 | **verified-in-full-text** |
| **SAP** (short form) | "Abstractness increases with stability." | Martin, "Principles of OOD" | **verified-in-full-text** |
| **ADP** | "THE DEPENDENCY STRUCTURE BETWEEN PACKAGES MUST BE A DIRECTED ACYCLIC GRAPH (DAG). THAT IS, THERE MUST BE NO CYCLES IN THE DEPENDENCY STRUCTURE." | Martin, "Granularity", *The C++ Report* col. #5, p. 6 | **verified-in-full-text** |
| **ADP** (short form) | "The dependency graph of packages must have no cycles." | Martin, "Principles of OOD" | **verified-in-full-text** |
| **ADP** (*Clean Architecture* form) | "Allow no cycles in the component dependency graph." | *Clean Architecture*, ch. 14 (O'Reilly content preview) | **verified-via-publisher-preview** |
| **morning after syndrome** | "…only to arrive the next morning at to find that your stuff no longer works? … Because somebody stayed later than you! I call this: 'the morning after syndrome'." | Martin, "Granularity", *The C++ Report* Nov/Dec 1996, col. #5, p. 6 | **verified-in-full-text** |
