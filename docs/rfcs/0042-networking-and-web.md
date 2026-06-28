# RFC 0042: Networking & web wave — real TLS, a verbatim HTTP/URL/cookie stack, the protocol clients, and a localhost grading harness

- **Status**: Accepted
- **Authors**: WeavePy authors
- **Created**: 2026-06-26
- **Tracking issue**: TBD
- **Builds on**: RFC 0017 (the OS & networking surface — the first `socket`
  pass), RFC 0023 (drop-in parity — `_https`/rustls landed), RFC 0039
  (real OS threads, native `select`/`poll`/`kqueue`, the `SelectorEventLoop`),
  RFC 0040 (real `subprocess`/`multiprocessing` — names "live-network grading"
  as the next step in its Future work), RFC 0041 (the C-accelerator numeric
  tower).

## Summary

WeavePy's transport layer is real — `_socket` is backed by `socket2`/libc,
the asyncio `SelectorEventLoop` works on loopback, and `rustls` powers a
one-shot `_https` fast path — but the **application layer is split and
ungraded**:

1. **TLS is a stub at the public surface.** `ssl_real.rs` performs real
   client handshakes, but only by opening *its own* `TcpStream`; the public
   `ssl` module's `SSLContext.wrap_socket` raises `NotImplementedError`, so
   nothing can wrap an *existing* socket and there is no **server** role.
   `http.client.HTTPSConnection.connect()` therefore raises, and `test_ssl`
   is ungradeable.
2. **`http.client`/`urllib` are hand-written shims**, not CPython ports.
   `HTTPResponse` is not socket-backed (it slurps the whole body up front),
   `urllib.parse`'s results are not `tuple` subclasses, the `urllib.request`
   handler chain is hollow, and `http.cookies` diverges on output. So
   `test_urlparse`/`test_urllib`/`test_http_cookies` **fail** and
   `test_httplib` is **skipped**.
3. **Several client modules don't ship at all** — `http.cookiejar`,
   `ftplib`, `poplib`, `imaplib`, `smtplib` (and `email.mime.*`) are named in
   `sys.stdlib_module_names` but have no source, so the matching tests skip.
4. **The conformance harness can't grade the network.** The subprocess
   bootstrap never sets `support.use_resources`, so `requires('network')`
   raises `ResourceDenied` and there is no localhost fixture server.

This wave closes all four. It is the web/networking counterpart to RFC
0040's systems work and the credible path to the README's promise of running
"existing Python code, packages, tools, and workflows unchanged" — the
ecosystem that real tooling (`requests`/`httpx`/`urllib3`/web servers/mail
clients) is built on.

As with every wave, the deliverable is **measured, not aspirational**: each
workstream names the `expectations.toml` rows it flips, ships at least one
bundled in-process fixture, and the commit is not done until a fresh sweep is
`--check` clean with the touched rows rewritten to their measured status.

## Motivation

After RFC 0040 (systems) and RFC 0041 (numeric tower), the single largest
remaining *capability* cluster — not fidelity nits, but whole subsystems the
suite can't even exercise — is the network/web stack. Two properties make it
the right next wave:

1. **The hard half is already built.** RFC 0017/0039 shipped real
   `socket2`-backed sockets (a socket's handle *is* its OS fd on POSIX),
   native selectors, and a `SelectorEventLoop`; RFC 0023 linked `rustls`.
   This wave is "wrap an existing fd in TLS, give the sockets a real
   `makefile()`, and port CPython's pure-Python drivers verbatim" — finish
   the primitives + vendor the drivers, not a from-scratch runtime change.
2. **The behaviour is defined by CPython.** `http`, `urllib`, `ssl`,
   `ftplib`, `smtplib`, `imaplib`, `poplib`, and `http.cookiejar` are
   pure-Python in CPython 3.13 over a thin `_ssl`/`_socket` core. The
   throughline of RFC 0035–0041 applies directly: **where behaviour is
   defined by CPython, port CPython** — verbatim `.py` modules over a faithful
   native core.

## CPython reference

This RFC matches **CPython 3.13** as defined by the vendored `vendor/cpython/`
tree:

- **ssl** — `Lib/ssl.py` (the `SSLContext`/`SSLSocket`/`SSLObject` wrapper)
  over `Modules/_ssl.c` (the `_ssl._SSLContext`/`_SSLSocket` primitive);
  WeavePy backs the `_ssl` core with `rustls` (client + server roles, memory
  BIO + fd wrapping).
- **http** — `Lib/http/client.py` (`HTTPConnection`/`HTTPSConnection`/
  `HTTPResponse` over `sock.makefile("rb")`), `Lib/http/server.py`,
  `Lib/http/cookies.py`, `Lib/http/cookiejar.py`.
- **urllib** — `Lib/urllib/{parse,request,response,error}.py`.
- **protocol clients** — `Lib/ftplib.py`, `Lib/poplib.py`,
  `Lib/imaplib.py`, `Lib/smtplib.py`, `Lib/email/mime/*`.
- **socket** — `Lib/socket.py` (`SocketIO` + `socket.makefile`) over
  `Modules/socketmodule.c`.
- **test harness** — `Lib/test/libregrtest/` resource model (`-u network`),
  `Lib/test/support/socket_helper.py`.

PEP 703 free-threading stays out of scope (the GIL stays). Windows IOCP/
proactor stays out of scope (the POSIX selector path is faithful).

## Current baseline (measured starting point)

- `cargo build --workspace` is green.
- Bundled `tests/regrtest/` suite is `--check` clean.
- CPython `Lib/test/` allowlist: **94 `pass`, 37 `fail`, 21 `skip`, 1
  `timeout`** across 153 tracked files.

The network-cluster rows this wave targets, with their committed status and
root cause:

| Row | Status | Root cause |
|---|---|---|
| `test_urlparse` | fail | `SplitResult`/`ParseResult` not `tuple` subclasses; WHATWG/IDNA edges |
| `test_http_cookies` | fail | `Morsel`/`SimpleCookie` output formatting |
| `test_http_cookiejar` | skip | `http.cookiejar` not shipped |
| `test_urllib` | fail | hollow `urllib.request` handler chain |
| `test_httplib` | skip | needs a localhost server + faithful `http.client` |
| `test_socketserver` | skip | `ResourceDenied` (`-u network` not enabled) |
| `test_ssl` | skip | no real `ssl.wrap_socket` (client+server) + certs |
| `test_ftplib` | skip | `ftplib` not shipped |
| `test_poplib` | skip | `poplib` not shipped |
| `test_imaplib` | skip | `imaplib` not shipped |
| `test_smtplib` | skip | `smtplib`/`email.mime` not shipped |
| `test_socket` | skip | needs `-u network` + localhost |
| `test_asyncio` | skip | graded as a 31-submodule package incl. SSL/subprocess |

## Detailed design

Six workstreams, sequenced in dependency order. Line-count estimates include
Rust glue, verbatim/ported frozen Python, and tests.

### WS1 — Grading harness: `-u network` + a localhost fixture server + `socket.makefile` · ~2K LOC

- **Resource enablement.** The `regrtest` subprocess bootstrap
  (`weavepy-conformance/src/regrtest.rs`) sets
  `WEAVEPY_REGRTEST_RESOURCES=network,subprocess,...`; `test.support`
  reads it into `use_resources` at import, so `requires('network')` /
  `@requires_resource('network')` stop skipping. The default (no env) stays
  fully sandboxed.
- **Loopback fixture server.** A small bundled server harness (Rust-spawned
  or Python `socketserver`/`threading`) that serves HTTP/echo on
  `127.0.0.1:0` for the `test.support` helpers that expect one. CPython's
  protocol tests (`test_ftplib`/`test_smtplib`/…) already spin up their own
  in-process mock servers over `threading` + `socket`, so the main lever is
  enabling the resource and ensuring `socket`/`select` work on localhost.
- **Real `socket.makefile()`.** Return a genuine buffered `io` stream over
  the socket fd (CPython's `socket.SocketIO` + `io.BufferedReader`/`Writer`)
  so the verbatim `http.client`/`ftplib`/`smtplib`/`imaplib`/`poplib`
  drivers — all of which do `sock.makefile("rb")` — work unchanged.

**Flips:** prerequisite for WS3/WS5; directly enables `test_socketserver`,
`test_socket` (loopback subset).

### WS2 — TLS unification: real `_ssl` over rustls (client + server) · ~3K LOC

Grow `ssl_real.rs` from "open my own client stream" to a faithful `_ssl`
core:

- **Wrap an existing fd.** Given a `socket.socket` handle (== OS fd on
  POSIX), attach a rustls session to that fd without taking ownership away
  from `socket_mod` (dup the fd or borrow via `ManuallyDrop`), for both
  **client** (`ClientConnection`) and **server** (`ServerConnection`) roles.
- **Server config + test certs.** Build a `ServerConfig` from a
  cert-chain/key (PEM), with checked-in self-signed fixtures under
  `tests/regrtest/certs/` for the loopback tests.
- **Faithful `ssl` surface.** Replace the stub `ssl_mod.rs` with a real
  `SSLContext`/`SSLSocket`/`SSLObject` (native, CPython-shaped): `load_cert_chain`,
  `load_verify_locations`, `wrap_socket(sock, server_side=...)`,
  `do_handshake`, `recv`/`send`/`read`/`write`, `getpeercert`,
  `cipher`/`version`/`selected_alpn_protocol`, `unwrap`, the
  `SSLWant{Read,Write}Error` non-blocking surface, and `match_hostname`.
- **Wire `HTTPSConnection`** (WS3) onto `context.wrap_socket(self.sock,
  server_hostname=host)`; retire the `_https` one-shot shim's role in
  `http.client` (kept as a fast path for `urllib` if useful).

**Flips:** `test_ssl` (loopback subset); unblocks `HTTPSConnection`.

### WS3 — Verbatim `http.client` + `http.server` · ~3K LOC

Replace the hand-written shims with CPython 3.13's `Lib/http/client.py` and
`Lib/http/server.py`, ported verbatim over the WS1 `makefile` and WS2 `ssl`.
This brings the faithful `HTTPResponse` (socket-backed, lazy chunked/length
reads, `http.HTTPStatus`, persistent connections, `putrequest`/`putheader`
state machine, 100-continue, `email`-parsed headers).

**Flips:** `test_httplib`; foundation for WS4/WS5.

### WS4 — Verbatim `urllib` + `http.cookies`/`http.cookiejar` · ~5K LOC

Port verbatim: `urllib/parse.py` (the `SplitResult`/`ParseResult` named
tuples, `urlsplit`/`urljoin`/`quote`/`unquote`/`parse_qs[l]`, IDNA),
`urllib/{request,response,error}.py` (the `OpenerDirector` +
`HTTPHandler`/`HTTPSHandler`/`HTTPRedirectHandler`/`HTTPErrorProcessor`/
`ProxyHandler`/auth handlers/`FileHandler`/`FTPHandler`/`DataHandler`),
`http/cookies.py` (`Morsel`/`SimpleCookie`), and `http/cookiejar.py`
(`CookieJar`/`MozillaCookieJar` + the policy machinery).

**Flips:** `test_urlparse`, `test_http_cookies`, `test_http_cookiejar`,
`test_urllib`.

### WS5 — Protocol clients: `ftplib`/`poplib`/`imaplib`/`smtplib` + `email.mime` · ~5K LOC

Port verbatim `Lib/ftplib.py`, `Lib/poplib.py`, `Lib/imaplib.py`,
`Lib/smtplib.py`, and the `Lib/email/mime/*` package over the WS1
`makefile` + WS2 `ssl`. These drivers each `socket.create_connection` + (for
the TLS variants) `context.wrap_socket`, then speak the line protocol over a
`makefile` stream. CPython's tests spin up their own threaded mock servers,
so with the WS1 resource enablement they grade in-process.

**Flips:** `test_ftplib`, `test_poplib`, `test_imaplib`, `test_smtplib`.

### WS6 — Fixtures + measured baseline rewrite + asyncio regrade · ~1.5K LOC

One bundled in-process fixture per workstream under `tests/regrtest/`
(`net_http_roundtrip.py`, `net_tls_roundtrip.py`, `net_urlparse.py`,
`net_cookiejar.py`, …), the `test.support` network helpers the cluster
imports (`socket_helper` gaps), checked-in test certs, and a measured rewrite
of every touched `expectations.toml` row. Re-grade `test_asyncio`'s networked
subset now that loopback + TLS work; split per-submodule expectations where
the package can't grade as a unit.

## Measured targets

The commit-acceptance bar is flipping these rows to `pass`:

| Cluster | Target rows (→ `pass`) |
|---|---|
| WS2 TLS | `test_ssl` |
| WS3 http | `test_httplib` |
| WS4 urllib/cookies | `test_urlparse`, `test_http_cookies`, `test_http_cookiejar`, `test_urllib` |
| WS5 clients | `test_ftplib`, `test_poplib`, `test_imaplib`, `test_smtplib` |
| WS1 harness | `test_socketserver`, `test_socket` (loopback subset) |

Rows that prove deeper than estimated are rewritten to a measured `reason`
and deferred rather than expanding the commit.

## Measured outcome

As-landed result of a fresh `weavepy regrtest --mode subprocess -j 1 --check`
sweep (the harness default resource set is now the WS1 `network,subprocess`;
`cpu`/`walltime`/`decimal`/`tzdata` are intentionally *not* enabled — they gate
slow, host-sensitive stress cases the checked-in baseline was calibrated to
skip, and turning them on only surfaces pre-existing, non-networking
failures/timeouts unrelated to this wave):

**Full sweep: 227 total — 180 pass / 32 fail / 13 skip / 2 timeout — 1
unexpected.** The single unexpected row is `test_multiprocessing_forkserver`
(RFC 0040), a pre-existing flake: it passes in ~105 s on both this branch and a
clean `HEAD` build, but its fork-server shutdown occasionally deadlocks under a
long sweep (accumulated leaked semaphores/dangling processes from earlier
`multiprocessing` rows) and then hits the 600 s cap. It is outside this wave and
not a regression (verified by an apples-to-apples `HEAD`-binary comparison).

Network-cluster rows, as graded:

| Row | As-landed | Note |
|---|---|---|
| `test_httplib` (WS3) | **pass** | verbatim `http.client`/`http.server` over the WS1 `makefile` + WS2 `ssl` |
| `test_urlparse` (WS4) | **pass** | `SplitResult`/`ParseResult` are real `tuple` subclasses |
| `test_http_cookies` (WS4) | **pass** | `Morsel`/`SimpleCookie` output parity |
| `test_http_cookiejar` (WS4) | **pass** | `CookieJar`/`MozillaCookieJar` + policy machinery shipped |
| `test_urllib` (WS4) | **pass** | full `OpenerDirector` handler chain |
| `test_ftplib` (WS5) | **pass** | line driver over `makefile` + loopback TLS |
| `test_poplib` (WS5) | **pass** | |
| `test_imaplib` (WS5) | **pass** | |
| `test_smtplib` (WS5) | **pass** | |
| `test_socketserver` (WS1) | **pass** | loopback `-u network` enabled |
| `test_ssl` (WS2) | **skip (measured)** | TLS *implemented + proven* (client+server `wrap_socket` over an existing fd — exercised by the five protocol clients above all round-tripping over loopback TLS — plus the socketless `MemoryBIO`/`SSLObject` rustls path: `MemoryBIOTests` 5/5, `SSLObjectTests` 2/2). Deferred surfaces are RFC non-goals: OpenSSL `getpeercert()` X.509→dict parsing, the OpenSSL cipher-string grammar, exact options/verify-flag bitmasks, and SNI servername callbacks (rustls, not OpenSSL). |
| `test_socket` (WS1) | **skip (measured)** | loopback subset *implemented + proven* (TCP/UDP connect/bind/listen/accept/send/recv on `127.0.0.1` + `socket.makefile()`; `test_socketserver` + the five clients + `test_net_makefile.py` all round-trip). Deferred: `sendmsg`/`recvmsg` ancillary data (SCM_RIGHTS), IPv6 cmsg, SCTP, `os.sendfile` — platform-specific, out of the loopback-subset scope. |
| `test_urllib2` (WS4) | **skip (measured)** | urllib2-style handlers need live network + `requires_subprocess` shapes unverified in the sandbox |
| `test_asyncio` (WS6) | **skip (measured)** | graded as a 31-submodule package incl. SSL/subprocess/unix transports unavailable in the sandbox; the networked selector subset works (`test_selectors` passes) |
| `test_email` | **fail (expected)** | the `email.policy`/`headerregistry` tail is named Future work (line 275); `email.mime.*` (this wave's WS5 deliverable) is proven by `test_net_email_mime.py` |

Bundled in-process fixtures (WS6), one per workstream, all **pass**:
`test_net_makefile.py` (WS1), `test_net_tls_roundtrip.py` + `test_net_bio_roundtrip.py`
(WS2), `test_net_http_roundtrip.py` (WS3), `test_net_urlparse.py` +
`test_net_cookies.py` + `test_net_cookiejar.py` (WS4), `test_net_email_mime.py`
(WS5).

`cargo build --workspace`, `cargo fmt --check`, and
`cargo clippy --workspace --all-targets -- -D warnings` are all green.

## Non-goals / Drawbacks

- **Windows IOCP/proactor stays out of scope.** The POSIX selector path is
  faithful; the Windows arms keep their existing `NotImplementedError`.
- **TLS is rustls, not OpenSSL.** `ssl.OPENSSL_VERSION`-style probes and the
  OpenSSL-specific cipher-string grammar are emulated, not byte-identical;
  tests that assert exact OpenSSL internals are rewritten to a measured
  `reason` and deferred.
- **Live internet access is not required.** All grading is loopback +
  checked-in certs; no test depends on reaching an external host.
- **Breadth.** Six workstreams across the `ssl` core, the HTTP/URL stack, and
  five protocol clients is a lot of surface. Capped by the per-workstream
  fixtures and the `--check` gate; anything that doesn't land green is
  deferred with a measured reason rather than expanding the commit.

## Alternatives

- **Keep the `_https` one-shot shim and document the gap.** Lowest effort,
  but it can't express `wrap_socket(existing_sock)`, server-side TLS, or the
  non-blocking `SSLWant*` surface `asyncio`/`test_ssl` assert. Rejected.
- **Port `_ssl` over OpenSSL via FFI.** Byte-faithful, but adds a heavyweight
  C dependency and a build/vendoring burden; `rustls` is already linked and
  safe. We emulate the OpenSSL-shaped surface instead.
- **Hand-maintain the HTTP/URL shims.** Rejected for the same reason every
  prior wave rejected it: the suite probes CPython behaviour directly, so the
  verbatim port is both smaller to maintain and measurably more correct.

## Prior art

- **CPython 3.13** — every decision tracks it: the `_ssl`/`ssl.py` split, the
  `http.client` `makefile` model, the `urllib` handler chain, and the
  protocol-client line drivers.
- **PyPy** ships CPython's `ssl.py`/`http`/`urllib`/`ftplib`/… essentially
  verbatim over its own `_ssl`/`_socket`, confirming the "port the driver,
  implement the primitive" split.
- **RFC 0017/0023/0039/0040** — the in-tree foundation this wave finishes.

## Future work

- **Windows process/network model** (IOCP, proactor event loop).
- **Full `email` package** verbatim (the `policy`/`headerregistry` tail) to
  close `test_email`.
- **Live-internet grading** (opt-in `-u urlfetch`) for the external-host
  subset currently skipped by design.
