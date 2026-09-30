# Topic 4 — What is actually computable without a call graph, per language

Scope: a tree-sitter-based Rust daemon that reads **one file at a time** and has a CST only —
**no name resolution, no type information, no call graph, no build system**. Question: for
Rust / TypeScript / JavaScript / Python / Go / Java, what can such a tool actually resolve,
and where does it silently lie?

Method note. Every construct below was parsed with the **official tree-sitter grammars**
(`uv run --with tree-sitter-rust …`, one snippet per form) and the resulting tree dumped with
node type + field names; the node shapes quoted here are what the grammar actually produced,
not what the docs claim. Grammar revisions the node-type dumps were taken from (GitHub HEAD at
the time of writing):

| grammar | commit |
| --- | --- |
| [tree-sitter-rust](https://github.com/tree-sitter/tree-sitter-rust) | `77a3747266f4d621d0757825e6b11edcbf991ca5` |
| [tree-sitter-typescript](https://github.com/tree-sitter/tree-sitter-typescript) | `75b3874edb2dc714fb1fd77a32013d0f8699989f` |
| [tree-sitter-javascript](https://github.com/tree-sitter/tree-sitter-javascript) | `58404d8cf191d69f2674a8fd507bd5776f46cb11` |
| [tree-sitter-python](https://github.com/tree-sitter/tree-sitter-python) | `26855eabccb19c6abf499fbc5b8dc7cc9ab8bc64` |
| [tree-sitter-go](https://github.com/tree-sitter/tree-sitter-go) | `2346a3ab1bb3857b48b29d779a1ef9799a248cd7` |
| [tree-sitter-java](https://github.com/tree-sitter/tree-sitter-java) | `e10607b45ff745f5f876bfa3e94fbcc6b44bdc11` |

"One file at a time" is the operative constraint. Most languages cannot resolve an import
*from one file* even in principle, because the mapping specifier → file lives in a **project
config file** (`go.mod`, `Cargo.toml`, `tsconfig.json`, `package.json`, classpath, `sys.path`).
Those are still *syntactic* inputs — reading them costs no type information — so the honest
verdict vocabulary is three-valued:

| verdict | meaning |
| --- | --- |
| **YES** | the literal target is recoverable from this file's CST alone (plus the filesystem). |
| **YES+CFG** | recoverable only if the tool also reads project config (`go.mod`/`Cargo.toml`/`tsconfig.json`/`package.json`/module tree), then matches specifier→path by string/glob rules. Still no name resolution. |
| **NO** | the target is a compile-time or run-time value the CST does not contain. |

---

## TL;DR

1. **For every one of the six languages, a single-file CST pass recovers the *shape* of a
   dependency, not its *target***. What you get is a specifier string (`"./a"`, `"lib/math"`)
   or a path segment list (`crate::a::b`, `java.util.List`). Turning that into a file path
   needs project config + a workspace scan in all six.
2. **The import graph is reliably computable as a set of *unresolved* references, and only
   conditionally computable as a set of *edges*.** The resolution step is where per-language
   recall collapses — measured on the local 33-repo corpus it ranges from 53.5% (Java) to 2.7%
   (Rust), see §7.6.
3. **fan-in / fan-out are downstream of edge resolution and inherit 100% of its error.**
   fan-out (out-edges of one file) is *far* more reliable than fan-in, because fan-out only
   needs the current file's specifiers resolved one level, while fan-in needs *every other
   file in the repo* to have been parsed and resolved correctly.
4. **Cycles (SCCs) are computable by a standard algorithm (Tarjan) but are the *least*
   trustworthy output**, because a cycle requires a closed loop of edges: one dropped edge
   destroys a real cycle, and one spurious edge invents a fake one. Both failure modes occur
   in all six languages (§7.3).
5. **Layering violations are not computable from syntax at all — the layer policy is declared
   project config.** ArchUnit, the reference implementation of the idea for Java, makes this
   explicit: the user writes
   `layeredArchitecture().layer("Controller").definedBy("..controller..")` — see
   <https://www.archunit.org/userguide/html/000_Index.html>. Without that declaration there is
   no notion of "layer" in any language spec.
6. **The single biggest silent-lie per language** (details in each section): Rust
   `#[cfg(...)] mod x;` and `macro_rules!`-generated modules; TypeScript `paths` aliases and
   `package.json` `"exports"`; JavaScript non-literal `require(expr)`; Python
   `from x import *` + `importlib.import_module`; Go `_GOOS`/`_GOARCH` file selection;
   Java same-package references that appear in **no import statement at all**.

---

## 1. Rust

### 1.1 Construct → can a syntax-only tool resolve it? (one file at a time)

| Construct | Exact syntax form | Verdict | Why / source |
| --- | --- | --- | --- |
| Outlined module | `mod foo;` | **YES** for the immediate file (`foo.rs` or `foo/mod.rs` beside the current file); **YES+CFG** for the full logical path | *"A module without a body is loaded from an external file. When the module does not have a path attribute, the path to the file mirrors the logical module path."* / *"Ancestor module path components are directories, and the module's contents are in a file with the name of the module plus the `.rs` extension."* — <https://doc.rust-lang.org/reference/items/modules.html> |
| Ambiguous both spellings | `util.rs` **and** `util/mod.rs` | — | *"It is not allowed to have both `util.rs` and `util/mod.rs`."* — same URL. Only one can exist, so a filesystem probe disambiguates. |
| Inline module | `mod foo { … }` | **YES** (body is in this file; no external target). A nested `mod bar;` inside it needs mod-rs vs non-mod-rs classification → **YES+CFG** | Syntax rule: `Module → unsafe? mod IDENTIFIER ; \| unsafe? mod IDENTIFIER { InnerAttribute* Item* }` — <https://doc.rust-lang.org/reference/items/modules.html> |
| Path attribute | `#[path = "other.rs"] mod c;` | **YES+CFG** | *"The directories and files used for loading external file modules can be influenced with the `path` attribute."* / *"For path attributes on modules not inside inline module blocks, the file path is relative to the directory the source file is located."* / *"For path attributes inside inline module blocks, the relative location of the file path depends on the kind of source file the path attribute is located in."* — same URL |
| Conditional path | `#[cfg_attr(target_os = "linux", path = "linux.rs")] mod os;` | **NO** | The Reference's own example: *"The following module will either be found at `linux.rs` or `windows.rs` based on the target."* — <https://doc.rust-lang.org/reference/conditional-compilation.html> |
| cfg-gated module | `#[cfg(feature = "x")] mod gated;` | **NO** | *"Which configuration options are set is determined statically during the compilation of the crate."* / *"If any predicate is false, the form is removed from the source code."* — same URL |
| Absolute-crate use | `use crate::x;` | **YES+CFG** | *"`crate` resolves the path relative to the current crate."* / *"`crate` can only be used as the first segment, without a preceding `::`."* — needs the crate-root file / a syntactic module-tree scan. <https://doc.rust-lang.org/reference/paths.html> |
| Parent / self | `use super::y;`, `use self::z;` | **YES+CFG** / **YES** | *"`super` in a path resolves to the parent module."* / *"`self` resolves the path relative to the current module."* — same URL |
| Bare leading segment | `use foo::bar;` | **NO** without `Cargo.toml`; **YES+CFG** with it | In the 2018 edition a bare first segment resolves via the extern prelude (crate names), *"External crates imported with `extern crate` in the root module or provided to the compiler (as with the `--extern` flag with rustc) are added to the extern prelude."* — <https://doc.rust-lang.org/reference/names/preludes.html#extern-prelude> |
| Leading `::` | `use ::top::level;` | **NO** for external crates (2018); **YES+CFG** in 2015 | *"Beginning with the 2018 Edition, paths starting with `::` resolve from crates in the extern prelude. That is, they must be followed by the name of a crate."* — <https://doc.rust-lang.org/reference/paths.html> |
| Dependency rename | `bar = { package = "foo", … }` in `Cargo.toml` | **NO** from the `.rs` file; **YES+CFG** | *"the key you write for a dependency typically matches up to the name of the crate you import from in the code"* / *"we're explicitly using the `package` key to inform Cargo that we want the `foo` package even though we're calling it something else locally."* — <https://doc.rust-lang.org/cargo/reference/specifying-dependencies.html> |
| Re-export | `pub use inner::Thing;` | **YES+CFG** (same path resolution); **NO** if it re-exports a dependency item | *"A `use` declaration creates one or more local name bindings synonymous with some other path."* / *"Such a use declaration serves to re-export a name."* — <https://doc.rust-lang.org/reference/items/use-declarations.html> |
| Grouped / aliased / glob | `use foo::{a, b::c};`, `use foo::bar as baz;`, `use foo::*;` | **YES+CFG** for the module path; the *name set* of a glob is not enumerable | *"The `*` character may be used as the last segment of a use path to import all importable entities from the entity of the preceding segment."* — same URL |
| `extern crate` | `extern crate serde as sd;` | **NO** to a file | Crate identity comes from Cargo/`--extern`, not from source; only the alias string is extractable. <https://doc.rust-lang.org/reference/names/preludes.html#extern-prelude> |
| `include!` | `include!("generated.rs");` | **YES** for a literal argument (path relative to the current file); **NO** if the argument is macro-produced | *"The included file is located relative to the current file (similarly to how modules are found). The provided path is interpreted in a platform-specific way at compile time."* — <https://doc.rust-lang.org/std/macro.include.html> |
| Macro that emits modules | `macro_rules! m { () => { mod generated {} } }` | **NO** | *"Macros can expand to expressions, statements, items (including traits, impls, and foreign items), types, or patterns."* — <https://doc.rust-lang.org/reference/macros-by-example.html>; *"A macro invocation expands a macro at compile time and replaces the invocation with the result of the macro."* — <https://doc.rust-lang.org/reference/macros.html> |
| Compile-time item selection | `cfg_select! { unix => { fn foo() {} } … }` | **NO** | *"`cfg_select` expands to the payload of the first arm whose configuration predicate evaluates to true."* — <https://doc.rust-lang.org/reference/conditional-compilation.html> |

### 1.2 tree-sitter-rust node types (verified against grammar, then by parsing)

Source: <https://raw.githubusercontent.com/tree-sitter/tree-sitter-rust/master/src/node-types.json>,
commit `77a3747266f4d621d0757825e6b11edcbf991ca5`.

| node | named | fields (type unions, `?` = optional, `[]` = multiple) |
| --- | --- | --- |
| `use_declaration` | yes | `argument: crate \| identifier \| metavariable \| scoped_identifier \| scoped_use_list \| self \| super \| use_as_clause \| use_list \| use_wildcard` |
| `scoped_identifier` | yes | `path: bracketed_type \| crate \| generic_type \| identifier \| metavariable \| scoped_identifier \| self \| super?`, `name: identifier \| super` |
| `scoped_use_list` | yes | `path: …`, `list: use_list` |
| `use_list` | yes | — (children: identifiers / nested use trees) |
| `use_as_clause` | yes | `path: crate \| identifier \| metavariable \| scoped_identifier \| self \| super`, `alias: identifier` |
| `use_wildcard` | yes | — |
| `mod_item` | yes | `name: identifier`, `body: declaration_list?` |
| `extern_crate_declaration` | yes | `name: identifier`, `alias: identifier?` |
| `attribute_item` → `attribute` | yes | `value: _expression?`, `arguments: token_tree?` |
| `macro_invocation` | yes | `macro: identifier \| scoped_identifier` |
| `macro_definition` → `macro_rule` → `token_tree` | yes | `name: identifier` |

Three findings from actually parsing, which matter more than the schema:

1. **The grammar's own node types admit unresolvable targets.** `use_declaration.argument`
   may be a bare `metavariable`, and `scoped_identifier.path` may be a `bracketed_type` or
   `generic_type`. Verified: `use <Foo as Bar>::Baz;` parses as
   `use_declaration → scoped_identifier (path: bracketed_type → qualified_type) → name: identifier`.
   The first path segment is a **type**, not a module — a resolver that assumes segment[0] is a
   crate/module produces a confident wrong answer.
2. **`#[path = "…"]` is not an import node.** It lives in `attribute_item → attribute` with
   `value: string_literal` (verified: `attribute` → `identifier 'path'`, `=`, `value: string_literal
   → string_content 'other.rs'`), *sibling* to the `mod_item`. Recovering it means implementing
   Rust attribute semantics on the CST.
3. **A `mod` produced by a macro is not a `mod_item`.** Verified: inside
   `macro_rules! make_mod { () => { mod generated {} } }` the body parses as
   `macro_rule → token_tree` containing the bare tokens `mod`, `identifier 'generated'`,
   `token_tree '{}'`. There is no `mod_item` node to find. Same for `include!("…")`, which is a
   `macro_invocation` whose argument is a `token_tree`, not a specifier node.

### 1.3 Primary sources

- Modules: <https://doc.rust-lang.org/reference/items/modules.html>
- Paths / editions / `crate`/`super`/`self`/`::`: <https://doc.rust-lang.org/reference/paths.html>
- Conditional compilation (`cfg`, `cfg_attr`, `cfg_select!`): <https://doc.rust-lang.org/reference/conditional-compilation.html>
- `use` declarations: <https://doc.rust-lang.org/reference/items/use-declarations.html>
- Extern prelude: <https://doc.rust-lang.org/reference/names/preludes.html#extern-prelude>
- Macros: <https://doc.rust-lang.org/reference/macros.html>, <https://doc.rust-lang.org/reference/macros-by-example.html>
- `include!`: <https://doc.rust-lang.org/std/macro.include.html>
- Cargo dependency naming/renaming: <https://doc.rust-lang.org/cargo/reference/specifying-dependencies.html>
- Grammar: <https://github.com/tree-sitter/tree-sitter-rust>

---

## 2. TypeScript

### 2.1 Construct → can a syntax-only tool resolve it?

The decisive fact for TypeScript is that **resolution is configuration**, not syntax:

> "moduleResolution controls how TypeScript resolves module specifiers (string literals in
> import/export/require statements) to files on disk, and should be set to match the module
> resolver used by the target runtime or bundler."
> — <https://www.typescriptlang.org/docs/handbook/modules/reference.html#the-moduleresolution-compiler-option>

> "While the ECMAScript specification defines how to parse and interpret import and export
> statements, it leaves module resolution up to the host." / "There's no single right answer,
> so the compiler must be told the rules through configuration options."
> — <https://www.typescriptlang.org/docs/handbook/modules/theory.html#module-resolution-is-host-defined>

The modes that change the answer (`tsconfig` option `moduleResolution`): `classic`, `node10`
(alias `node`), `node16`, `nodenext`, `bundler` — <https://www.typescriptlang.org/tsconfig/#moduleResolution>.
The older `docs/handbook/module-resolution.html` page is now only a redirect stub to
`modules/theory.html#module-resolution`; use the latter.

| Construct | Exact syntax form | Verdict | Why / source |
| --- | --- | --- | --- |
| Relative with extension | `import x from "./a.js"` | **YES** literal / **YES+CFG** target | *"All of TypeScript's moduleResolution algorithms support referencing a module by a relative path that includes a file extension"*; target found by extension substitution `"./a.js" → a.ts / a.tsx / a.d.ts` — <https://www.typescriptlang.org/docs/handbook/modules/reference.html#relative-file-path-resolution> |
| Extensionless relative | `import x from "./a"` | **YES+CFG** | *"In some cases, the runtime or bundler allows omitting a .js file extension from a relative path."* — <https://www.typescriptlang.org/docs/handbook/modules/reference.html#extensionless-relative-paths>. Under `node16`/`nodenext` for an ESM-format file this is not allowed at all. |
| Directory / index | `import x from "./dir"` | **YES+CFG** | *"In some cases, a directory, rather than a file, can be referenced as a module."* — <https://www.typescriptlang.org/docs/handbook/modules/reference.html#directory-modules-index-file-resolution>; allowed under `node10`/`bundler`, an error for ESM under `node16`/`nodenext`. |
| `baseUrl` bare specifier | `import { helloWorld } from "hello/world"` | **YES+CFG** | *"Sets a base directory from which to resolve bare specifier module names."* / *"This resolution has higher priority than lookups from node_modules."* — <https://www.typescriptlang.org/tsconfig/#baseUrl> |
| `paths` alias | `import x from "@app/components/Button"` | **YES+CFG** | *"A series of entries which re-map imports to lookup locations relative to the baseUrl if set, or to the tsconfig file itself otherwise."* — <https://www.typescriptlang.org/tsconfig/#paths>. **This is the construct that sets the measured TS/JS ceiling.** |
| `node_modules` bare | `import x from "pkg"` | **YES+CFG** | *"All of TypeScript's moduleResolution options except classic support node_modules lookups."* — <https://www.typescriptlang.org/docs/handbook/modules/reference.html#the-moduleresolution-compiler-option> |
| Subpath imports | `import "#utils"` | **YES+CFG** | *"TypeScript will attempt to resolve import paths beginning with # through the `imports` field of the nearest ancestor package.json of the importing file."* — <https://www.typescriptlang.org/docs/handbook/modules/reference.html> |
| `package.json` `"exports"` | `import x from "pkg/sub"` | **YES+CFG** | *"When moduleResolution is set to node16, nodenext, or bundler, and resolvePackageJsonExports is not disabled, TypeScript follows Node.js's package.json `exports` spec"* — <https://www.typescriptlang.org/docs/handbook/modules/reference.html#packagejson-exports> |
| Type-only import | `import type { T } from "./t"` | **YES** literal, same resolution | *"Import declarations written with `import type` … are all guaranteed to be elided from the output JavaScript."* — <https://www.typescriptlang.org/docs/handbook/modules/reference.html#type-only-imports-and-exports>. **Runtime-invisible edge**: it is a genuine type-level dependency and a false runtime one. |
| Import-equals | `import y = require("./y")` | **YES+CFG** (require algorithm) | <https://www.typescriptlang.org/docs/handbook/modules/reference.html#export--and-import--require>. **Grammar trap**: the specifier is *not* on `import_statement.source` — it is on the `import_require_clause` child (verified by parsing). |
| Import-alias | `import x = A.B` | **NO** (no file target) | `<https://www.typescriptlang.org/docs/handbook/modules/reference.html>`; this is an alias to a qualified name, not a module. Grammar node: `import_alias`. |
| Re-export | `export * from "./d"`, `export { c } from "./c"`, `export * as ns from "./nsx"` | **YES** literal | <https://www.typescriptlang.org/docs/handbook/modules/reference.html#module-syntax>; spec grammar `ExportFromClause : * FromClause ; \| NamedExports FromClause ;` — <https://tc39.es/ecma262/#sec-exports>. **This is the barrel edge that corrupts fan-in** (see §7.3). |
| Ambient module | `declare module "ambient" { }` | **NO** | *"declaring a module that exists in the runtime but has no corresponding file"* — <https://www.typescriptlang.org/docs/handbook/modules/reference.html#ambient-modules>. Verified: parses as `ambient_declaration → module` with `name: string`. |
| Pattern ambient module | `declare module "*.html" { }` | **NO** | *"A pattern ambient module contains a single `*` wildcard character in its name, matching zero or more characters in import paths."* — same anchor |
| Triple-slash path | `/// <reference path="./ref.d.ts" />` | **YES+CFG**, but **invisible to an import-node pass** | *"It serves as a declaration of dependency between files."* / *"A triple-slash reference path is resolved relative to the containing file, if a relative path is used."* — <https://www.typescriptlang.org/docs/handbook/triple-slash-directives.html#-reference-path->. Verified: the whole line parses as a `comment` node. |
| Triple-slash types | `/// <reference types="node" />` | **YES+CFG** | *"The process of resolving these package names is similar to the process of resolving module names in an import statement."* — <https://www.typescriptlang.org/docs/handbook/triple-slash-directives.html#-reference-types-> |
| `.d.ts` file | `import x from "./a"` where only `a.d.ts` exists | **YES+CFG** | *"TypeScript always wants to resolve internally to a file that can provide type information"* — <https://www.typescriptlang.org/docs/handbook/modules/reference.html>; *"the compiler assumes that wherever it sees a declaration file, there is a corresponding JavaScript file that is perfectly described by the type information in the declaration file."* — <https://www.typescriptlang.org/docs/handbook/modules/theory.html#typescript-imitates-the-hosts-module-resolution-but-with-types> |
| Non-literal dynamic import | `` import(`./${x}`) ``, `import(someExpr)` | **NO** | `ImportCall : import ( AssignmentExpression , opt )` — the argument is an arbitrary expression — <https://tc39.es/ecma262/#sec-import-calls> |
| `import()` type | `type M = typeof import("./mod.js")` | **YES** literal | *"import() types are resolved according to the format of the importing file"* — <https://www.typescriptlang.org/docs/handbook/modules/reference.html> |

### 2.2 tree-sitter-typescript node types (verified against grammar, then by parsing)

Source: <https://raw.githubusercontent.com/tree-sitter/tree-sitter-typescript/master/typescript/src/node-types.json>
(183 named types; the `tsx/` grammar is identical for all module-related nodes and adds the JSX
set). Commit `75b3874edb2dc714fb1fd77a32013d0f8699989f`.

| node | named | fields / children |
| --- | --- | --- |
| `import_statement` | yes | `source: string` (**optional** — see trap 1); children `import_clause`, `import_attribute`, `import_require_clause` |
| `import_require_clause` | yes | `source: string` (required); child `identifier` — this is `import x = require("…")` |
| `import_clause` | yes | children `identifier`, `named_imports`, `namespace_import` |
| `named_imports` → `import_specifier` | yes | `name`, `alias` |
| `namespace_import` | yes | child `identifier` |
| `export_statement` | yes | `declaration?`, `decorator[]?`, `source: string?`, `value?`; children `export_clause`, `namespace_export`, `expression`, `identifier` |
| `namespace_export` | yes | children `identifier`, `string` — `export * as ns from "…"` |
| `import_alias` | yes | children `identifier`, `nested_identifier` — `import x = A.B` |
| `ambient_declaration` | yes | wraps `declare module "…"` |
| `module` | yes | `name: identifier \| nested_identifier \| string`, `body: statement_block` |
| `call_expression` | yes | `function: expression \| import`, `arguments`, `type_arguments?` |
| `require` | **no** | anonymous token, present in TS/TSX node types; only inside `import_require_clause` |
| `import` | yes + no | node types contain both a named and an anonymous `import` entry |

Three verified traps:

1. **`import_statement.source` is optional in TS but required in JS.** Verified: for
   `import y = require('./y');` the `import_statement` has **no** `source` field at all — the
   literal is on `import_require_clause.source`. A TS extractor written against the JS shape
   silently drops every `import x = require(...)`.
2. **`require` is a node type in TS/TSX but not in JavaScript.** Verified from node-types.json:
   TS has a `require` entry (`named: false`); JS has **no** `require` entry at all. But a plain
   `const a = require('./a');` in a `.ts` file is still a `call_expression` with
   `function: identifier "require"` — the `require` node only appears in the import-equals
   position. So `require`-scanning must be done on `identifier` text in general and on
   `import_require_clause` specifically for import-equals.
3. **`import type` has no dedicated node.** `import type { T } from "./t"` parses as an ordinary
   `import_statement`; the `type` marker is an anonymous token. Distinguishing type-only edges
   (elided at runtime) from value edges therefore requires reading anonymous children.

`import_statement.source` is a **named `string` node** (verified `is_named = true`); read it with
`child_by_field_name("source")`, then take its `string_fragment` (or strip quotes) — do not rely
on `named_children[0]`, which is `import_clause`.

### 2.3 Primary sources

- Module resolution reference: <https://www.typescriptlang.org/docs/handbook/modules/reference.html>
- Module theory (host-defined resolution): <https://www.typescriptlang.org/docs/handbook/modules/theory.html>
- tsconfig `moduleResolution`, `baseUrl`, `paths`: <https://www.typescriptlang.org/tsconfig/>
- Triple-slash directives: <https://www.typescriptlang.org/docs/handbook/triple-slash-directives.html>
- ECMAScript import/export grammar: <https://tc39.es/ecma262/#sec-imports>, <https://tc39.es/ecma262/#sec-exports>, <https://tc39.es/ecma262/#sec-import-calls>
- Grammar: <https://github.com/tree-sitter/tree-sitter-typescript>

---

## 3. JavaScript / Node.js

### 3.1 Construct → can a syntax-only tool resolve it?

JavaScript has **two different resolution algorithms in the same file extension space**, and the
CST does not tell you which one applies — that depends on the file's module format
(`package.json` `"type"`, `.mjs`/`.cjs` extension), i.e. project config:

> "In Node.js, the resolution algorithm for ECMAScript imports is significantly different from
> the algorithm for CommonJS require calls."
> — <https://www.typescriptlang.org/docs/handbook/modules/reference.html#node16-nodenext>

| Construct | Exact syntax form | Verdict | Why / source |
| --- | --- | --- | --- |
| CJS relative | `require('./path/myLocalModule')` | **YES** literal / **YES+CFG** target | `id <string> module name or path`; *"Local modules and JSON files can be imported using a relative path (e.g. `./`, `./foo`, `./bar/baz`, `../foo`) that will be resolved against the directory named by `__dirname`"* — <https://nodejs.org/api/modules.html#requireid> |
| CJS extension search | `require('./a')` → `a.js` / `a.json` / `a.node` | **YES+CFG** | CJS pseudocode `LOAD_AS_FILE(X)`: `1. If X is a file … 2. If X.js is a file … 3. If X.json is a file … 4. If X.node is a file` — <https://nodejs.org/api/modules.html#all-together> |
| CJS directory `main`/index | `require('./some-library')` | **YES+CFG** | `LOAD_AS_DIRECTORY(X)`: `1. If X/package.json is a file, a. Parse X/package.json, and look for "main" field.`; `LOAD_INDEX(X)`: `1. If X/index.js is a file`; *"If there is no package.json file present in the directory, or if the "main" entry is missing or cannot be resolved, then Node.js will attempt to load an index.js or index.node file out of that directory."* — same URL |
| CJS bare package | `require('pkg')`, `require('pkg/sub')` | **YES+CFG** | CJS pseudocode `LOAD_NODE_MODULES(X, START)`, `LOAD_PACKAGE_EXPORTS(SUBPATH, PACKAGE_DIR)` — same URL; *"The "exports" provides a modern alternative to "main" allowing multiple entry points to be defined, conditional entry resolution support between environments, and preventing any other entry points besides those defined in "exports"."* — <https://nodejs.org/api/packages.html#package-entry-points> |
| CJS subpath import | `require('#dep')` | **YES+CFG** | CJS pseudocode `LOAD_PACKAGE_IMPORTS(X, DIR)`; *"Entries in the "imports" field must always start with `#` to ensure they are disambiguated from external package specifiers."* — <https://nodejs.org/api/packages.html#subpath-imports> |
| CJS self-reference | `require('my-pkg/x')` from inside `my-pkg` | **YES+CFG** | CJS pseudocode `LOAD_PACKAGE_SELF(X, DIR)` — <https://nodejs.org/api/modules.html#all-together> |
| ESM relative with extension | `import x from './a.js'` | **YES** literal / **YES+CFG** existence | <https://nodejs.org/api/esm.html#mandatory-file-extensions> |
| ESM extensionless | `import x from './a'` | **NO at runtime** (must be resolved by a bundler/config) | *"A file extension must be provided when using the import keyword to resolve relative or absolute specifiers."* — <https://nodejs.org/api/esm.html#mandatory-file-extensions> |
| ESM directory index | `import x from './dir'` | **NO** | *"Directory indexes (e.g. './startup/index.js') must also be fully specified."* — same URL; the ESM resolver raises `Unsupported Directory Import` — <https://nodejs.org/api/esm.html#resolution-algorithm> |
| ESM bare | `import x from 'pkg'`, `'pkg/shuffle'` | **YES+CFG** | *"There are three types of specifiers: … Relative specifiers like './startup.js' … Bare specifiers like 'some-package' or 'some-package/shuffle' … Absolute specifiers like 'file:///opt/nodejs/config.js'"* — <https://nodejs.org/api/esm.html#terminology>; ESM resolver step 5 "specifier is now a bare specifier → `PACKAGE_RESOLVE`", with `defaultConditions is the conditional environment name array, ["node", "import"]` — <https://nodejs.org/api/esm.html#resolution-algorithm> |
| ESM subpath import | `import x from '#dep'` | **YES+CFG** | ESM resolver step 4 → `PACKAGE_IMPORTS_RESOLVE`; <https://nodejs.org/api/packages.html#subpath-imports> |
| URL specifiers | `import 'file:///opt/x.js'`, `'data:text/javascript,…'`, `'node:fs'` | **YES** recognisable, but no filesystem target | ESM resolver step 2 "If specifier is a valid URL"; *"ES modules are resolved and cached as URLs."* — <https://nodejs.org/api/esm.html#urls>, <https://nodejs.org/api/esm.html#node-imports> |
| Re-export | `export * from './a'`, `export { x } from './a'` | **YES** literal | *"Specifiers are also used in export `from` statements"* — <https://nodejs.org/api/esm.html#terminology>; grammar `ExportFromClause` — <https://tc39.es/ecma262/#sec-exports> |
| Dynamic `import(expr)` | `await import(path)`, `` import(`./x/${n}`) `` | **NO** | `ImportCall : import ( AssignmentExpression , opt )` — arbitrary expression — <https://tc39.es/ecma262/#sec-import-calls> |
| Computed `require` | `require('./a' + x)`, `` require(`./${n}`) `` | **NO** | `require` has no ECMAScript grammar at all; the only thing that is statically readable is a literal argument. `<https://nodejs.org/api/modules.html#requireid>` documents the *runtime* string. |
| `require.resolve` / `import.meta.resolve` | `require.resolve('pkg')` | **YES+CFG** | *"All features of the Node.js module resolution are supported."* — <https://nodejs.org/api/esm.html#importmetaresolvespecifier> |

**The host owns resolution — the language spec does not.** This is the sentence to cite when
someone asks why a syntax-only tool cannot do this:

> "The actual process performed is host-defined, but typically consists of performing whatever
> I/O operations are necessary to load the appropriate Module Record."
> — <https://tc39.es/ecma262/#sec-HostLoadImportedModule>
> (the old anchor `#sec-hostresolveimportedmodule` still redirects; the clause was renamed —
> `HostResolveImportedModule` no longer occurs in the current draft)

And the static form is literal-only by grammar: `ModuleSpecifier : StringLiteral` —
<https://tc39.es/ecma262/#sec-imports>.

### 3.2 tree-sitter-javascript node types (verified against grammar, then by parsing)

Source: <https://raw.githubusercontent.com/tree-sitter/tree-sitter-javascript/master/src/node-types.json>
(119 named types). Commit `58404d8cf191d69f2674a8fd507bd5776f46cb11`.

| node | named | fields / children |
| --- | --- | --- |
| `import_statement` | yes | `source: string` (**required** in JS); children `import_clause`, `import_attribute` |
| `import_clause` | yes | children `identifier`, `named_imports`, `namespace_import` |
| `named_imports` → `import_specifier` | yes | `name`, `alias` |
| `namespace_import` | yes | child `identifier` |
| `export_statement` | yes | `declaration?`, `decorator[]?`, `source: string?`, `value?`; children `export_clause`, `namespace_export` |
| `export_clause` → `export_specifier` | yes | `name`, `alias` |
| `namespace_export` | yes | `export * as ns from './x'` |
| `call_expression` | yes | `function: expression \| import`, `arguments`, `optional_chain?` |
| `string` | yes | children `string_fragment`, `escape_sequence`, `html_character_reference` |
| `template_string` | yes | children `string_fragment`, `escape_sequence`, `template_substitution` |
| `import` | yes + no | node types contain both entries; dynamic import is `call_expression.function = import` |
| `require` | **ABSENT** | `require` is **not a node type**; `require('./e')` is `call_expression` → `function: identifier 'require'` |

Verified parse results (this is exactly what an extractor must branch on):

- `const e = require('./e');` → `call_expression`, `function: identifier 'require'`, argument `string`.
- ``const f = require(`./${name}`);`` → `call_expression`, argument `template_string` **with a `template_substitution` child** — a template containing any substitution is not a static specifier.
- `const i = require('./' + name);` → `call_expression`, argument `binary_expression`.
- `import('./g');` → `call_expression`, `function: import`, argument `string`.
- `export { c } from './c';` and `export * from './d';` → both put the literal on `export_statement.source` (a `string` node with a `string_fragment` child).

Extraction rule: match `import_statement` and `export_statement` by field, and treat
`call_expression` as an import only when `function` is the `import` node or an `identifier`
whose text is exactly `require` **and** the single argument is a `string` — anything else is a
dynamic edge that must be recorded as "unresolved", not dropped.

### 3.3 Primary sources

- Node CJS module resolution (full pseudocode): <https://nodejs.org/api/modules.html#all-together>
- `require(id)`: <https://nodejs.org/api/modules.html#requireid>
- Node ESM: terminology <https://nodejs.org/api/esm.html#terminology>, mandatory extensions <https://nodejs.org/api/esm.html#mandatory-file-extensions>, resolution algorithm <https://nodejs.org/api/esm.html#resolution-algorithm>, dynamic import <https://nodejs.org/api/esm.html#import-expressions>
- package.json `"exports"` / `"imports"`: <https://nodejs.org/api/packages.html#package-entry-points>, <https://nodejs.org/api/packages.html#subpath-imports>
- ECMAScript grammar: <https://tc39.es/ecma262/#sec-imports>, <https://tc39.es/ecma262/#sec-import-calls>, host resolution <https://tc39.es/ecma262/#sec-HostLoadImportedModule>
- Grammar: <https://github.com/tree-sitter/tree-sitter-javascript>

---

## 4. Python

### 4.1 Construct → can a syntax-only tool resolve it?

Python's syntax always gives you the module *name*; **nothing in the syntax tells you which
directory that name lives in** — that is `sys.path`, which is runtime state:

> "The search operation of the import statement is defined as a call to the `__import__()`
> function." — <https://docs.python.org/3/reference/import.html>
> "`sys.path` contains a list of strings providing search locations for modules and packages."
> — <https://docs.python.org/3/reference/import.html>

| Construct | Exact syntax form | Verdict | Why / source |
| --- | --- | --- | --- |
| Absolute dotted | `import a.b.c` | **YES+CFG** | The dotted name maps to `a/b/c.py` or `a/b/c/__init__.py` **only relative to unknown roots** (`sys.path`). |
| Absolute + alias | `import a.b.c as x` | **YES+CFG** | *"If the module name is followed by `as`, then the name following `as` is bound directly to the imported module."* — <https://docs.python.org/3/reference/simple_stmts.html#the-import-statement>. The alias changes the local binding, not the target. |
| From-import | `from a.b import c` | **YES+CFG** (module `a.b`) / **NO** (is `c` a submodule?) | Official two-step semantics: *"check if the imported module has an attribute by that name"* / *"if not, attempt to import a submodule with that name and then check the imported module again for that attribute"* — same URL. `c` is a **submodule file or a runtime attribute**, and the CST cannot tell. |
| From-import + alias | `from a.b import c as d` | **YES+CFG** / **NO** | same as above |
| Relative, sibling | `from . import x` | **YES+CFG** | *"By using leading dots in the specified module or package after `from` you can specify how high to traverse up the current package hierarchy… One leading dot means the current package where the module making the import exists."* — same URL. Verified: parses as `relative_import` with an `import_prefix` child and **no** `dotted_name`. |
| Relative, module | `from .mod import x` | **YES+CFG** | *"Relative imports must always use `from <> import`; `import <>` is always absolute."* — <https://peps.python.org/pep-0328/> |
| Relative, parent | `from ..pkg.mod import y` | **YES+CFG** | *"Two or more leading dots give a relative import to the parent(s) of the current package, one level per dot after the first."* — <https://peps.python.org/pep-0328/> |
| Wildcard | `from x import *` | **YES+CFG** (module `x`) / **NO** (names) | *"If the list of identifiers is replaced by a star (`'*'`), all public names defined in the module are bound…"*; *"The public names defined by a module are determined by checking the module's namespace for a variable named `__all__`"* — <https://docs.python.org/3/reference/simple_stmts.html#the-import-statement>. Only legal at module level. |
| Future import | `from __future__ import annotations` | **NO** as a *project-file* edge | *"A future statement is a directive to the compiler… recognized and treated specially at compile time"* — same URL; the target is the stdlib `__future__`, not a repo file. |
| Dynamic | `importlib.import_module("pkg." + name)` | **NO** (unless the argument is a literal) | *"The `name` argument specifies what module to import in absolute or relative terms (e.g. either `pkg.mod` or `..mod`)."* — <https://docs.python.org/3/library/importlib.html#importlib.import_module>. And the language docs themselves frame it as the dynamic escape hatch: *"`importlib.import_module()` is provided to support applications that determine dynamically the modules to be loaded."* — <https://docs.python.org/3/reference/simple_stmts.html#the-import-statement> |
| Dynamic | `__import__(name, …)` | **NO** | *"This function is invoked by the import statement. It can be replaced…"* / *"imports the module `name`, potentially using the given globals and locals to determine how to interpret the name in a package context."* — <https://docs.python.org/3/library/functions.html#import__> |
| Type-only guard | `if TYPE_CHECKING:` + `from foo import Bar` | **YES+CFG**, must be tagged type-only | *"A special constant that is assumed to be `True` by static type checkers. It's `False` at runtime."*; *"This prevents the module from actually being imported at runtime"* — <https://docs.python.org/3/library/typing.html#typing.TYPE_CHECKING>. Verified: the import inside the block is a normal `import_from_statement`; only the enclosing `if_statement` tells you it is conditional. |
| Conditional | `try: import fast_json as json` / `except ImportError: import json` | **NO** for "which branch holds"; each branch is **YES+CFG** | Both imports are syntactically present; which one executes is platform/environment-dependent. `ModuleNotFoundError`: *"A subclass of `ImportError` which is raised by import when a module could not be located."* — <https://docs.python.org/3/library/exceptions.html>. A naive extractor emits **both** edges and therefore an edge to a module that does not exist here. |
| `sys.path` mutation | `sys.path.append(...)` | **NO** | *"A list of strings that specifies the search path for modules. Initialized from the environment variable `PYTHONPATH`, plus an installation-dependent default."* / *"A program is free to modify this list for its own purposes."* — <https://docs.python.org/3/library/sys.html#sys.path> |
| `site` / `.pth` | `*.pth` in `site-packages` | **NO** | *"its contents are additional items (one per line) to be added to `sys.path`."* — <https://docs.python.org/3/library/site.html> |
| Custom finders | `sys.meta_path`, `sys.path_hooks` | **NO** | *"The import machinery is extensible, so new finders can be added to extend the range and scope of module searching."* — <https://docs.python.org/3/reference/import.html> |
| Regular package | directory with `__init__.py` | **YES+CFG** (package → `__init__.py` edge is derivable) | *"A regular package is typically implemented as a directory containing an `__init__.py` file. When a regular package is imported, this `__init__.py` file is implicitly executed"* — <https://docs.python.org/3/reference/import.html#regular-packages> |
| Namespace package (PEP 420) | directory **without** `__init__.py` | **NO** for uniqueness | *"Namespace packages are a mechanism for splitting a single Python package across multiple directories on disk."* / *"Namespace packages cannot contain an `__init__.py`."* — <https://peps.python.org/pep-0420/>; *"A subdirectory inside a regular package that does not contain an `__init__.py` file is treated as an implicit namespace package"* — <https://docs.python.org/3/reference/import.html#namespace-packages>. One name may map to **N** directories, so the target is not unique. This is what breaks naive `__init__.py`-based indexers. |
| Entry-point mode | `python script.py` vs `python -m pkg.mod` | **NO** | *"`__main__.__spec__` is set to `None`… running directly from a source or bytecode file"* — <https://docs.python.org/3/reference/import.html#special-considerations-for-main>; PEP 328: *"Relative imports use a module's `__name__` attribute to determine that module's position in the package hierarchy. If the module's name does not contain any package information (e.g. it is set to `'__main__'`) then relative imports are resolved as if the module were a top level module"* — <https://peps.python.org/pep-0328/> |

Python 2's implicit relative imports are gone, which actually *helps* a syntax-only tool:
*"The only acceptable syntax for relative imports is `from .[ module ] import name`. All import
forms not starting with `.` are interpreted as absolute imports. (PEP 328)"* —
<https://docs.python.org/3/whatsnew/3.0.html>.

### 4.2 tree-sitter-python node types (verified against grammar, then by parsing)

Source: <https://raw.githubusercontent.com/tree-sitter/tree-sitter-python/master/src/node-types.json>.
Commit `26855eabccb19c6abf499fbc5b8dc7cc9ab8bc64`. Root node is `module`.

| node | named | fields / children |
| --- | --- | --- |
| `import_statement` | yes | `name: aliased_import \| dotted_name[]` (required, multiple) |
| `import_from_statement` | yes | `module_name: dotted_name \| relative_import` (required, single), `name: aliased_import \| dotted_name[]?`; child `wildcard_import?` |
| `future_import_statement` | yes | `name: aliased_import \| dotted_name` |
| `relative_import` | yes | children `import_prefix` (required), `dotted_name?` |
| `import_prefix` | yes | the leading-dot run (`repeat1('.')`), so the relative level is directly readable |
| `wildcard_import` | yes | `*` |
| `aliased_import` | yes | `name: dotted_name`, `alias: identifier` |
| `dotted_name` | yes | children `identifier` |
| `call` | yes | `function`, `arguments` |
| `attribute` | yes | `object`, `attribute` |

Findings from parsing:

1. **`from . import x` has a `module_name` but no dotted part.** Verified: `import_from_statement`
   → `<field module_name>` → `relative_import` → `import_prefix '.'`, with no `dotted_name`. An
   extractor that reads `module_name` as a dotted name gets an empty string here.
2. **`from x import *` has no `name` field at all** — the payload is a `wildcard_import` child.
   Verified. The module edge `x` is still recoverable.
3. **`importlib.import_module("pkg." + name)` is an ordinary `call`.** Verified shape:
   `call` → `<field function>` = `attribute` (`object: identifier 'importlib'`,
   `attribute: identifier 'import_module'`) → `<field arguments>` = `argument_list` containing a
   `binary_operator`. `__import__(name)` is `call` → `identifier '__import__'`. To get anything
   at all from these, the tool must special-case a callee *by name* and then still only succeeds
   when the argument is a lone `string`.
4. **Relative imports are only well-defined inside a package.** The grammar happily parses
   `from . import x` in a top-level script; whether it means anything depends on how the file is
   executed (§4.1, last row).

### 4.3 Primary sources

- Import system: <https://docs.python.org/3/reference/import.html>
- Import statement semantics: <https://docs.python.org/3/reference/simple_stmts.html#the-import-statement>
- PEP 328 (relative imports): <https://peps.python.org/pep-0328/>
- PEP 420 (namespace packages): <https://peps.python.org/pep-0420/>
- `importlib.import_module`: <https://docs.python.org/3/library/importlib.html#importlib.import_module>
- `__import__`: <https://docs.python.org/3/library/functions.html#import__>
- `TYPE_CHECKING`: <https://docs.python.org/3/library/typing.html#typing.TYPE_CHECKING>
- `sys.path`: <https://docs.python.org/3/library/sys.html#sys.path>; `site`/`.pth`: <https://docs.python.org/3/library/site.html>
- Python 2 → 3 import change: <https://docs.python.org/3/whatsnew/3.0.html>
- Grammar: <https://github.com/tree-sitter/tree-sitter-python>

---

## 5. Go

**Granularity caveat up front: Go's unit of dependency is the *package*, which is all `.go`
files in one directory — not the file.** An import path resolves to a package/directory, never
to a single file. Every per-file number for Go below is therefore an attribution choice, not a
language fact.

### 5.1 Construct → can a syntax-only tool resolve it? (one file at a time)

| Construct | Exact syntax form | Verdict | Why / source |
| --- | --- | --- | --- |
| Plain import | `import "lib/math"` | **NO** single-file; **YES+CFG** with `go.mod` | The path is an opaque literal: *"The import names an identifier (PackageName) to be used for access and an ImportPath that specifies the package to be imported."* / `ImportPath = string_lit .` / *"The interpretation of the ImportPath is implementation-dependent but it is typically a substring of the full file name of the compiled package and may be relative to a repository of installed packages."* — <https://go.dev/ref/spec#Import_declarations> |
| Grouped import | `import ( "os"; "fmt" )` | same as plain | syntactically `import_declaration → import_spec_list → import_spec*` |
| Named import | `import m "path/to/pkg"` | **NO** for the file; default name **YES+CFG** | *"The PackageName is used in qualified identifiers…"* / *"If the PackageName is omitted, it defaults to the identifier specified in the package clause of the imported package."* — same URL (reading the target package's `package` clause is a workspace scan, not type info) |
| Dot import | `import . "math"` | **NO** to a file | *"If an explicit period (`.`) appears instead of a name, all the package's exported identifiers… will be declared in the importing source file's file block and must be accessed without a qualifier."* — same URL |
| Blank import | `import _ "net/http/pprof"` | **NO** to a file (still a real edge) | *"To import a package solely for its side-effects (initialization), use the blank identifier as explicit package name."* — same URL |
| Intra-module import | `import "example.com/m/foo/bar"` with `module example.com/m` in `go.mod` | **YES+CFG** | *"A module path is the canonical name for a module, declared with the module directive in the module's go.mod file. A module's path is the prefix for package paths within the module."* — <https://go.dev/ref/mod#module-path> |
| `replace` | `replace golang.org/x/net => ./fork/net` | **YES+CFG** | *"A replace directive replaces the contents of a specific version of a module, or all versions of a module, with contents found elsewhere."* / *"replace directives only apply in the main module's go.mod file and are ignored in other modules."* — <https://go.dev/ref/mod#go-mod-file-replace> |
| Vendoring | `vendor/` + `vendor/modules.txt` | **YES+CFG** | *"When vendoring is enabled, build commands like go build and go test load packages from the vendor directory instead of accessing the network or the local module cache."* — <https://go.dev/ref/mod#vendoring> |
| `internal` packages | `.../internal/baz` | **YES+CFG** | *"Code in or below a directory named \"internal\" is importable only by code that shares the same import path above the internal directory."* — <https://pkg.go.dev/cmd/go#hdr-Internal_packages>. This is a **visibility filter on import-path strings**, not a file-resolution step. |
| Build constraint | `//go:build linux && amd64`, `// +build linux,amd64` | **NO** | *"A build constraint, also known as a build tag, is a condition under which a file should be included in the package. Build constraints are given by a line comment that begins `//go:build`."* — <https://pkg.go.dev/cmd/go#hdr-Build_constraints>. Verified: both lines parse as `comment` nodes, invisible to a node-type walk. |
| Filename convention | `foo_linux.go`, `foo_amd64.go`, `foo_linux_amd64.go` | **YES** as an implicit constraint (from the filename); **NO** to know whether the file is in a given build | *"If a file's name, after stripping the extension and a possible `_test` suffix, matches any of the following patterns: `*_GOOS`, `*_GOARCH`, `*_GOOS_GOARCH` … then the file is considered to have an implicit build constraint requiring those terms."* — same URL |
| Import cycles | any import graph | **YES+CFG** on the *package* graph (`internal`/`replace`/`vendor` shift targets); **NO** pure single-file | *"An import declaration declares a dependency relation between the importing and imported package. It is illegal for a package to import itself, directly or indirectly, or to directly import a package without referring to any of its exported identifiers."* — <https://go.dev/ref/spec#Import_declarations> |

### 5.2 tree-sitter-go node types (verified against grammar, then by parsing)

Source: <https://raw.githubusercontent.com/tree-sitter/tree-sitter-go/master/src/node-types.json>,
commit `2346a3ab1bb3857b48b29d779a1ef9799a248cd7`.

| node | named | fields / children |
| --- | --- | --- |
| `import_declaration` | yes | no fields; children `import_spec` \| `import_spec_list` |
| `import_spec` | yes | `name: blank_identifier \| dot \| package_identifier?`, `path: interpreted_string_literal \| raw_string_literal` |
| `import_spec_list` | yes | children `import_spec` |
| `package_clause` | yes | child `package_identifier` |
| `interpreted_string_literal` | yes | child `interpreted_string_literal_content` (the unquoted path) |
| `raw_string_literal` | yes | child `raw_string_literal_content` |

Findings from parsing:

1. **`import_spec.path` is always a string literal node** — the target is never a single file,
   only a package path string. The `.` / `_` / alias distinction is a `name` field, so a tool
   can classify the import form reliably; it just cannot resolve the string.
2. **`//go:build` and `// +build` are `comment` nodes** (verified). File-level build selection
   therefore needs a comment-scanning pass plus the filename convention, and the result is
   build-configuration-dependent.
3. **Grouped imports are one `import_declaration` per `import ( … )` block**, so "imports per
   file" must count `import_spec` nodes, not `import_declaration` nodes.

### 5.3 Primary sources

- Go spec, import declarations: <https://go.dev/ref/spec#Import_declarations>
- Go modules reference: <https://go.dev/ref/mod> (module path, `replace`, `vendor`, resolution)
- `go` command docs (build constraints, internal packages, `.go` file rules): <https://pkg.go.dev/cmd/go>
- Grammar: <https://github.com/tree-sitter/tree-sitter-go>

---

## 6. Java

### 6.1 Construct → can a syntax-only tool resolve it?

Java's import statement **does not contain a path** — it contains a *canonical type name*, and
the mapping name → file is done by the compiler against a source root and a classpath of
directories/JARs/JMODs. Two spec facts set the ceiling:

> "An import declaration makes classes, interfaces, or members available by their simple names
> only within the compilation unit that actually contains the import declaration."
> — JLS §7.5, <https://docs.oracle.com/javase/specs/jls/se21/html/jls-7.html#jls-7.5>

> "It is not a compile-time error to name either `java.lang` or the named package of the current
> compilation unit in a type-import-on-demand declaration. The type-import-on-demand declaration
> is ignored in such cases." — JLS §7.5.2

| Construct | Exact syntax form | Verdict | Why / source |
| --- | --- | --- | --- |
| Single-type import | `import a.b.C;` | **YES+CFG** | *"A single-type-import declaration imports a single class or interface by giving its canonical name"*; *"The `TypeName` must be the canonical name of a class or interface."* — JLS §7.5.1, <https://docs.oracle.com/javase/specs/jls/se21/html/jls-7.html#jls-7.5.1>. Name → `a/b/C.java` under a source root, **or** a `.class` inside a JAR (invisible to source scanning). |
| Type-import-on-demand | `import a.b.*;` | **NO** | *"A type-import-on-demand declaration allows all accessible classes and interfaces of a named package, class, or interface to be imported as needed."* — JLS §7.5.2. No type is named; which class is used is decided elsewhere in the file. |
| Wildcard + bare simple name | bare `Foo` with `import a.b.*;` | **NO** | *"If a type name consists of a single `Identifier`, then the identifier must occur in the scope of exactly one declaration of a class, interface, or type parameter with this name"* — JLS §6.5.5.1, <https://docs.oracle.com/javase/specs/jls/se21/html/jls-6.html#jls-6.5.5.1>. Resolving it needs a classpath-wide type index. |
| Single-static import | `import static a.b.C.x;` | container **YES+CFG** / member **NO** | *"A single-static-import declaration imports all accessible static members with a given simple name from a class or interface."*; *"The `Identifier` must name at least one static member of the named class or interface."* — JLS §7.5.3. **Verified grammar trap**: the final segment `x` is an `identifier`, indistinguishable by node type from a class name — a naive "last segment = class" resolver produces an edge to a file that cannot exist. |
| Static-import-on-demand | `import static a.b.C.*;` | container **YES+CFG** / members **NO** | *"A static-import-on-demand declaration allows all accessible static members of a named class or interface to be imported as needed."* — JLS §7.5.4 |
| **Implicit same-package** | bare `Point` in `package points;` — **no import statement at all** | **NO** single-file / **YES+CFG** with a source-tree package index | *"In the absence of an access modifier, a top level class or interface has package access: it is accessible only within ordinary compilation units of the package in which it is declared."* — JLS §7.6. The spec's own example: *"Because the classes `Point` and `PointColor` have all the class declarations in package `points`… as their scope, this program compiles correctly."* — JLS §7.6 Ex 7.6-2. **This is the single largest source of missed Java edges** and the dominant reason the measured Java resolution rate is only 53.5% (§7.6). |
| Implicit `java.lang` | bare `String`, `Object` | **NO** as a project file | *"Every compilation unit implicitly imports every public class or interface declared in the predefined package `java.lang`, as if the declaration `import java.lang.*;` appeared at the beginning of each compilation unit"* — JLS §7.3 |
| Same-file type | `class Foo {}` used in the same file | **YES** | JLS §7.3 `OrdinaryCompilationUnit: [ PackageDeclaration ] { ImportDeclaration } { TopLevelClassOrInterfaceDeclaration }` — the braces mean **multiple top-level types per compilation unit are legal**. |
| Self / shadowing | `import a.b.C;` + `class C {}` in the same file | **YES** (pure syntax) | JLS §7.5.1: *"If the class or interface imported by the single-type-import declaration is declared as a top level class or interface… in the compilation unit that contains the import declaration, then the import declaration is ignored."* |
| Subpackage import | `import java.util;` | **YES** (detectably illegal) | JLS §7.5.1 Ex 7.5.1-3: *"Note that an import declaration cannot import a subpackage, only a class or interface."* |
| Classpath / JAR target | `import com.lib.X;` where `X` is only in a JAR | **NO** | *"To compile a source file, javac needs to find the declaration of every class or interface that is used, extended, or implemented by the code in the source file."* / *"`--class-path path` … Specifies where to find user class files and annotation processors."* / *"Depending on the option, the file system locations may be directories, JAR files or JMOD files."* / *"The first occurrence of a particular file shadows (hides) any subsequent occurrences of like-named files."* — <https://docs.oracle.com/en/java/javase/21/docs/specs/man/javac.html> |
| JPMS directive | `requires java.logging;` in `module-info.java` | **NO** (module, not a file) | *"The `requires` directive specifies the name of a module on which the current module has a dependence."* — JLS §7.7.1, <https://docs.oracle.com/javase/specs/jls/se21/html/jls-7.html#jls-7.7.1>. This is a **module-level** edge set that must not be mixed into a file-level graph. |
| `module-info` filename | `module-info.java` | **YES** | JLS §7.7: *"the host system may choose to enforce the restriction that it is a compile-time error if a module declaration is not found in a file under a name composed of `module-info` plus an extension"* |
| Generated types | anything produced by `-processor` | **NO** | *"If any processors generate new source files, then another round of annotation processing occurs"* / *"the compiler compiles the original and all generated source files."* / *"`-s directory` Specifies the directory used to place the generated source files."* — <https://docs.oracle.com/en/java/javase/21/docs/specs/man/javac.html>; *"Annotation processing happens in a sequence of rounds."* — <https://docs.oracle.com/en/java/javase/21/docs/api/java.compiler/javax/annotation/processing/Processor.html> |
| Stale `.class` vs `.java` | same type present both as source and class | **NO** | *"If both a compiled class file and a source file are found, the most recently modified file will be used by default."* / *"If both are specified, javac looks for compiled class files on the class path and for source files on the source path."* — javac, same URL |

**Unit-of-dependency correction to the brief.** The canonical title of JLS §7.6 in SE 21 is
*"Top Level Class and Interface Declarations"*. More importantly, the one-public-class-per-file
rule is **host-system-conditional**: *"**If and only if** packages are stored in a file system
(§7.2), the host system **may choose to enforce** the restriction…"* and *"This restriction
implies that there must be at most one such class or interface per compilation unit."* —
<https://docs.oracle.com/javase/specs/jls/se21/html/jls-7.html#jls-7.6>. So a Java file may
legally declare several top-level types, and mapping a class to a file is a heuristic
(`Toad.java` for `wet.sprocket.Toad`, per the spec's own note) rather than a language rule.

### 6.2 tree-sitter-java node types (verified against grammar, then by parsing)

Source: <https://raw.githubusercontent.com/tree-sitter/tree-sitter-java/master/src/node-types.json>.
Commit `e10607b45ff745f5f876bfa3e94fbcc6b44bdc11`. Root node is `program`.

| node | named | fields / children |
| --- | --- | --- |
| `program` | yes | root; holds several `class_declaration` siblings |
| `package_declaration` | yes | children `annotation`, `marker_annotation`, `identifier`, `scoped_identifier` |
| `import_declaration` | yes | **no fields**; children `import` (anon), `static` (anon), `identifier` / `scoped_identifier`, `asterisk` |
| `scoped_identifier` | yes | `scope: identifier \| scoped_identifier`, `name: identifier` |
| `asterisk` | yes | the `*` of `import a.b.*;` |
| `static` | **no** | anonymous keyword token — the only marker distinguishing `import static …` |
| `identifier` | yes | a path segment |
| `class_declaration`, `interface_declaration`, `enum_declaration`, `record_declaration`, `annotation_type_declaration` | yes | `name: identifier`, `body: *_body`; these give a pure-syntax *type name → file* index |
| `module_declaration` | yes | `name`, `body: module_body` |
| `requires_modifier` | yes | `transitive \| static` (the `requires` keyword itself is anonymous) |

Findings from parsing:

1. **`import_declaration` has no fields at all** (verified). Extraction means walking children:
   a child `scoped_identifier` (or `identifier`) is the name, an `asterisk` child means
   on-demand, and an anonymous `static` child means a static import. There is no
   `import_on_demand` / `static_import` node to switch on.
2. **`import java.util.List;` parses as a nested `scoped_identifier`** chain —
   `scoped_identifier(scope: scoped_identifier(scope: identifier 'java', name: 'util'), name: 'List')`.
   Flattening by walking `scope` is required; the simple name is the outermost `name` field.
3. **`import static java.lang.Math.max;` looks identical to a type import** apart from the
   anonymous `static` token, and its final `name` is `max` — a **method**. Path-based resolution
   turns this into a phantom file.
4. **Multiple top-level classes per file parse fine** (verified: a file with `class A {}` and
   `class B {}` yields two `class_declaration` children of `program`). A file-level graph must
   decide how to attribute an edge addressed to `B` when `B.java` does not exist.

### 6.3 Primary sources

- JLS §7.3 Compilation Units: <https://docs.oracle.com/javase/specs/jls/se21/html/jls-7.html#jls-7.3>
- JLS §7.5 Import Declarations (+ §7.5.1 single-type, §7.5.2 on-demand, §7.5.3 single-static, §7.5.4 static-on-demand): <https://docs.oracle.com/javase/specs/jls/se21/html/jls-7.html#jls-7.5>
- JLS §7.6 Top Level Class and Interface Declarations: <https://docs.oracle.com/javase/specs/jls/se21/html/jls-7.html#jls-7.6>
- JLS §7.7 Module Declarations / §7.7.1 `requires`: <https://docs.oracle.com/javase/specs/jls/se21/html/jls-7.html#jls-7.7>
- JLS §6.5.5.1 simple type names: <https://docs.oracle.com/javase/specs/jls/se21/html/jls-6.html#jls-6.5.5.1>
- `javac` (class path, annotation processing, `-s`): <https://docs.oracle.com/en/java/javase/21/docs/specs/man/javac.html>
- Annotation processing API: <https://docs.oracle.com/en/java/javase/21/docs/api/java.compiler/javax/annotation/processing/Processor.html>
- Grammar: <https://github.com/tree-sitter/tree-sitter-java>

## 7. Synthesis

### 7.1 What *is* reliably computable at file granularity, syntax-only

Ranked by how much you can trust the output. "Reliably" below means *the tool never silently
claims a target it cannot see* — recall is a separate, per-language number.

| # | Output | Reliable? | Why / what it costs |
| --- | --- | --- | --- |
| 1 | **Out-edges with a literal relative specifier** (`import './a'`, `use crate::a::b`, `from .mod import x`, `import "./pkg"` that maps intra-module) | **Yes**, if you implement the language's file-naming rules | The literal is in the CST and the target is a deterministic function of (current file, literal, project layout). This is the only class where a single-file pass plus a filesystem probe suffices. |
| 2 | **Specifier inventory per file** ("this file references 7 external modules and 3 relative paths") | **Yes** | Pure CST extraction; no resolution. Useful on its own for a "does this file touch too many modules" signal. |
| 3 | **fan-out per file** | **Yes with config**; degrades only on the unresolved specifiers | Needs only *this* file's specifiers resolved. Errors are local: one bad specifier costs one out-edge. |
| 4 | **Import graph edges (resolved)** | **Yes with config, per-language recall 2.7%–53.5%** | Requires whole-repo scan + config. Recall is bounded by the "NO" rows in §1–§6, not by the algorithm. |
| 5 | **fan-in per file** | **Only as a lower bound** | Requires every importer to be parsed *and* to have resolved this file. Compounding: if resolution recall is *r*, fan-in recall is roughly *r* too, but ordering across files is distorted because which files are undercounted depends on which construct *they* use (e.g. only files importing through a `paths` alias are lost). |
| 6 | **Cycles / SCCs** | **Least reliable** | Needs a closed loop of correct edges. Sensitive to *both* error directions; see §7.3 for the concrete loops that vanish and the ones that appear. |
| 7 | **Layering violations** | **No — project config** | No language spec defines "layer". A tool can offer the *mechanism* (SCC + declared layer globs) but the policy is user input. |
| 8 | **Anything function-level** (which symbols, which calls, dynamic dispatch) | **No** | Requires a call graph; out of scope by construction (§7.4). |

One asymmetry deserves emphasis, because it decides the feature's design:

> **fan-out is cheap and honest; fan-in is expensive and biased.** fan-out needs one file
> resolved. fan-in needs the whole repo resolved *and* is dominated by the handful of files
> with the most importers — exactly the files most likely to be imported through an alias,
> a barrel/re-export, or a wildcard, i.e. the constructs that fail resolution.

A corollary that already shows up in the local data (§7.6): fan-in is **anti-correlated with
churn** (Spearman −0.227 vs. number of changes, −0.311 vs. age-normalised change rate) and has
**essentially zero correlation with complexity** (+0.025 vs. max cognitive in file). The
handful of "risky" files are the *intersection* of fan-in and change rate, not high fan-in.

### 7.2 What is not computable, and the exact form that breaks it

Grouped by *why* it breaks, since the fix differs per group.

**(a) The specifier is not a literal in the CST**

| language | exact form | why |
| --- | --- | --- |
| JS/TS | `require('./' + name)`, ``require(`${dir}/mod`)`` | the argument is a `binary_expression` / `template_string` with a `template_substitution`; the string is a runtime value |
| JS/TS | `import(path)` with any non-literal | `call_expression` with `function: import`, argument is an arbitrary expression |
| Python | `importlib.import_module("pkg." + name)`, `__import__(name)` | ordinary `call` with a runtime argument |
| Python | `from x import *` | the *module* `x` resolves; the *names* do not, so you cannot attribute the dependency further |
| Go | `import "lib/math"` | the path is a `string_lit` and the Go spec says its interpretation is implementation-dependent: *"The interpretation of the ImportPath is implementation-dependent but it is typically a substring of the full file name of the compiled package"* — <https://go.dev/ref/spec#Import_declarations> |

**(b) The target is chosen at compile time, not written in the file**

| language | exact form | why |
| --- | --- | --- |
| Rust | `#[cfg(feature = "x")] mod gated;` | *"Which configuration options are set is determined statically during the compilation of the crate."* — <https://doc.rust-lang.org/reference/conditional-compilation.html> |
| Rust | `#[cfg_attr(target_os = "linux", path = "linux.rs")] mod os;` | the Reference's own example: *"The following module will either be found at `linux.rs` or `windows.rs` based on the target."* — same URL |
| Rust | `macro_rules! m { () => { mod generated {} } }` | parsed as `macro_definition` → `macro_rule` → `token_tree`; the `mod` inside is a bare token, not a `mod_item`, and *"Macros can expand to … items (including traits, impls, and foreign items)…"* — <https://doc.rust-lang.org/reference/macros-by-example.html> |
| Rust | `include!("generated.rs")` | `macro_invocation` with `macro: identifier` + `token_tree`; *"The included file is located relative to the current file… interpreted in a platform-specific way at compile time."* — <https://doc.rust-lang.org/std/macro.include.html> |
| Go | `//go:build linux && amd64`, `_linux.go`, `_amd64.go` | parsed as a `comment` node (verified); membership depends on `GOOS`/`GOARCH`/tags. *"A build constraint, also known as a build tag, is a condition under which a file should be included in the package."* — <https://pkg.go.dev/cmd/go#hdr-Build_constraints> |
| Java | annotation processors | javac `-processor` may emit new source files during compilation — <https://docs.oracle.com/en/java/javase/21/docs/specs/man/javac.html> |

**(c) The target lives in project config the file does not contain**

`go.mod` `module`/`replace`/`vendor/`; `Cargo.toml` `[dependencies]` keys and `package = "…"`
renames; `tsconfig.json` `paths`/`baseUrl`; `package.json` `"exports"`/`"imports"`; the Java
classpath (JARs); Python `sys.path` / `.pth` / `site-packages`. All are readable *as data* — so
these are **YES+CFG**, not NO — but a one-file-at-a-time tool sees none of them, which is why
the verdict column separates the two.

**(d) The unit of dependency is not a file**

| language | issue |
| --- | --- |
| Go | the unit is the **package = directory** (all `.go` files). `import "fmt"` resolves to a directory; per-file resolution is a category error. |
| Java | the unit is the **class**. JLS §7.6 allows several top-level types per compilation unit, only one of which may be `public` and must match the filename — so `import a.b.C;` is a class→file lookup, not a path lookup. |
| TS/JS | re-export barrels (`export * from './x'`) make the *imported* file a re-export hub, so fan-in accrues to the barrel rather than to the module actually used. |
| Rust | `mod` is a *module* unit and `mod.rs`/`foo.rs` are two spellings of the same module; both map to one logical node. |

### 7.3 False edges and missed edges, concretely

**Missed edges (cycle destroyed, fan-in undercounted).** Each of these is invisible to a
node-type-driven walk because there is no import node to find:

- **Java same-package references.** A class in `com.example` using another class in
  `com.example` writes **no import at all** — the import statement is simply absent, so the
  edge does not exist in the CST. This is the dominant cause of the measured 53.5% Java
  ceiling. JLS §7.5: a type in the same package is visible without import —
  <https://docs.oracle.com/javase/specs/jls/se21/html/jls-7.html#jls-7.5>.
- **Rust `#[cfg]`-gated `mod`/`use`, macro-generated `mod`, `include!`** (§7.2b).
- **Go files excluded by build constraints.** The import statements are still *text*, but the
  file is not in the package for a given `GOOS` — and conversely an edge *to* a package whose
  only implementing file is excluded is missed.
- **TS `/// <reference path="./ref.d.ts" />`** — verified to parse as a plain `comment` node.
  A comment-walking pass can recover it; a node-type pass cannot.
- **TS `paths` aliases** (`import x from '@/lib/x'`) and **`package.json` `imports`** (`#foo`).
- **Python `TYPE_CHECKING` / `try: import … except ImportError:`** — the statement *is* present,
  so it is not missed syntactically, but it is conditional. The spec is explicit that
  `TYPE_CHECKING` is false at runtime, so for *runtime* coupling these are false edges and for
  *type* coupling they are the only edges. You must pick one semantic and document it.

**False / spurious edges (fake cycles invented, fan-in inflated).**

- **Java `import a.b.*;`** — the CST gives `scoped_identifier` + `asterisk`, i.e. the *package*.
  A tool that emits an edge to the package (or to every class in it) invents edges for classes
  the file never mentions. Which class is actually used requires resolving `type_identifier`
  occurrences inside the file.
- **Java `import static a.b.C.member;`** — the last segment is a **member, not a type**
  (`scoped_identifier` with `name: identifier`; verified). Treating the final segment as a class
  name produces an edge to a file that cannot exist. Note the grammar gives you no flag
  distinguishing this from `import a.b.C` except the anonymous `static` token.
- **Java/Python/Rust glob imports** (`import a.b.*`, `from x import *`, `use foo::*;`) — the
  module edge is real, but expanding the glob to "depends on everything in x" is a false
  attribution.
- **Python `from x import name`** — the official semantics are explicitly two-step: *"check if
  the imported module has an attribute by that name; if not, attempt to import a submodule with
  that name and then check the imported module again for that attribute"* —
  <https://docs.python.org/3/reference/simple_stmts.html#the-import-statement>. So a resolver
  that maps the last segment to `x/name.py` is wrong whenever `name` is a class/function in
  `x/__init__.py`; a resolver that never does is wrong whenever `name` *is* a submodule.
- **Go `import _ "net/http/pprof"`** — a genuine edge (side-effect init) that "only imported
  names are used" heuristics drop, and `import . "math"` injects names invisibly.
- **Platform-conditional `try/except ImportError`** — both branches are in the text, so both
  edges appear even though only one can execute on a given platform.

**Cycle-specific consequence.** A false cycle needs only *one* false edge to close a loop that
does not exist (e.g. a Java wildcard edge plus a same-package edge); a missed cycle needs only
*one* missed edge in the loop (e.g. a Rust `#[cfg]`-gated `mod` in the middle of an otherwise
clean chain). Both are single-point failures, which is why SCC output should be presented with
the resolution rate attached, not as a defect list.

### 7.4 What needs a call graph, and is therefore out of scope

Everything below is invisible to a CST and must not be implied by a file-level feature:

- **Dynamic dispatch / virtual methods.** Java `obj.f()`, Rust `dyn Trait`, Go interface values,
  Python `self.m()`, JS `obj.m()` — which *implementation* runs is a runtime property. A file
  graph can say "A imports B"; it can never say "A calls B.f".
- **Reflection.** Java `Class.forName("a.b.C")`, `Method.invoke`; Python `getattr`; JS
  `obj[prop]()`.
- **Dependency-injection wiring.** Spring `@Autowired`, Guice modules, Dagger — the *code*
  dependency is on the interface; the concrete binding is configuration.
- **Python `importlib.import_module` / `__import__`** — the docs frame this as the runtime
  path: *"`importlib.import_module()` is provided to support applications that determine
  dynamically the modules to be loaded."* —
  <https://docs.python.org/3/library/functions.html#import__> and
  <https://docs.python.org/3/library/importlib.html#importlib.import_module>.
- **JS computed `require`/`import`** (§7.2a).
- **Rust trait objects and macros.** Trait-object dispatch has no static callee; macro expansion
  happens at compile time and can synthesise items, modules *and* whole impls.
- **Go interface satisfaction.** Go has no `implements`; whether a type satisfies an interface
  is decided structurally by the compiler, so no syntactic pass can build the
  "which types implement this interface" edge set.

### 7.5 What the file-level graph still sees that a per-file metric cannot

This is the positive case for adding it, independent of call-graph absence:

1. **Dependents (fan-in).** A per-file metric (`nloc`, max cognitive complexity) is a property
   of one file. "412 files could break if this one changes" is not.
2. **Cycles / SCCs.** A property of a *set* of files; no per-file metric can express it.
3. **Dependency direction and stability.** The fan-in × fan-out pair expresses the Stable
   Dependencies Principle: high fan-in + low fan-out = stable, low fan-in + high fan-out =
   volatile. Neither number alone means anything.
4. **Re-export hubs / barrels.** High fan-in concentrated in files with near-zero own
   complexity (the measured corpus: `ComponentTypeId.java` with 188 importers, 1 commit ever,
   max cognitive 4) — a structural smell invisible to any complexity threshold.
5. **Cross-file breadth per file (fan-out).** A weak but real proxy for "this file knows too
   much"; unlike complexity it is not a function of the file's own text *length*.
6. **Risk intersection.** The measured, sharp set is `fan-in ≥ 3 ∧ change rate in top decile`
   (18 of 1014 files), not either axis alone.

### 7.6 Empirical anchor from this repo

The companion measurement (`coupling/local-measurements.md`, `coupling/fanin.py`) already ran an
approximate version of exactly this analysis over 33 local repos, and its per-language
resolution ceiling is the number to beat:

| Language | files where ≥1 importer was found | dominant cause of loss |
| --- | --- | --- |
| Java | 53.5% | same-package references need no import and are invisible |
| Python | 41.3% | `importlib` / dynamic imports; namespace packages |
| TypeScript/JS | 23.3% | bare specifiers and `tsconfig` `paths` aliases (`@/…`) skipped |
| Rust | **2.7%** | `crate::`/`super::` resolution too naive — **unusable** |
| Go | (not split out) | suffix matching against in-repo package dirs |

Correlations from the same pass (3 repos with deep history, age-normalised change rate):

| pair | Spearman |
| --- | --- |
| fan-in ↔ number of changes | −0.227 |
| fan-in ↔ change rate (per month) | −0.311 |
| fan-in ↔ max cognitive in file | **+0.025** |
| fan-in ↔ file nloc | −0.250 |

**What this means for the feature decision:** the graph is worth building *for the structural
facts* (dependents, SCCs, direction, barrel hubs) but the resolution ceiling must be surfaced in
the output, per language, or every downstream number is a silent undercount. Rust at 2.7% is not
"a lower bound worth reporting" — it is a bug, and the Rust section above names the four
constructs (`#[cfg]`, macro-generated `mod`, `include!`, `#[path]`) that a real resolver would
have to handle before Rust numbers may be quoted at all.
