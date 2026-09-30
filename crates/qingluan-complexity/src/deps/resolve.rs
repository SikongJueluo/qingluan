//! Specifier resolution: turn one [`ImportSpec`] into repo files, an external
//! package, or an honest "cannot know".
//!
//! The three-way outcome is the load-bearing part of the report
//! (`docs/research/coupling-as-complexity.md` §6): `resolved` builds the
//! graph, `external` says "outside the repo, not our edge to draw", and
//! `unresolved` is a *visible* miss — a specifier that looks like it should
//! land in the repo but did not. An unresolvable specifier never contributes
//! an edge, because a fabricated edge can close a cycle that does not exist.
//!
//! Rust gets the full treatment (module tree from `mod` declarations, crate
//! roots from manifests); the file-naming rules implemented here were
//! verified against `rustc` (see the `#[path]` comments).

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

use super::extract::{FileExtract, RustModDecl};

/// What one specifier resolved to. Go can resolve to a whole package
/// directory, hence a `Vec`.
pub(crate) enum Resolution {
    Resolved(Vec<PathBuf>),
    External,
    Unresolved,
}

// ---------------------------------------------------------------- Rust ----

/// One crate's module tree: a module is a file plus named child modules.
pub(crate) struct RustModule {
    pub file: PathBuf,
    pub children: BTreeMap<String, RustModule>,
}

/// A crate: one import target plus one module tree per entry file. `lib.rs`,
/// each binary, each integration test — they are separate roots that all
/// spell `crate::` from their own top.
struct Crate {
    import_target: PathBuf,
    trees: Vec<RustModule>,
}

/// Where a `.rs` file sits in which tree.
struct FileModule {
    crate_idx: usize,
    tree_idx: usize,
    /// Module path from the tree root, the root itself being `[]`.
    path: Vec<String>,
}

/// Everything needed to resolve a Rust `use` path.
pub(crate) struct RustResolver {
    crates: Vec<Crate>,
    /// `use`-path crate name (hyphens mapped to underscores) → crate index.
    crate_names: HashMap<String, usize>,
    file_modules: HashMap<PathBuf, FileModule>,
}

impl RustResolver {
    /// Build every crate tree found in the scan set.
    ///
    /// `manifests` are the `Cargo.toml` paths the walk saw; `extracts` maps
    /// every scanned `.rs` file to what its CST yielded; `nodes` is the
    /// canonicalized scan set (a module whose file was skipped is not
    /// followed — that branch dies, an honest miss rather than a wrong edge).
    pub(crate) fn build(
        manifests: &[PathBuf],
        extracts: &HashMap<PathBuf, FileExtract>,
        nodes: &HashSet<PathBuf>,
    ) -> Self {
        let mut crates: Vec<Crate> = Vec::new();
        let mut crate_names: HashMap<String, usize> = HashMap::new();
        let mut file_modules: HashMap<PathBuf, FileModule> = HashMap::new();

        for manifest in manifests {
            let package_dir = manifest.parent().unwrap_or(Path::new("."));
            let parsed = parse_cargo_toml(&std::fs::read_to_string(manifest).unwrap_or_default());
            let roots = crate_root_files(&parsed, package_dir, nodes);
            if roots.is_empty() {
                continue;
            }
            let crate_idx = crates.len();
            // The target of a cross-crate `use name::…` is the lib root when
            // the crate has one, else its first entry file.
            let import_target = parsed
                .lib_path
                .as_ref()
                .and_then(|rel| canonical_in_nodes(&package_dir.join(rel), nodes))
                .unwrap_or_else(|| roots[0].clone());
            if let Some(name) = parsed.use_name() {
                crate_names.entry(name).or_insert(crate_idx);
            }
            // One tree per entry file; every tree of a crate shares the
            // file→module map but gets its own visited set (a file reachable
            // from two roots is only claimed by the first).
            let mut trees = Vec::new();
            for root_file in &roots {
                let mut builder = TreeBuilder {
                    crate_idx,
                    extracts,
                    nodes,
                    visited: HashSet::new(),
                    file_modules: &mut file_modules,
                };
                if let Some(tree) = builder.build(root_file, Vec::new(), trees.len()) {
                    trees.push(tree);
                }
            }
            crates.push(Crate {
                import_target,
                trees,
            });
        }
        RustResolver {
            crates,
            crate_names,
            file_modules,
        }
    }

