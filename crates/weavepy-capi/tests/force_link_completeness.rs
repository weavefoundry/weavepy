//! Force-link completeness guard (RFC 0047, wave 5).
//!
//! Every `#[no_mangle] extern "C" fn` this crate defines is part of the
//! CPython ABI surface a dlopen'd extension may bind against. On macOS
//! the linker dead-strips any such function that isn't rooted in
//! [`weavepy_capi::force_link_table`]; afterwards an extension that
//! calls the missing entry point jumps through an unbound PLT stub into
//! a NULL address and segfaults with no Rust frame on the stack.
//!
//! That is exactly how real numpy crashed: `numpy.random.SeedSequence`
//! runs `n //= 2**32`, Cython lowers it to `PyNumber_InPlaceFloorDivide`,
//! and that function — though defined here — had never been added to the
//! table, so it was stripped and the call faulted.
//!
//! Rather than trust a hand-maintained list, this test re-derives the
//! full set of defined entry points straight from the source tree and
//! asserts every one survives into the host binary's dynamic symbol
//! table. If anyone adds a `#[no_mangle]` C-API function without rooting
//! it, this fails the build with the precise list to fix.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Reference the force-link table so this very test binary links the
/// `#[used]` root array (and therefore every symbol it pins). Without
/// this, an integration test that touched no other crate symbol could
/// leave the table's object file out of the link entirely.
fn ensure_table_linked() -> usize {
    weavepy_capi::force_link_table::touch()
}

fn src_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src")
}

fn collect_rs(dir: &Path, files: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).expect("read_dir src") {
        let path = entry.expect("dirent").path();
        if path.is_dir() {
            collect_rs(&path, files);
        } else if path.extension().and_then(|s| s.to_str()) == Some("rs") {
            files.push(path);
        }
    }
}

/// Extract `NAME` from a line containing `extern "C" fn NAME`. Returns
/// `None` for function-pointer *types* (`extern "C" fn(...)`, no space
/// before the paren) and for lines without the marker.
fn extern_c_fn_name(line: &str) -> Option<String> {
    let marker = "extern \"C\" fn ";
    let idx = line.find(marker)?;
    let rest = &line[idx + marker.len()..];
    let name: String = rest
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect();
    (!name.is_empty()).then_some(name)
}

/// Names of all `#[no_mangle] extern "C" fn` defined under `src/`,
/// skipping `#[cfg(...)]`-gated definitions (which may legitimately be
/// absent on this target).
fn defined_symbols() -> BTreeSet<String> {
    let mut files = Vec::new();
    collect_rs(&src_dir(), &mut files);
    files.sort();

    let mut out = BTreeSet::new();
    for file in files {
        let text = std::fs::read_to_string(&file).expect("read src file");
        let mut no_mangle_at: Option<usize> = None;
        let mut cfg_gated = false;
        for (i, raw) in text.lines().enumerate() {
            let line = raw.trim_start();
            // A blank line ends an attribute run: `#[no_mangle]` and the
            // signature it decorates are always contiguous.
            if line.is_empty() {
                no_mangle_at = None;
                cfg_gated = false;
                continue;
            }
            if line.starts_with("//") {
                continue;
            }
            if line.contains("no_mangle") {
                no_mangle_at = Some(i);
                cfg_gated = false;
            }
            if no_mangle_at.is_some() && line.contains("#[cfg(") {
                cfg_gated = true;
            }
            if let Some(name) = extern_c_fn_name(raw) {
                if let Some(start) = no_mangle_at {
                    if i - start <= 8 && !cfg_gated {
                        out.insert(name);
                    }
                    no_mangle_at = None;
                    cfg_gated = false;
                }
            }
        }
    }
    out
}

/// Defined, external symbols exported by the running test executable —
/// i.e. the set a dlopen'd extension could actually resolve against it.
fn exported_symbols() -> Option<BTreeSet<String>> {
    let exe = std::env::current_exe().ok()?;
    let macos = cfg!(target_os = "macos");

    let mut cmd = Command::new("nm");
    if macos {
        // External (`-g`), defined (`-U` = suppress undefined) symbols;
        // macOS executables expose these to dlopen by default.
        cmd.arg("-gU");
    } else {
        // The ELF dynamic symbol table is the dlopen-visible export set
        // (`crates/weavepy-capi/build.rs` adds `--export-dynamic`).
        cmd.arg("-D").arg("--defined-only");
    }
    let output = cmd.arg(&exe).output().ok()?;
    if !output.status.success() {
        return None;
    }

    let mut set = BTreeSet::new();
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        let cols: Vec<&str> = line.split_whitespace().collect();
        let (ty, name) = match cols.as_slice() {
            [ty, name] => (*ty, *name),         // undefined (no address)
            [_addr, ty, name] => (*ty, *name),  // defined
            _ => continue,
        };
        if ty.eq_ignore_ascii_case("u") {
            continue;
        }
        // Mach-O prepends a single underscore to C symbol names.
        let name = if macos {
            name.strip_prefix('_').unwrap_or(name)
        } else {
            name
        };
        set.insert(name.to_string());
    }
    Some(set)
}

#[test]
fn every_no_mangle_export_survives_dead_strip() {
    let table_len = ensure_table_linked();
    assert!(table_len > 0, "force-link table is empty");

    let defined = defined_symbols();
    assert!(
        defined.len() > 400,
        "source scan found only {} #[no_mangle] fns — the scanner is \
         probably broken, not the table",
        defined.len()
    );

    let Some(exported) = exported_symbols() else {
        eprintln!(
            "warning: `nm` unavailable or failed; skipping force-link \
             completeness check"
        );
        return;
    };

    let missing: Vec<&str> = defined
        .iter()
        .filter(|name| !exported.contains(*name))
        .map(String::as_str)
        .collect();

    assert!(
        missing.is_empty(),
        "\n{} C-API function(s) are defined `#[no_mangle]` but were \
         dead-stripped (absent from this binary's exports). Every one \
         will segfault a dlopen'd extension that calls it. Root each in \
         `src/force_link_table.rs`:\n  {}\n",
        missing.len(),
        missing.join("\n  ")
    );
}
