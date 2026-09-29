//! Scanning, exclusion and skip accounting.
//!
//! The point of these tests is the *negative* space: what must not appear in
//! the results, and which bucket each omission is reported in.

use std::fs;
use std::path::Path;

use qingluan_complexity::scan::relative_path;
use qingluan_complexity::{Language, ScanOptions, analyze_path, scan};
use tempfile::TempDir;

fn write(root: &Path, relative: &str, contents: &str) {
    let path = root.join(relative);
    fs::create_dir_all(path.parent().expect("file has a parent")).unwrap();
    fs::write(path, contents).unwrap();
}

fn scanned_paths(report: &qingluan_complexity::ScanReport) -> Vec<String> {
    report
        .files
        .iter()
        .map(|file| relative_path(&report.root, &file.path))
        .collect()
}

#[test]
fn scan_keeps_real_code_and_reports_every_skip_reason() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    write(root, "src/lib.rs", "fn ok() {}\n");
    // Build output and vendored trees are excluded outright, never counted.
    write(root, "target/debug/build.rs", "fn generated() {}\n");
    write(
        root,
        "node_modules/pkg/index.js",
        "function vendored() {}\n",
    );
    // `.gitignore` applies even though a temp directory is not a git repo.
    write(root, "src/ignored.rs", "fn ignored() {}\n");
    write(root, ".gitignore", "src/ignored.rs\n");
    // Machine output, code without a grammar, and something non-code.
    write(root, "web/bundle.min.js", "function minified(){}\n");
    write(root, "native/tool.c", "int main(void) { return 0; }\n");
    write(root, "docs/README.md", "# hi\n");
    write(root, "src/huge.rs", &"// padding\n".repeat(200));

    let mut options = ScanOptions::new(root);
    options.max_file_bytes = 512;
    let report = scan(&options).unwrap();

    assert_eq!(scanned_paths(&report), vec!["src/lib.rs".to_string()]);
    assert_eq!(report.skipped.generated, 1);
    assert_eq!(report.skipped.unsupported, 1);
    assert_eq!(report.skipped.too_large, 1);
    assert_eq!(report.skipped.total(), 3);
    assert_eq!(report.function_count(), 1);
}

#[test]
fn include_and_exclude_globs_narrow_the_scan() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    write(root, "src/lib.rs", "fn rust() {}\n");
    write(root, "web/app.ts", "export function ts() {}\n");

    let mut include = ScanOptions::new(root);
    include.include = vec!["**/*.ts".into()];
    let report = scan(&include).unwrap();
    assert_eq!(scanned_paths(&report), vec!["web/app.ts".to_string()]);

    let mut exclude = ScanOptions::new(root);
    exclude.exclude = vec!["**/web/**".into()];
    let report = scan(&exclude).unwrap();
    assert_eq!(scanned_paths(&report), vec!["src/lib.rs".to_string()]);
}

#[test]
fn a_missing_path_is_an_error_not_an_empty_report() {
    let dir = TempDir::new().unwrap();
    let mut options = ScanOptions::new(dir.path());
    options.paths = vec![dir.path().join("nope")];
    let error = scan(&options).expect_err("missing path must fail");
    assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
}

#[test]
fn analyze_path_returns_none_for_a_file_without_a_grammar() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("notes.md");
    fs::write(&path, "# hi\n").unwrap();
    assert!(analyze_path(&path).unwrap().is_none());

    let rust = dir.path().join("lib.rs");
    fs::write(&rust, "fn one() {}\nfn two() {}\n").unwrap();
    let analyzed = analyze_path(&rust).unwrap().expect("rust is supported");
    assert_eq!(analyzed.language, Language::Rust);
    assert_eq!(analyzed.functions.len(), 2);
}