    /// Resolve one `use` path from `file`.
    pub(crate) fn resolve_use(&self, file: &Path, segments: &[String]) -> Resolution {
        let Some(fm) = self.file_modules.get(file) else {
            // An orphan `.rs` file (no crate root claimed it): bare names can
            // still name a workspace crate; keywords have no anchor.
            return match segments[0].as_str() {
                "crate" | "self" | "super" => Resolution::Unresolved,
                first => match self.crate_names.get(first) {
                    Some(idx) => {
                        Resolution::Resolved(vec![self.crates[*idx].import_target.clone()])
                    }
                    None => Resolution::External,
                },
            };
        };
        match segments[0].as_str() {
            "crate" => self.walk(fm, &[], &segments[1..]),
            "self" => self.walk(fm, &fm.path, &segments[1..]),
            "super" => {
                // `super::super::x` pops one level per `super`, the first one
                // included.
                let supers = 1 + segments[1..]
                    .iter()
                    .take_while(|s| s.as_str() == "super")
                    .count();
                if fm.path.len() < supers {
                    return Resolution::Unresolved;
                }
                let start = &fm.path[..fm.path.len() - supers];
                self.walk(fm, start, &segments[supers..])
            }
            first => {
                // A workspace crate name wins over a same-named root module:
                // the compiler treats the two as an ambiguity error, and no
                // real repo has both. Referring to the *current* crate by
                // name is legal and means `crate::`, so it walks the tree.
                if let Some(&idx) = self.crate_names.get(first) {
                    let own = self.file_modules.get(file).map(|fm| fm.crate_idx);
                    if Some(idx) != own {
                        return Resolution::Resolved(vec![self.crates[idx].import_target.clone()]);
                    }
                    return self.walk(fm, &[], &segments[1..]);
                }
                // 2018 uniform paths: a bare first segment may resolve from
                // the crate root or the current module — try the root first.
                if self.is_module(fm, &[], &[first]) {
                    return self.walk(fm, &[first.to_string()], &segments[1..]);
                }
                if self.is_module(fm, &fm.path, &[first]) {
                    let mut start = fm.path.clone();
                    start.push(first.to_string());
                    return self.walk(fm, &start, &segments[1..]);
                }
                Resolution::External
            }
        }
    }

    /// Walk `rest` from the module at `path` in `fm`'s tree, returning the
    /// deepest module file reached. The first non-module segment is an item
    /// (fn, struct, …) living in the last module file — that file is the
    /// dependency edge.
    fn walk(&self, fm: &FileModule, path: &[String], rest: &[String]) -> Resolution {
        let mut node = match self.module_at(fm, path) {
            Some(node) => node,
            None => return Resolution::Unresolved,
        };
        let mut file = node.file.clone();
        for segment in rest {
            match node.children.get(segment) {
                Some(child) => {
                    node = child;
                    file = node.file.clone();
                }
                None => break,
            }
        }
        Resolution::Resolved(vec![file])
    }

    /// Whether walking `segments` from `path` stays on module nodes.
    fn is_module(&self, fm: &FileModule, path: &[String], segments: &[&str]) -> bool {
        let Some(mut node) = self.module_at(fm, path) else {
            return false;
        };
        for segment in segments {
            match node.children.get(*segment) {
                Some(child) => node = child,
                None => return false,
            }
        }
        true
    }

    fn module_at(&self, fm: &FileModule, path: &[String]) -> Option<&RustModule> {
        let mut node = &self.crates[fm.crate_idx].trees[fm.tree_idx];
        for segment in path {
            node = node.children.get(segment)?;
        }
        Some(node)
    }
}

/// Recursive module-tree construction for one crate.
struct TreeBuilder<'a> {
    crate_idx: usize,
    extracts: &'a HashMap<PathBuf, FileExtract>,
    nodes: &'a HashSet<PathBuf>,
    visited: HashSet<PathBuf>,
    file_modules: &'a mut HashMap<PathBuf, FileModule>,
}

