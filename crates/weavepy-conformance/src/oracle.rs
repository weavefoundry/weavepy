//! Subprocess-based CPython oracle.
//!
//! Invokes a `python3` binary on the host (configurable via
//! [`crate::PYTHON_ENV_VAR`]) and asks it, in turn, to:
//!
//! - tokenize a file using the standard-library [`tokenize`] module,
//! - parse a file and dump the AST via [`ast.parse`] + [`ast.dump`],
//! - compile a file and disassemble the result with [`dis.dis`].
//!
//! The output of CPython is the ground truth WeavePy is graded against. We
//! capture stdout, parse JSON where applicable, and surface non-zero exits
//! as oracle errors rather than panicking — every input that the oracle
//! can't handle is still a meaningful classification.
//!
//! Each oracle script lives as an inline string constant so the crate is
//! self-contained: no separate scripts to ship, no path resolution at
//! runtime.
//!
//! [`tokenize`]: https://docs.python.org/3/library/tokenize.html
//! [`ast.parse`]: https://docs.python.org/3/library/ast.html#ast.parse
//! [`ast.dump`]: https://docs.python.org/3/library/ast.html#ast.dump
//! [`dis.dis`]: https://docs.python.org/3/library/dis.html#dis.dis

use std::path::Path;
use std::process::{Command, Stdio};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

/// A single token emitted by CPython's [`tokenize`] module.
///
/// We deliberately retain only the fields stable enough to diff against:
/// the symbolic token name and the raw lexeme text. Position info is
/// available from CPython but ignored until WeavePy and CPython agree on a
/// span representation.
///
/// [`tokenize`]: https://docs.python.org/3/library/tokenize.html
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OracleToken {
    /// Token name from `tokenize.tok_name` (e.g. `"NAME"`, `"NUMBER"`,
    /// `"OP"`, `"NEWLINE"`, `"ENDMARKER"`).
    #[serde(rename = "type")]
    pub kind: String,
    /// Raw token text as it appeared in the source.
    pub string: String,
}

/// Tokenize a Python file using CPython's `tokenize` module.
pub fn tokens(python: &str, source_file: &Path) -> Result<Vec<OracleToken>> {
    let stdout = run_python(python, ORACLE_TOKENS, source_file)?;
    serde_json::from_str(&stdout)
        .with_context(|| format!("failed to parse oracle JSON for {}", source_file.display()))
}

/// Parse a Python file with CPython and return `ast.dump(tree, indent=2)`.
pub fn ast_dump(python: &str, source_file: &Path) -> Result<String> {
    run_python(python, ORACLE_AST_DUMP, source_file)
}

/// Compile a Python file with CPython and return `dis.dis(code)` output.
pub fn dis(python: &str, source_file: &Path) -> Result<String> {
    run_python(python, ORACLE_DIS, source_file)
}

/// Verify that the configured python binary exists and is runnable.
///
/// Returns the trimmed `python -V` banner on success, e.g. `"Python 3.13.1"`.
pub fn ensure_available(python: &str) -> Result<String> {
    let output = Command::new(python)
        .arg("-V")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .with_context(|| {
            format!(
                "failed to launch `{python}`. install Python 3.13+ or set ${}",
                crate::PYTHON_ENV_VAR
            )
        })?;
    if !output.status.success() {
        bail!("`{python} -V` exited with status {}", output.status);
    }
    // `python -V` historically wrote to stderr; modern CPython writes to
    // stdout. Accept either.
    let banner = if output.stdout.is_empty() {
        String::from_utf8_lossy(&output.stderr).trim().to_owned()
    } else {
        String::from_utf8_lossy(&output.stdout).trim().to_owned()
    };
    Ok(banner)
}

fn run_python(python: &str, script: &str, source_file: &Path) -> Result<String> {
    let output = Command::new(python)
        .arg("-c")
        .arg(script)
        .arg(source_file)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .with_context(|| format!("failed to launch `{python}`"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!(
            "python oracle exited with status {} on {}\nstderr:\n{}",
            output.status,
            source_file.display(),
            stderr.trim_end(),
        );
    }
    String::from_utf8(output.stdout).context("oracle stdout was not utf-8")
}

const ORACLE_TOKENS: &str = r#"
import json, sys, tokenize
path = sys.argv[1]
out = []
try:
    with open(path, 'rb') as f:
        for tok in tokenize.tokenize(f.readline):
            out.append({"type": tokenize.tok_name[tok.type], "string": tok.string})
except (tokenize.TokenError, SyntaxError, IndentationError) as e:
    sys.stderr.write(f"{type(e).__name__}: {e}\n")
    sys.exit(2)
json.dump(out, sys.stdout)
"#;

const ORACLE_AST_DUMP: &str = r#"
import ast, sys
src = open(sys.argv[1], 'rb').read()
try:
    tree = ast.parse(src, sys.argv[1])
except SyntaxError as e:
    sys.stderr.write(f"SyntaxError: {e}\n")
    sys.exit(2)
sys.stdout.write(ast.dump(tree, indent=2))
sys.stdout.write("\n")
"#;

const ORACLE_DIS: &str = r#"
import dis, io, sys
src = open(sys.argv[1], 'rb').read()
try:
    code = compile(src, sys.argv[1], 'exec')
except SyntaxError as e:
    sys.stderr.write(f"SyntaxError: {e}\n")
    sys.exit(2)
buf = io.StringIO()
dis.dis(code, file=buf)
sys.stdout.write(buf.getvalue())
"#;
