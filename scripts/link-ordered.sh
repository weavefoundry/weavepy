#!/bin/sh
# The host targets' linker (see .cargo/config.toml): `cc`, after
# scripts/link-ordered.py remaps the `weavepy` symbol order file onto the
# symbols this link defines. Without python3, plain `cc`.
if command -v python3 >/dev/null 2>&1; then
    exec python3 "$(dirname "$0")/link-ordered.py" "$@"
fi
exec "${WEAVEPY_LINK_CC:-cc}" "$@"