impl TreeBuilder<'_> {
    /// Build one entry file's module tree, recording every file's module
    /// path exactly once (the first visit wins — a file reachable twice via
    /// `#[path]` aliases keeps its first module identity).
    fn build(&mut self, file: &Path, path: Vec<String>, tree_idx: usize) -> Option<RustModule> {
        if !self.visited.insert(file.to_path_buf()) {
            return None;
        }
        self.file_modules.insert(
            file.to_path_buf(),
            FileModule {
                crate_idx: self.crate_idx,
                tree_idx,
                path: path.clone(),
            },
        );
        let mut children = BTreeMap::new();
        let Some(extract) = self.extracts.get(file) else {
            return Some(RustModule {
                file: file.to_path_buf(),
                children,
            });
        };
        for decl in &extract.rust_mods {
            let Some(target) = mod_target_file(file, decl, self.nodes) else {
                continue;
            };
            if self.visited.contains(&target) {
                continue;
            }
            let mut child_path = path.clone();
            child_path.push(decl.name.clone());
            if let Some(child) = self.build(&target, child_path, tree_idx) {
                children.insert(decl.name.clone(), child);
            }
        }
        Some(RustModule {
            file: file.to_path_buf(),
            children,
        })
    }
}

/// Where one outlined `mod name;` declaration's file is.
///
/// Rules (verified against `rustc`, issue 08):
/// * plain `mod n;` in a `mod.rs`-like file (`mod.rs`, `lib.rs`, `main.rs`,
///   `build.rs`): `dir/n.rs` or `dir/n/mod.rs`;
/// * plain `mod n;` in any other file `a/b.rs`: `a/b/n.rs` or `a/b/n/mod.rs`;
/// * `#[path = "p"]` at the top level of file `F`: relative to **F's own
///   directory** (not the child directory);
/// * anything inside inline `mod` blocks: relative to the inline module's
///   virtual directory `child_dir/inline…/`;
/// * `#[cfg]`-gated mods are followed like plain ones when the file exists —
///   an over-approximation of the compiled graph, documented as such.
fn mod_target_file(file: &Path, decl: &RustModDecl, nodes: &HashSet<PathBuf>) -> Option<PathBuf> {
    let parent = file.parent()?;
    let child_dir = if is_modrs_like(file) {
        parent.to_path_buf()
    } else {
        parent.join(file.file_stem()?.to_string_lossy().as_ref())
    };
    let candidates: Vec<PathBuf> = if decl.inline.is_empty() {
        match &decl.path_attr {
            Some(p) => vec![parent.join(p)],
            None => vec![
                child_dir.join(format!("{}.rs", decl.name)),
                child_dir.join(&decl.name).join("mod.rs"),
            ],
        }
    } else {
        let mut dir = child_dir;
        for name in &decl.inline {
            dir = dir.join(name);
        }
        match &decl.path_attr {
            Some(p) => vec![dir.join(p)],
            None => vec![
                dir.join(format!("{}.rs", decl.name)),
                dir.join(&decl.name).join("mod.rs"),
            ],
        }
    };
    candidates
        .into_iter()
        .find(|candidate| nodes.contains(candidate))
}

fn is_modrs_like(file: &Path) -> bool {
    matches!(
        file.file_name().and_then(|n| n.to_str()),
        Some("mod.rs") | Some("lib.rs") | Some("main.rs") | Some("build.rs")
    )
}

/// Facts a `Cargo.toml` contributes. Hand-parsed: the manifest grammar we
/// care about is flat `key = "value"` pairs under section headers, and a TOML
/// dependency would buy nothing for ten lines of state machine.
struct CargoManifest {
    package_name: Option<String>,
    lib_path: Option<String>,
    lib_name: Option<String>,
    /// Explicit `path` entries of `[[bin]]`/`[[test]]`/`[[example]]`/`[[bench]]`.
    target_paths: Vec<String>,
    saw_bins: bool,
}

impl CargoManifest {
    /// The name a `use` path would address this crate by.
    fn use_name(&self) -> Option<String> {
        let raw = self.lib_name.clone().or(self.package_name.clone())?;
        Some(raw.replace('-', "_"))
    }
}

