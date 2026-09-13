# Repository artifact policy

Commit maintained source, reusable tools, regression fixtures, and concise
documentation. Keep raw timing results, allocation profiles, logs, copied
source snapshots, frozen caches, and research archives under `target/` or in
separate research storage. CI can publish run output with artifact uploads.
Older local tools may use the already ignored `tmp/` directory.

Generated files can still be required build or test inputs. The Unicode tables,
CJK codec tables, vendored dependency metadata, and
`crates/weavepy-bench/baselines/bench-*.json` stay tracked: the runtime, build, or
CI consumes them. Don't blanket-ignore or delete every JSON, text, or binary file.

The census directory ignores new research directories by default. To add a
maintained probe, allow its directory in `crates/weavepy-bench/census/.gitignore`
and list the exact source files in that directory's `.gitignore`. Required new
fixture data needs a narrow, reviewed exception. Keep larger result collections
outside Git and link a concise report to their location or historical commit.

Before committing, stage the intended files and run:

```sh
python3 tools/check_repository_artifacts.py
git diff --cached --stat
```

The checker examines the Git index using repository `.gitignore` files. It
rejects ignored files even when they were already tracked or added with
`git add -f`. Personal global ignores and `.git/info/exclude` don't change this
policy. CI runs the same check. It detects policy violations, not the provenance
of every file, so source and data still need review.

Adding an ignore rule doesn't untrack an existing file. Use
`git rm --cached -- path/to/generated-file` to remove it from the next commit
while keeping the local copy. Normal cleanup commits leave earlier history
intact; they don't reclaim the historical Git objects. History rewriting is a
separate operation and isn't needed for these small older result collections.
