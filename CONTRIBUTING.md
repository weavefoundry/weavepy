# Contributing to WeavePy

Welcome! This document covers the day-to-day mechanics of working on
WeavePy. For higher-level design context, read
[`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) first.

## Development setup

WeavePy is a pure Cargo workspace targeting stable Rust. The toolchain is
pinned via `rust-toolchain.toml`, so all you need is `rustup`:

```bash
git clone https://github.com/weavefoundry/weavepy
cd weavepy
cargo build --workspace
cargo test --workspace
```

## Editor setup

- `rust-analyzer` works out of the box. Set it to use the workspace's
  `Cargo.toml` rather than per-crate.
- `.editorconfig` is provided. Most modern editors honour it automatically.
- `rustfmt.toml` is used by `cargo fmt`. Some advanced options are
  nightly-only and are commented out; if you want them, use
  `cargo +nightly fmt`.

## Pre-commit checklist

Run these locally before opening a PR. CI runs the same set on every push.

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-targets
```

The workspace defines convenience aliases that match CI exactly:

```bash
cargo xfmt    # fmt --all
cargo xclippy # clippy with -D warnings
cargo xtest   # test --workspace --all-targets
cargo xcheck  # check --workspace --all-targets --all-features
```

## Coding standards

- **Compatibility is paramount.** When in doubt about behavior, do what
  CPython does. Match exception types, error messages, edge-case semantics,
  and undocumented quirks unless we have a written reason not to.
- **Document compatibility level.** Public items should state whether they
  are "stable WeavePy API", "tracks CPython behavior", or "experimental".
- **No casual `unsafe`.** Every `unsafe` block needs a `// SAFETY:` comment
  describing the invariants the caller (or callee) is upholding.
- **Tests live next to code.** Unit tests in `#[cfg(test)] mod tests`,
  workspace-level integration tests under a top-level `tests/` directory
  (added when it earns its keep).
- **Snapshot the AST and bytecode.** Once parser and compiler output is
  meaningful, prefer `insta` snapshot tests over hand-rolled equality
  assertions.

## Proposing larger changes

For anything bigger than a bug fix or small feature, open an RFC under
[`docs/rfcs/`](docs/rfcs/). Copy `0000-template.md`, fill in the
template, and open a PR. Discussion happens in the PR; once consensus is
reached, the RFC is merged with a number assigned and implementation
work proceeds in follow-up PRs.

## Commit messages

We loosely follow [Conventional Commits](https://www.conventionalcommits.org/):

```
<type>(<scope>): <subject>

<body>
```

Common types: `feat`, `fix`, `perf`, `refactor`, `test`, `docs`, `build`,
`ci`, `chore`. The `scope` is usually a crate name (`lexer`, `parser`,
`compiler`, `vm`, `cli`) or `workspace` for cross-cutting changes.

Examples:

```
feat(lexer): tokenize integer literals with separators
fix(vm): preserve frame f_back across generator resumes
perf(compiler): cache constant pool lookups in nested scopes
```

## License

By contributing, you agree that your contribution will be dual-licensed
under MIT and Apache-2.0 (see [`LICENSE-MIT`](LICENSE-MIT) and
[`LICENSE-APACHE`](LICENSE-APACHE)).