fn parse_cargo_toml(text: &str) -> CargoManifest {
    let mut manifest = CargoManifest {
        package_name: None,
        lib_path: None,
        lib_name: None,
        target_paths: Vec::new(),
        saw_bins: false,
    };
    let mut section = String::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line.starts_with('[') {
            section = line.trim_matches(|c| c == '[' || c == ']').to_string();
            if section == "bin" {
                manifest.saw_bins = true;
            }
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let value = value.trim().trim_matches('"');
        match (section.as_str(), key.trim()) {
            ("package", "name") => manifest.package_name = Some(value.to_string()),
            ("lib", "path") => manifest.lib_path = Some(value.to_string()),
            ("lib", "name") => manifest.lib_name = Some(value.to_string()),
            ("bin" | "test" | "example" | "bench", "path") => {
                manifest.target_paths.push(value.to_string())
            }
            _ => {}
        }
    }
    manifest
}

/// Every crate entry file present in the scan set.
fn crate_root_files(
    manifest: &CargoManifest,
    package_dir: &Path,
    nodes: &HashSet<PathBuf>,
) -> Vec<PathBuf> {
    let mut roots: Vec<PathBuf> = Vec::new();
    let push = |rel: &str, roots: &mut Vec<PathBuf>| {
        if let Some(path) = canonical_in_nodes(&package_dir.join(rel), nodes)
            && !roots.contains(&path)
        {
            roots.push(path);
        }
    };
    push("src/lib.rs", &mut roots);
    // Cargo's implicit src/main.rs only applies when no [[bin]] is declared.
    if !manifest.saw_bins {
        push("src/main.rs", &mut roots);
    }
    for rel in &manifest.target_paths {
        push(rel, &mut roots);
    }
    // Integration tests / examples / benches: every top-level .rs file is its
    // own crate root.
    for dir in ["tests", "examples", "benches"] {
        if let Ok(entries) = std::fs::read_dir(package_dir.join(dir)) {
            let mut files: Vec<PathBuf> = entries
                .flatten()
                .map(|entry| entry.path())
                .filter(|path| path.extension().is_some_and(|e| e == "rs"))
                .collect();
            files.sort();
            for file in files {
                if let Some(path) = canonical_in_nodes(&file, nodes)
                    && !roots.contains(&path)
                {
                    roots.push(path);
                }
            }
        }
    }
    push("build.rs", &mut roots);
    roots
}

// ---------------------------------------------------------------- Java ----

/// Package and type index over the scanned `.java` files.
pub(crate) struct JavaIndex {
    packages: HashSet<String>,
    types: HashMap<String, PathBuf>,
}

impl JavaIndex {
    pub(crate) fn build<'a>(files: impl Iterator<Item = (&'a PathBuf, &'a FileExtract)>) -> Self {
        let mut packages = HashSet::new();
        let mut types = HashMap::new();
        for (file, extract) in files {
            let package = extract.java_package.clone().unwrap_or_default();
            packages.insert(package.clone());
            for type_name in &extract.java_types {
                types
                    .entry(if package.is_empty() {
                        type_name.clone()
                    } else {
                        format!("{package}.{type_name}")
                    })
                    .or_insert_with(|| file.clone());
            }
        }
        JavaIndex { packages, types }
    }

    pub(crate) fn resolve(&self, fqn: &[String], wildcard: bool, is_static: bool) -> Resolution {
        let joined = fqn.join(".");
        // On-demand type imports name a package, not a type: fanning the edge
        // out to every file in the package would invent dependencies on types
        // the importer never mentions (research §7.3), so they stay
        // unresolved instead.
        if wildcard && !is_static {
            return if self.packages.contains(&joined) {
                Resolution::Unresolved
            } else {
                Resolution::External
            };
        }
        if !is_static
            && !wildcard
            && let Some(file) = self.types.get(&joined)
        {
            return Resolution::Resolved(vec![file.clone()]);
        }
        // A static import's last segment is a *member*; the container is the
        // type that resolves (verified trap, research §6.2 finding 3).
        let container: &[String] = if is_static && !wildcard {
            &fqn[..fqn.len() - 1]
        } else {
            fqn
        };
        if let Some(file) = self.types.get(&container.join(".")) {
            return Resolution::Resolved(vec![file.clone()]);
        }
        // Known package but unknown type: our miss. Unknown package: outside.
        let mut prefix = String::new();
        for (i, segment) in container.iter().enumerate() {
            if i > 0 {
                prefix.push('.');
            }
            prefix.push_str(segment);
            if self.packages.contains(&prefix) {
                return Resolution::Unresolved;
            }
        }
        Resolution::External
    }
}

