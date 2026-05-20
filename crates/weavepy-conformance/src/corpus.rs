//! Corpus discovery for the conformance harness.
//!
//! The corpus is the collection of Python source files we grade WeavePy on.
//! Two sources are supported:
//!
//! 1. **In-tree**: small fixtures checked into `conformance/corpus/`. Always
//!    available, intended for fast iteration in the inner dev loop.
//! 2. **Vendored CPython**: optional submodule at `vendor/cpython/`. If
//!    present, a curated set of files under `Lib/test/` is added to the
//!    corpus.
//!
//! Discovery is intentionally unsophisticated: a fixed allowlist of CPython
//! files plus a recursive walk of the in-tree directory. A manifest with
//! per-file expectations will arrive alongside the regrtest runner.

use std::path::{Path, PathBuf};

/// A single Python source file scheduled for grading.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CorpusFile {
    /// Absolute (or workspace-relative) path on disk.
    pub path: PathBuf,
    /// Stable label used in reports and JSON output, e.g.
    /// `"in-tree/lex_arith.py"`. Independent of the absolute path so that
    /// report diffs across machines remain readable.
    pub label: String,
}

/// CPython files we currently track. Curated to focus on the front of the
/// pipeline (lexer/parser); the runtime-heavy tests are deliberately out
/// of scope until the VM can run anything.
pub const CPYTHON_INCLUDE: &[&str] = &["test_tokenize.py", "test_grammar.py", "test_ast.py"];

/// Discover all corpus files relative to `workspace_root`.
///
/// Results are sorted by label for deterministic report ordering.
pub fn discover(workspace_root: &Path) -> Vec<CorpusFile> {
    let mut files = Vec::new();

    let in_tree = workspace_root.join("conformance").join("corpus");
    if in_tree.is_dir() {
        collect_in_tree(&in_tree, &mut files);
    }

    let cpython_test = workspace_root
        .join("vendor")
        .join("cpython")
        .join("Lib")
        .join("test");
    if cpython_test.is_dir() {
        collect_cpython(&cpython_test, &mut files);
    }

    files.sort_by(|a, b| a.label.cmp(&b.label));
    files
}

fn collect_in_tree(root: &Path, out: &mut Vec<CorpusFile>) {
    for entry in walkdir::WalkDir::new(root)
        .into_iter()
        .filter_map(Result::ok)
    {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        if path.extension().and_then(|s| s.to_str()) != Some("py") {
            continue;
        }
        let rel = path.strip_prefix(root).unwrap_or(path);
        let label = format!("in-tree/{}", rel.display());
        out.push(CorpusFile {
            path: path.to_path_buf(),
            label,
        });
    }
}

fn collect_cpython(test_dir: &Path, out: &mut Vec<CorpusFile>) {
    for name in CPYTHON_INCLUDE {
        let p = test_dir.join(name);
        if p.is_file() {
            out.push(CorpusFile {
                path: p,
                label: format!("cpython/Lib/test/{name}"),
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discover_returns_empty_for_missing_root() {
        let tmp = std::env::temp_dir().join("weavepy-conformance-missing");
        let files = discover(&tmp);
        assert!(files.is_empty());
    }
}
