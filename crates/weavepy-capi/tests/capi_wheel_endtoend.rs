//! Full RFC 0029 end-to-end: bake a binary wheel, install it with
//! `_minipip`, then `import` the extension through the regular
//! `importlib.machinery.ExtensionFileLoader` path.
//!
//! This is the canonical smoke test that wheels containing C
//! extensions actually land on `sys.path` and load through the
//! C-API bridge — i.e. the "numpy installs" claim from RFC 0029
//! is honoured end-to-end.
//!
//! Skipped (passes trivially) when the C extension was not built
//! by `build.rs` — that happens when `cc` is missing in CI.

use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

use weavepy::{run_source_with_options, InterpreterFlags, RunOptions};

fn numpylike_path() -> Option<PathBuf> {
    option_env!("WEAVEPY_CAPI_NUMPYLIKE_EXTENSION").map(PathBuf::from)
}

/// Render `s` as a Python single-quoted literal, escaping backslashes
/// and quotes. We avoid Rust's `Debug` formatter because it would
/// double-escape Unicode and produce `"…"` not `'…'`, which makes
/// the eyeball-debugging story worse than just doing it ourselves.
fn py_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\'' => out.push_str("\\'"),
            c => out.push(c),
        }
    }
    out.push('\'');
    out
}

/// Materialise a tiny but valid PEP 427 wheel that ships a single
/// compiled `_numpylike.so` payload. The wheel structure is:
///
/// ```text
/// _numpylike.cpython-313-<plat>.so   ← the extension
/// numpylike-1.0.0.dist-info/METADATA
/// numpylike-1.0.0.dist-info/WHEEL
/// numpylike-1.0.0.dist-info/RECORD
/// ```
fn build_wheel(out_dir: &Path, ext_path: &Path) -> PathBuf {
    // The wheel format is a regular zip; we hand-roll a minimal one
    // here so the test doesn't depend on the host `zip` binary or
    // an extra crate.
    let wheel_path = out_dir.join("numpylike-1.0.0-cp313-cp313-any.whl");
    let mut wheel = zip_minimal::Writer::new(File::create(&wheel_path).unwrap());

    let so_name = if cfg!(target_os = "windows") {
        "_numpylike.pyd"
    } else {
        "_numpylike.so"
    };

    let mut so_bytes = Vec::new();
    File::open(ext_path)
        .expect("opening extension")
        .read_to_end(&mut so_bytes)
        .expect("reading extension");
    wheel.add_file(so_name, &so_bytes);

    wheel.add_file(
        "numpylike-1.0.0.dist-info/METADATA",
        b"Metadata-Version: 2.1\nName: numpylike\nVersion: 1.0.0\n",
    );
    wheel.add_file(
        "numpylike-1.0.0.dist-info/WHEEL",
        b"Wheel-Version: 1.0\nGenerator: weavepy-test/0.1\nRoot-Is-Purelib: false\nTag: cp313-cp313-any\n",
    );
    wheel.add_file("numpylike-1.0.0.dist-info/RECORD", b"");

    wheel.finalize();
    wheel_path
}

#[test]
fn wheel_install_and_import_round_trip() {
    let Some(ext) = numpylike_path() else {
        eprintln!("WEAVEPY_CAPI_NUMPYLIKE_EXTENSION not set; skipping");
        return;
    };
    if !ext.is_file() {
        eprintln!("extension path missing: {} — skipping", ext.display());
        return;
    }

    // Lay out a private venv-style prefix:
    //   <tmp>/
    //     bin/
    //     lib/python3.13/site-packages/
    //     wheels/numpylike-1.0.0-cp313-cp313-any.whl
    let tmp = tempfile::tempdir().expect("mktemp");
    let prefix = tmp.path();
    let site_packages = prefix.join("lib/python3.13/site-packages");
    std::fs::create_dir_all(&site_packages).unwrap();
    std::fs::create_dir_all(prefix.join("bin")).unwrap();
    let wheel_dir = prefix.join("wheels");
    std::fs::create_dir_all(&wheel_dir).unwrap();
    let wheel = build_wheel(&wheel_dir, &ext);

    // Drive WeavePy to:
    //   1. add the venv prefix as `sys.prefix` (via `VIRTUAL_ENV`)
    //   2. invoke `_minipip._install_wheel(<wheel>)` so the .so
    //      lands in site-packages alongside the dist-info metadata
    //   3. prepend site-packages to `sys.path`
    //   4. `import _numpylike` and exercise a few APIs to prove
    //      the dlopen + PyInit dance ran.
    let p_prefix = py_quote(&prefix.display().to_string());
    let p_wheel = py_quote(&wheel.display().to_string());
    let p_site = py_quote(&site_packages.display().to_string());
    let driver = format!(
        "
import os, sys
os.environ['VIRTUAL_ENV'] = {p_prefix}

import _minipip
installed = _minipip._install_wheel({p_wheel}, dest={p_site})
assert installed, 'wheel install returned no paths'

sys.path.insert(0, {p_site})
import _numpylike
arr = _numpylike.arange(5)
total = arr.sum()
shape = arr.shape
assert shape == (5,), shape
assert total == 10, total
print('numpylike import OK from wheel; arr.shape =', shape, 'sum =', total)
"
    );

    let opts = RunOptions::new("<wheel-test>").with_flags(InterpreterFlags::default());
    if let Err(err) = run_source_with_options(&driver, &opts) {
        let formatted = err.format(&driver, "<wheel-test>");
        panic!("wheel install/import failed:\n{formatted}");
    }
}