// ------------------------------------------------------------------ Go ----

/// `go.mod` module paths found by the walk, plus the scanned files per
/// package directory (Go's dependency unit is the package = directory).
pub(crate) struct GoIndex {
    modules: Vec<(String, PathBuf)>,
    package_files: HashMap<PathBuf, Vec<PathBuf>>,
}

impl GoIndex {
    pub(crate) fn build(go_mods: &[PathBuf], go_files: &[PathBuf]) -> Self {
        let mut modules = Vec::new();
        for go_mod in go_mods {
            let text = std::fs::read_to_string(go_mod).unwrap_or_default();
            let module = text
                .lines()
                .find_map(|line| line.trim().strip_prefix("module "))
                .map(|m| m.trim().trim_matches('"').to_string());
            if let Some(module) = module {
                modules.push((
                    module,
                    go_mod.parent().unwrap_or(Path::new(".")).to_path_buf(),
                ));
            }
        }
        // Longest module path wins when one path prefixes another.
        modules.sort_by_key(|(module, _)| std::cmp::Reverse(module.len()));
        let mut package_files: HashMap<PathBuf, Vec<PathBuf>> = HashMap::new();
        for file in go_files {
            package_files
                .entry(file.parent().unwrap_or(Path::new(".")).to_path_buf())
                .or_default()
                .push(file.clone());
        }
        GoIndex {
            modules,
            package_files,
        }
    }

    pub(crate) fn resolve(&self, import_path: &str, importing_file: &Path) -> Resolution {
        for (module, dir) in &self.modules {
            let prefix = format!("{module}/");
            let rel = if import_path == module {
                ""
            } else if import_path.starts_with(&prefix) {
                &import_path[prefix.len()..]
            } else {
                continue;
            };
            let target_dir = dir.join(rel);
            let mut files: Vec<PathBuf> = self
                .package_files
                .get(&target_dir)
                .map(|files| {
                    files
                        .iter()
                        .filter(|f| f.as_path() != importing_file)
                        .cloned()
                        .collect()
                })
                .unwrap_or_default();
            if files.is_empty() {
                return Resolution::Unresolved;
            }
            files.sort();
            return Resolution::Resolved(files);
        }
        Resolution::External
    }
}

// -------------------------------------------------------------- Python ----

/// Resolve a Python import against the scan set.
///
/// Absolute imports emulate `sys.path` by walking up from the importing
/// file's directory (nearest match wins); an absolute module that lands
/// nowhere in the repo is `External` — without a venv index, stdlib and
/// site-packages are indistinguishable from a missed local, so the resolved
/// count is the honest part. Relative imports have no external semantics, so
/// a miss there is `Unresolved`.
pub(crate) fn resolve_python(
    module: &[String],
    level: usize,
    from_name: &Option<Vec<String>>,
    file: &Path,
    nodes: &HashSet<PathBuf>,
) -> Resolution {
    let dir = file.parent().unwrap_or(Path::new("."));
    let (module_file, absolute) = if level == 0 {
        let mut base = Some(dir);
        let mut found = None;
        while let Some(b) = base {
            if let Some(hit) = probe_module(b, module, nodes) {
                found = Some(hit);
                break;
            }
            base = b.parent();
        }
        (found, true)
    } else {
        let mut base = Some(dir);
        for _ in 0..level - 1 {
            base = base.and_then(Path::parent);
        }
        (base.and_then(|b| probe_module(b, module, nodes)), false)
    };

    let Some(module_file) = module_file else {
        return if absolute {
            Resolution::External
        } else {
            Resolution::Unresolved
        };
    };

    // `from x import y`: the official semantics are two-step (attribute, or
    // submodule) — a filesystem probe decides which, which is exactly as
    // much as any syntax-only tool can know (research §4.1). A submodule can
    // only exist when the module is a package (`__init__.py`).
    if let Some(name) = from_name
        && module_file.file_name().is_some_and(|n| n == "__init__.py")
        && let Some(package_dir) = module_file.parent()
        && let Some(hit) = probe_module(package_dir, name, nodes)
    {
        return Resolution::Resolved(vec![hit]);
    }
    Resolution::Resolved(vec![module_file])
}

/// `dir` + dotted segments → the module file, if it is a scanned node.
/// Empty segments address the package itself (`__init__.py`).
fn probe_module(dir: &Path, segments: &[String], nodes: &HashSet<PathBuf>) -> Option<PathBuf> {
    let Some(last) = segments.last() else {
        return nodes
            .contains(&dir.join("__init__.py"))
            .then(|| dir.join("__init__.py"));
    };
    let mut cur = dir.to_path_buf();
    for segment in &segments[..segments.len() - 1] {
        cur = cur.join(segment);
    }
    let candidates = [
        cur.join(format!("{last}.py")),
        cur.join(last).join("__init__.py"),
    ];
    candidates.into_iter().find(|c| nodes.contains(c))
}

// ---------------------------------------------------------- TypeScript ----

/// Resolve a TS/JS specifier. Only relative specifiers carry a filesystem
/// meaning the syntax can see; `#subpath` and the common alias spellings
/// (`@/`, `~/`) stay `Unresolved` so the per-language stats show the gap
/// instead of hiding it under `External` (tsconfig `paths` is phase 2).
pub(crate) fn resolve_ts(specifier: &str, file: &Path, nodes: &HashSet<PathBuf>) -> Resolution {
    if !specifier.starts_with("./") && !specifier.starts_with("../") {
        return match specifier.starts_with('#') || specifier.starts_with("@/") {
            true => Resolution::Unresolved,
            false => Resolution::External,
        };
    }
    let base = lexical_normalize(&file.parent().unwrap_or(Path::new(".")).join(specifier));
    let candidates = ts_candidates(&base);
    candidates
        .into_iter()
        .find(|candidate| nodes.contains(candidate))
        .map(|hit| Resolution::Resolved(vec![hit]))
        .unwrap_or(Resolution::Unresolved)
}

/// Every path a resolver might pick for one specifier base: extension
/// substitution (`.js` → `.ts`/`.tsx`/`.d.ts`) before directory `index`.
fn ts_candidates(base: &Path) -> Vec<PathBuf> {
    const EXTS: &[&str] = &["ts", "tsx", "d.ts", "js", "jsx", "mjs", "cjs"];
    let mut out = Vec::new();
    let base_str = base.to_string_lossy();
    let rewritten = [".js", ".jsx", ".mjs", ".cjs"]
        .iter()
        .find_map(|js| base_str.strip_suffix(js).map(|stem| (stem, *js)));
    match rewritten {
        Some((stem, _)) => {
            // `./a.js` may really be `./a.ts` on disk (TS substitution).
            for ext in ["ts", "tsx", "d.ts"] {
                out.push(PathBuf::from(format!("{stem}.{ext}")));
            }
            out.push(base.to_path_buf());
        }
        None => {
            out.push(base.to_path_buf());
            for ext in EXTS {
                out.push(PathBuf::from(format!("{base_str}.{ext}")));
            }
        }
    }
    for ext in EXTS {
        out.push(base.join(format!("index.{ext}")));
    }
    out
}

// -------------------------------------------------------------- helpers ----

/// Lexically resolve `.` and `..` without touching the filesystem; probe
/// paths built from canonical parents stay canonical, so a plain set lookup
/// against the canonicalized scan set suffices.
fn lexical_normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out
}

/// Canonicalize `path` and check it against the scan set.
pub(crate) fn canonical_in_nodes(path: &Path, nodes: &HashSet<PathBuf>) -> Option<PathBuf> {
    let canonical = std::fs::canonicalize(path).ok()?;
    nodes.contains(&canonical).then_some(canonical)
}