// ---------------------------------------------------------------------
// Minimal zip writer
//
// We don't want to pull `zip` into the dev-deps just for one test, so
// inline a tiny store-mode (uncompressed) writer. It produces a
// PKZIP file that `zipfile` (in CPython and WeavePy) reads back
// without complaint.
// ---------------------------------------------------------------------

mod zip_minimal {
    use std::fs::File;
    use std::io::Write;

    pub(crate) struct Writer {
        file: File,
        offset: u32,
        entries: Vec<Entry>,
    }

    struct Entry {
        name: Vec<u8>,
        crc: u32,
        size: u32,
        local_offset: u32,
    }

    impl Writer {
        pub(crate) fn new(file: File) -> Self {
            Self {
                file,
                offset: 0,
                entries: Vec::new(),
            }
        }

        pub(crate) fn add_file(&mut self, name: &str, payload: &[u8]) {
            let crc = crc32(payload);
            let name_bytes = name.as_bytes().to_vec();
            let local_offset = self.offset;
            // Local file header (PK\x03\x04).
            self.write_all(&[0x50, 0x4b, 0x03, 0x04]);
            self.write_all(&20u16.to_le_bytes()); // version needed
            self.write_all(&0u16.to_le_bytes()); // flags
            self.write_all(&0u16.to_le_bytes()); // method (store)
            self.write_all(&0u16.to_le_bytes()); // mtime
            self.write_all(&0u16.to_le_bytes()); // mdate
            self.write_all(&crc.to_le_bytes());
            self.write_all(&(payload.len() as u32).to_le_bytes());
            self.write_all(&(payload.len() as u32).to_le_bytes());
            self.write_all(&(name_bytes.len() as u16).to_le_bytes());
            self.write_all(&0u16.to_le_bytes());
            self.write_all(&name_bytes);
            self.write_all(payload);
            self.entries.push(Entry {
                name: name_bytes,
                crc,
                size: payload.len() as u32,
                local_offset,
            });
        }

        pub(crate) fn finalize(mut self) {
            let central_offset = self.offset;
            let entries = std::mem::take(&mut self.entries);
            let mut total = 0u32;
            for e in &entries {
                // Central directory header (PK\x01\x02).
                self.write_all(&[0x50, 0x4b, 0x01, 0x02]);
                self.write_all(&20u16.to_le_bytes()); // version made by
                self.write_all(&20u16.to_le_bytes()); // version needed
                self.write_all(&0u16.to_le_bytes()); // flags
                self.write_all(&0u16.to_le_bytes()); // method
                self.write_all(&0u16.to_le_bytes()); // mtime
                self.write_all(&0u16.to_le_bytes()); // mdate
                self.write_all(&e.crc.to_le_bytes());
                self.write_all(&e.size.to_le_bytes()); // compressed
                self.write_all(&e.size.to_le_bytes()); // uncompressed
                self.write_all(&(e.name.len() as u16).to_le_bytes());
                self.write_all(&0u16.to_le_bytes()); // extra
                self.write_all(&0u16.to_le_bytes()); // comment
                self.write_all(&0u16.to_le_bytes()); // disk
                self.write_all(&0u16.to_le_bytes()); // internal attrs
                self.write_all(&0u32.to_le_bytes()); // external attrs
                self.write_all(&e.local_offset.to_le_bytes());
                self.write_all(&e.name);
                total += 1;
            }
            let central_size = self.offset - central_offset;
            // EOCD record (PK\x05\x06).
            self.write_all(&[0x50, 0x4b, 0x05, 0x06]);
            self.write_all(&0u16.to_le_bytes()); // disk
            self.write_all(&0u16.to_le_bytes()); // start disk
            self.write_all(&(total as u16).to_le_bytes());
            self.write_all(&(total as u16).to_le_bytes());
            self.write_all(&central_size.to_le_bytes());
            self.write_all(&central_offset.to_le_bytes());
            self.write_all(&0u16.to_le_bytes()); // comment len
            let _ = self.file.flush();
        }

        fn write_all(&mut self, buf: &[u8]) {
            self.file.write_all(buf).expect("write");
            self.offset += buf.len() as u32;
        }
    }

    fn crc32(buf: &[u8]) -> u32 {
        let mut table = [0u32; 256];
        for i in 0..256u32 {
            let mut c = i;
            for _ in 0..8 {
                c = if c & 1 != 0 {
                    0xedb8_8320 ^ (c >> 1)
                } else {
                    c >> 1
                };
            }
            table[i as usize] = c;
        }
        let mut crc = 0xffff_ffff_u32;
        for &b in buf {
            let idx = ((crc ^ u32::from(b)) & 0xff) as usize;
            crc = table[idx] ^ (crc >> 8);
        }
        crc ^ 0xffff_ffff
    }
}
