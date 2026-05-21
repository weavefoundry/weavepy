# RFC 0017: The OS and networking interface

- **Status**: Accepted
- **Authors**: WeavePy authors
- **Created**: 2026-05-21
- **Tracking issue**: TBD

## Summary

Close the gap between "modern Python — the async half of the
language — runs" (post RFC 0016) and "modern Python — the I/O half
of the ecosystem — runs." After this RFC lands:

- The runtime gains a real **socket** layer: `socket.socket(...)`,
  `bind`, `listen`, `accept`, `connect`, `send`, `recv`, blocking
  *and* non-blocking, TCP / UDP / Unix-domain, IPv4 and IPv6, name
  resolution via `getaddrinfo` / `gethostbyname`, the full set of
  `SOL_SOCKET` / `IPPROTO_*` options Python user code reaches for.
- A real **TLS** layer ships under `ssl`: client and server
  contexts backed by `rustls`, certificate loading, hostname
  verification, ALPN. Modern TLS only — no SSLv2/SSLv3/TLS-1.0/1.1
  — matching CPython 3.13's "secure by default" stance.
- A real **selectors** primitive lands: `selectors.DefaultSelector`,
  `KqueueSelector` / `EpollSelector` / `PollSelector` on the
  appropriate platforms, plus `select.select` / `select.poll` /
  `select.kqueue` / `select.epoll`. The implementation is backed
  by `mio`.
- A real **subprocess** module ships: `Popen` with `stdin` /
  `stdout` / `stderr` redirection, `PIPE` / `DEVNULL` /
  `STDOUT` sentinels, `communicate`, `wait`, `poll`, `kill`,
  `terminate`, `send_signal`, `run`, `check_output`,
  `check_call`, `getoutput`, `CalledProcessError` /
  `TimeoutExpired`.
- A pragmatic **signal** module — `SIGINT` / `SIGTERM` / `SIGHUP` /
  `SIGUSR1` / etc., `signal.signal(...)`, `signal.getsignal(...)`,
  `signal.alarm` (POSIX), with the asyncio loop hooked up to
  honour Ctrl-C cleanly.
- Filesystem helpers: `tempfile` (NamedTemporaryFile,
  TemporaryDirectory, mkstemp, mkdtemp, gettempdir), `glob`,
  `fnmatch`, `shutil` (copy/copy2/copytree/rmtree/move/which/disk_usage/get_terminal_size),
  expanded `os` (listdir, mkdir, makedirs, rmdir, remove,
  rename, walk, stat, fstat, lstat, scandir, urandom, getpid,
  getuid, popen, system).
- A frozen pure-Python **`urllib`** package — `urllib.parse`,
  `urllib.request`, `urllib.error`, `urllib.response` — on top of
  sockets and TLS.
- Frozen **`http`** package — `http`, `http.client`,
  `http.server`, `http.cookies`, `http.cookiejar` — plus
  `socketserver` and a tiny `email` parser big enough for the HTTP
  modules' header handling.
- Cryptographic primitives: `hashlib` (md5/sha1/sha224/sha256/
  sha384/sha512/blake2b/blake2s, plus `pbkdf2_hmac` and the
  HMAC-shaped `hmac` companion), `secrets`, `uuid`, `base64`,
  `binascii`.
- Misc: `csv` (Rust-backed for speed + correctness), `mimetypes`
  (frozen), `ipaddress` (frozen), `zlib` (Rust, flate2-backed).
- **Asyncio I/O integration**: the event loop now drives a real
  selector. `loop.add_reader` / `add_writer` / `remove_reader` /
  `remove_writer` work. `asyncio.open_connection`,
  `asyncio.start_server`, `asyncio.create_subprocess_exec`,
  `asyncio.create_subprocess_shell` all return live
  `StreamReader` / `StreamWriter` pairs.
- A frozen **`asyncio.streams`** layer covers `StreamReader`,
  `StreamWriter`, `BaseProtocol`, `Protocol`, the readers'
  buffered-line / framed-read API.

The combination delivers what the project calls "Option A" in
the roadmap: drop in `import requests` (well, `urllib.request`),
`subprocess.run(["git", "status"])`, or a 5-line
`asyncio.start_server(...)` HTTP echo server, and have it work.

## Motivation

After RFC 0016 WeavePy could *parse and execute* `async` /
`await` / `async for` / `async with` and ship an asyncio event
loop. But the loop had **nothing to multiplex** — no sockets, no
pipes, no subprocesses. `asyncio.open_connection(...)` raised
`AttributeError` (the function didn't exist), and even simple
synchronous code like `subprocess.run(["echo", "hi"])` failed at
`import subprocess`.

That left an awkward middle ground:

- Coroutines existed but had nothing to suspend on except
  `asyncio.sleep`.
- The vast majority of "real" Python is networked. The web
  framework story, the database story, the HTTP client story —
  every one of them is the OS interface dressed up.
- "Drop-in CPython replacement" was technically true for a
  fizzbuzz program but false for any program a real user would
  type into a fresh checkout.

Down-tree, this RFC unblocks:

- A useful `asyncio` — `asyncio.run(main())` where `main` actually
  fetches an URL.
- An HTTP server in a one-liner (`python -m http.server`).
- A shell-out story (`subprocess.run(["git", "log", "--oneline"])`).
- The next-tier RFCs that depend on having OS facilities to
  observe: test runners (`unittest`'s parallel runner uses
  subprocess), package managers (`pip` shells out to `git` and
  `tar`), CLI tools.
- A real Ctrl-C story.

## CPython reference

This RFC tracks **CPython 3.13**:

- **`socket`** — `Modules/socketmodule.c` plus the language
  reference's "socket — Low-level networking interface" chapter.
  The address family / socket-type / protocol constants are
  reproduced under their CPython names (`AF_INET`, `SOCK_STREAM`,
  etc.). `getaddrinfo`'s flag set follows POSIX and CPython's
  surface.
- **`ssl`** — PEP 543 (TLS API redesign — partially adopted in
  3.10+) and the `Lib/ssl.py` source. We track the *secure*
  surface: `SSLContext`, `wrap_socket`, `PROTOCOL_TLS_CLIENT` /
  `PROTOCOL_TLS_SERVER`, hostname verification on by default.
- **`select` / `selectors`** — `Lib/selectors.py` and
  `Modules/selectmodule.c`. The kqueue / epoll / poll backends
  match the documented surface.
- **`subprocess`** — `Lib/subprocess.py`. We follow the full
  surface (`Popen`, `run`, `PIPE`, `DEVNULL`, `STDOUT`,
  `CalledProcessError`, `TimeoutExpired`) and the convenience
  wrappers (`check_output`, `check_call`, `getoutput`,
  `getstatusoutput`).
- **`signal`** — `Lib/signal.py`. `signal.signal`,
  `signal.getsignal`, the constants, `signal.alarm` (POSIX), the
  default-handler / ignore sentinels.
- **`tempfile`** — `Lib/tempfile.py`. `NamedTemporaryFile`,
  `TemporaryDirectory`, `mkstemp`, `mkdtemp`, `gettempdir`.
- **`glob` / `fnmatch`** — `Lib/glob.py`, `Lib/fnmatch.py`. Same
  metacharacters, same semantics on `[...]`, same `recursive=True`
  for `**`.
- **`shutil`** — `Lib/shutil.py`. Copy semantics match (preserves
  mode for `copy2`, preserves only data for `copyfile`).
- **`urllib`** — `Lib/urllib/parse.py`, `Lib/urllib/request.py`,
  `Lib/urllib/error.py`, `Lib/urllib/response.py`. We reproduce
  the public surface (`urlopen`, `urlretrieve`, `Request`,
  `urlencode`, `quote`, `unquote`, `urlparse`).
- **`http`** — `Lib/http/client.py`, `Lib/http/server.py`,
  `Lib/http/cookies.py`. Same status code table, same default
  request/response semantics.
- **`socketserver`** — `Lib/socketserver.py`. `TCPServer`,
  `UDPServer`, the `ThreadingMixIn` / `ForkingMixIn` (we
  collapse threading-mixin to in-process under RFC 0016's
  cooperative model — see Drawbacks).
- **`hashlib`** — `Lib/hashlib.py` plus `Modules/_hashopenssl.c`.
  We back the hashes with RustCrypto (`sha2`, `sha1`, `md-5`,
  `blake2`).
- **`hmac`** — `Lib/hmac.py`.
- **`secrets`** — `Lib/secrets.py`.
- **`uuid`** — `Lib/uuid.py`.
- **`base64`** — `Lib/base64.py`.
- **`binascii`** — `Modules/binascii.c`.
- **`csv`** — `Modules/_csv.c`.
- **`mimetypes`** — `Lib/mimetypes.py`.
- **`ipaddress`** — `Lib/ipaddress.py`.
- **`zlib`** — `Modules/zlibmodule.c`.

We deliberately do **not** track in this RFC:

- **`asyncio.subprocess`'s full transport hierarchy.** We ship
  `create_subprocess_exec` / `create_subprocess_shell` returning a
  `Process` with `.stdin`/`.stdout`/`.stderr` streams and the
  `.communicate(input=...)` convenience. The deep
  `SubprocessTransport` / `WriteSubprocessTransport` hierarchy
  CPython exposes for custom protocols is approximate.
- **`ssl.SSLObject` (memory BIO mode).** We do TLS over real
  sockets; the BIO-decoupled mode used by some async frameworks
  for custom I/O is not exposed.
- **`socket.if_*` interface enumeration**, `socket.sethostname`,
  the platform-specific knobs (`SO_BINDTODEVICE`, `TCP_FASTOPEN`,
  `IP_TRANSPARENT`). The common path is wired; the long tail of
  platform-specific options is not.
- **`urllib.robotparser`.** Rarely needed; trivial follow-up.
- **`xmlrpc`, `wsgiref`, `cgi`**, the older networked-stdlib
  surface.
- **`asyncio.SSL` for `start_tls`-after-connect.** TLS sockets
  work through `ssl.wrap_socket` on a plain socket; the deep
  protocol-upgrade mid-stream is not yet wired through asyncio's
  transport layer.
- **`hashlib.new("ripemd160", ...)` / `shake_128` /
  `shake_256`.** The fixed-output hashes that everyone uses are
  here; the streaming-output variants are a follow-up.
- **CPython's `gzip`, `bz2`, `lzma`.** We ship `zlib` (the
  underlying engine) but not the higher-level file wrappers in
  this slice.

## Detailed design

### Crate-by-crate scope

#### `weavepy-vm` (Rust-side modules)

| Module | Source file | LOC (approx.) |
|--------|-------------|--------------:|
| `socket` | `stdlib/socket.rs` | 1900 |
| `ssl` | `stdlib/ssl.rs` | 900 |
| `select` | `stdlib/select.rs` | 350 |
| `selectors` (Rust-side helpers; surface is frozen) | shared with `select` | — |
| `subprocess` (Rust core) | `stdlib/subprocess.rs` | 800 |
| `signal` | `stdlib/signal.rs` | 380 |
| `tempfile` (Rust core) | `stdlib/tempfile.rs` | 300 |
| `glob` / `fnmatch` | `stdlib/glob.rs`, `stdlib/fnmatch.rs` | 300 |
| `shutil` core helpers | `stdlib/shutil.rs` | 400 |
| `os` extensions | `stdlib/os.rs` (extended) | +500 |
| `hashlib` | `stdlib/hashlib.rs` | 350 |
| `hmac` (companion) | `stdlib/hmac_mod.rs` | 200 |
| `base64` | `stdlib/base64_mod.rs` | 220 |
| `binascii` | `stdlib/binascii_mod.rs` | 220 |
| `secrets` (Rust shim) | `stdlib/secrets_mod.rs` | 130 |
| `zlib` | `stdlib/zlib_mod.rs` | 250 |
| `csv` | `stdlib/csv_mod.rs` | 380 |

#### Frozen Python modules

| Module | Source file | LOC (approx.) |
|--------|-------------|--------------:|
| `selectors` (Python surface) | `stdlib/python/selectors.py` | 280 |
| `subprocess` (Python wrapper) | `stdlib/python/subprocess.py` | 450 |
| `tempfile` (Python wrapper) | `stdlib/python/tempfile.py` | 200 |
| `shutil` (Python wrapper) | `stdlib/python/shutil.py` | 250 |
| `urllib` (package) | `stdlib/python/urllib_*.py` | 1200 |
| `http` (package) | `stdlib/python/http_*.py` | 1100 |
| `socketserver` | `stdlib/python/socketserver.py` | 280 |
| `email` (lite) | `stdlib/python/email_lite.py` | 200 |
| `mimetypes` | `stdlib/python/mimetypes.py` | 250 |
| `ipaddress` | `stdlib/python/ipaddress.py` | 600 |
| `uuid` | `stdlib/python/uuid.py` | 220 |

#### Asyncio integration

| Patch | File | LOC (approx.) |
|-------|------|--------------:|
| Real `_select` driving `_ready` | `stdlib/python/asyncio.py` | +200 |
| `add_reader` / `add_writer` / `remove_reader` | as above | +100 |
| `open_connection` / `start_server` / `Server` | new section in `asyncio.py` | +250 |
| `create_subprocess_exec` / `create_subprocess_shell` / `Process` | as above | +200 |
| `streams` (`StreamReader` / `StreamWriter`) | as above | +250 |

#### Total

~13.4K LOC Rust, ~6.4K LOC frozen Python, ~1K LOC of asyncio
integration, plus fixtures (~1.5K LOC) and the small lift in
`builtins`/`object`/`builtin_types` for new exception classes
(~500 LOC). Net diff ≈ **23–28K LOC**.

### Object model

Three new variants under `Object`:

```rust
pub enum Object {
    // ... existing variants ...
    Socket(Rc<PySocket>),
    Selector(Rc<PySelector>),
    SubprocessHandle(Rc<PySubprocessHandle>),
}

pub struct PySocket {
    pub fd: RefCell<Option<RawSocket>>,
    pub family: i32,
    pub kind: i32,
    pub proto: i32,
    pub timeout: Cell<SocketTimeout>,
    pub blocking: Cell<bool>,
    pub tls: RefCell<Option<TlsState>>,
}

pub struct PySelector {
    pub kind: SelectorKind,
    pub registry: RefCell<HashMap<i64, SelectorEntry>>,
}

pub struct PySubprocessHandle {
    pub child: RefCell<Option<std::process::Child>>,
    pub pid: i32,
    pub returncode: Cell<Option<i32>>,
    pub stdin: RefCell<Option<Object>>,
    pub stdout: RefCell<Option<Object>>,
    pub stderr: RefCell<Option<Object>>,
}
```

Sockets are represented as small wrapper structs around the
underlying `mio::net::*` or `socket2::Socket` handle. We use
`socket2::Socket` as the primary low-level type because it
exposes every option Python's `socket.setsockopt` reaches for.

The `TlsState` enum wraps a `rustls` connection. TLS is layered
*on top of* a plain socket — `ssl.wrap_socket(sock, ...)`
returns the same socket object with `tls` populated, and the
socket's `send` / `recv` paths consult `tls` first.

### New exception classes

```text
BaseException
└── Exception
    ├── OSError                       (existing)
    │   ├── BlockingIOError           (existing)
    │   ├── ConnectionError           (existing)
    │   │   ├── BrokenPipeError       (new)
    │   │   ├── ConnectionAbortedError (new)
    │   │   ├── ConnectionRefusedError (new)
    │   │   └── ConnectionResetError  (new)
    │   ├── FileExistsError           (new)
    │   ├── FileNotFoundError         (existing)
    │   ├── IsADirectoryError         (new)
    │   ├── NotADirectoryError        (new)
    │   ├── PermissionError           (new)
    │   └── TimeoutError              (new — distinct from asyncio.TimeoutError)
    ├── ssl.SSLError                  (new, in ssl module dict)
    └── subprocess.SubprocessError    (new, in subprocess module dict)
        ├── CalledProcessError        (new)
        └── TimeoutExpired            (new, shadows OSError.TimeoutError name)
```

`socket.error` aliases `OSError` (CPython 3.3+).

`subprocess.SubprocessError` / `CalledProcessError` /
`TimeoutExpired` live in the `subprocess` module's globals
rather than the global exception namespace — matching CPython.

### `socket` module

The Rust core (`stdlib/socket.rs`) exposes a `socket.socket`
factory returning a `PySocket` wrapping a `socket2::Socket`. The
methods we implement:

| Method | Behaviour |
|--------|-----------|
| `socket(family=AF_INET, type=SOCK_STREAM, proto=0, fileno=None)` | Constructor. |
| `bind(address)` | Binds to `(host, port)` or `socketaddr_in`. |
| `listen(backlog=128)` | Marks server side. |
| `accept()` | Returns `(conn, addr)` tuple. |
| `connect(address)` | Blocks (or raises `BlockingIOError` if non-blocking). |
| `connect_ex(address)` | Returns errno-style int. |
| `send(data, flags=0)` | Returns bytes written. |
| `sendall(data, flags=0)` | Loops `send` until done. |
| `sendto(data, address)` / `sendto(data, flags, address)` | UDP. |
| `recv(bufsize, flags=0)` | Returns bytes. |
| `recv_into(buffer, nbytes=0, flags=0)` | Returns count. |
| `recvfrom(bufsize, flags=0)` | Returns `(bytes, addr)`. |
| `setsockopt(level, optname, value)` | Wraps `setsockopt`. |
| `getsockopt(level, optname, buflen=0)` | Wraps `getsockopt`. |
| `setblocking(flag)` | Bool. |
| `settimeout(value)` | Float or None. |
| `gettimeout()` | Float or None. |
| `getsockname()` | Local address. |
| `getpeername()` | Remote address. |
| `fileno()` | Returns the underlying fd as an int. |
| `close()` | Drops the underlying socket. |
| `shutdown(how)` | `SHUT_RD` / `SHUT_WR` / `SHUT_RDWR`. |
| `makefile(mode='r', buffering=None, encoding=None, ...)` | Returns a `PyFile` over the socket. |
| `detach()` | Releases the fd. |

Module-level functions: `gethostname`, `gethostbyname`,
`gethostbyaddr`, `getaddrinfo`, `getnameinfo`, `socketpair`,
`create_connection`, `create_server`, `inet_aton`, `inet_ntoa`,
`inet_pton`, `inet_ntop`, `getservbyname`, `getservbyport`,
`if_nameindex` (best effort), `has_ipv6`, `htons`, `htonl`,
`ntohs`, `ntohl`.

Constants — all of the AF_* / SOCK_* / SOL_* / IPPROTO_* / SO_* /
TCP_* / IP_* / MSG_* / SHUT_* / NI_* / AI_* names CPython exposes
on the host platform. On non-POSIX hosts, the POSIX-only names
are simply absent from the module dict (matching CPython).

### `ssl` module

Backed by `rustls`. The surface tracks `Lib/ssl.py` for the
*secure* subset:

```python
ssl.create_default_context(purpose=Purpose.SERVER_AUTH, ...)
ssl.SSLContext(protocol=PROTOCOL_TLS_CLIENT)
ctx.wrap_socket(sock, server_side=False, do_handshake_on_connect=True,
                suppress_ragged_eofs=True, server_hostname=None,
                session=None)
ctx.load_cert_chain(certfile, keyfile=None, password=None)
ctx.load_verify_locations(cafile=None, capath=None, cadata=None)
ctx.set_alpn_protocols(['h2', 'http/1.1'])
ctx.check_hostname  # bool
ctx.verify_mode     # CERT_NONE / CERT_OPTIONAL / CERT_REQUIRED
```

The `SSLContext` carries a `rustls::ClientConfig` or
`rustls::ServerConfig`. `wrap_socket` mutates the socket's
`tls: RefCell<Option<TlsState>>` to install a fresh
`rustls::ClientConnection` / `ServerConnection`. Reads and
writes route through the connection's `read_tls` /
`write_tls` / `process_new_packets` cycle.

We deliberately ship only `PROTOCOL_TLS_CLIENT` /
`PROTOCOL_TLS_SERVER` / `PROTOCOL_TLS` (alias for the first
two depending on context). Legacy `PROTOCOL_SSLv23`,
`PROTOCOL_TLSv1`, etc. are absent — they're deprecated upstream
and `rustls` doesn't speak them anyway.

System CA roots load through `webpki-roots` (Mozilla's CA list
embedded in the binary). `load_verify_locations(cafile=...)`
parses PEM with `rustls-pemfile`.

### `selectors` and `select`

`select.select(rlist, wlist, xlist, timeout=None)` is implemented
in Rust over `mio::Poll` — we register each fd with the
appropriate interest, poll for at most `timeout` seconds, then
return the per-list "ready" subsets. For platforms where
`mio` falls back to a `select(2)` wrapper, we match that.

`selectors.DefaultSelector` is a frozen Python class with a
`_kqueue` / `_epoll` / `_poll` backend selection done at import
time. Internally it consults a Rust-side `PySelector` for the
actual `mio::Poll` (so user code doesn't lose registrations
across asyncio's loop step).

`select.poll()`, `select.kqueue()`, `select.epoll()` return
thin wrappers around the same `PySelector` machinery, with the
right CPython-shaped constants (`POLLIN` / `POLLOUT` /
`POLLERR` / `POLLHUP` / etc.).

### `subprocess`

The Rust side (`stdlib/subprocess.rs`) is tiny — it wraps
`std::process::Command` and exposes the spawn primitives. The
*user-visible* `Popen` class lives in the frozen Python wrapper
(`stdlib/python/subprocess.py`) on top:

```python
class Popen:
    def __init__(self, args, bufsize=-1, executable=None,
                 stdin=None, stdout=None, stderr=None,
                 cwd=None, env=None, shell=False,
                 universal_newlines=None, text=None, encoding=None,
                 timeout=None):
        ...
    def communicate(self, input=None, timeout=None): ...
    def wait(self, timeout=None): ...
    def poll(self): ...
    def kill(self): ...
    def terminate(self): ...
    def send_signal(self, sig): ...
```

The wrapper calls into `_subprocess.spawn(...)` (the Rust
core) which returns the `PySubprocessHandle`, plus the
appropriate read/write file objects for each redirected pipe.

`PIPE` / `DEVNULL` / `STDOUT` are sentinels in the
`subprocess` module dict. Mode handling, encoding, line-
buffering, and the byte/str dichotomy live in Python.

`run(args, ...)`, `check_output`, `check_call`, `getoutput`,
`getstatusoutput` are convenience wrappers.

### `signal`

Rust side:

```rust
pub struct SignalState {
    /// User-installed handlers keyed by signal number.
    handlers: RefCell<HashMap<i32, Object>>,
    /// Pending signal numbers received but not yet dispatched.
    pending: RefCell<VecDeque<i32>>,
}
```

`signal.signal(signum, handler)` installs `handler` (a Python
callable, `SIG_DFL`, or `SIG_IGN`). The Rust side uses
`signal-hook` to register a low-level handler that pushes the
signal number into `pending` (which is an atomic-flavoured
queue — single-threaded but with `signal-hook`'s safe
publication semantics).

The VM consults `pending` on every interpreter tick (lazy: only
between bytecode batches). When a signal is pending and a
Python handler is installed, the VM dispatches it.

Constants: every `SIG*` name present on the host platform —
on macOS we include the BSD subset, on Linux the full POSIX +
RT range. On Windows we ship a smaller set (`SIGINT`,
`SIGTERM`, `SIGBREAK`).

`SIGINT` is wired through to asyncio: when raised on a running
loop, the loop's next `_run_once` raises `KeyboardInterrupt`
into the topmost task.

### Filesystem extensions to `os`

`os.listdir`, `os.mkdir`, `os.makedirs`, `os.rmdir`, `os.remove`
(`os.unlink`), `os.rename`, `os.replace`, `os.walk`, `os.stat`,
`os.fstat`, `os.lstat`, `os.scandir`, `os.urandom`, `os.getpid`,
`os.getuid` (POSIX), `os.geteuid` (POSIX), `os.getgid` (POSIX),
`os.chmod`, `os.chown` (POSIX), `os.symlink`, `os.readlink`,
`os.utime`, `os.open` / `os.close` / `os.read` / `os.write` /
`os.lseek`, `os.devnull`, `os.kill`, `os.popen`, `os.system`.

`os.stat` returns a tuple-shaped object exposing both index
access (`st[0]`) and attribute access (`st.st_size`). For
attribute access we use a small frozen-Python wrapper that
constructs a `_StatResult` instance on top of the
Rust-returned tuple.

### `tempfile`, `glob`, `fnmatch`, `shutil`

`tempfile` is split: Rust core (`mkstemp`, `mkdtemp`,
`gettempdir`, `gettempprefix`) plus a frozen Python wrapper
that adds `NamedTemporaryFile`, `TemporaryDirectory`,
`SpooledTemporaryFile`, and the context-manager wrappers.

`glob` and `fnmatch` are entirely Rust (the pattern matching
is hot-path-y and the pattern surface is small). `glob.glob` /
`glob.iglob` walk via `os.scandir`; recursion (`**`) is
honoured with `recursive=True`.

`shutil` is split: Rust helpers (`shutil._copyfileobj`,
`shutil._rmtree`, `shutil._copytree`) plus a frozen Python
wrapper for the user-visible API.

### Crypto and encoding modules

- `hashlib` — Rust core wrapping `sha2`, `sha1`, `md-5`,
  `blake2`, with a unified `Hasher` object exposing
  `update(data)`, `digest()`, `hexdigest()`,
  `digest_size`, `block_size`, `name`, `copy()`.
  `pbkdf2_hmac(name, password, salt, iterations, dklen=None)`
  runs entirely in Rust.
- `hmac` — wraps `hashlib` for the keyed variant.
  `hmac.new(key, msg=None, digestmod='sha256')`,
  `hmac.digest(key, msg, digest)`, `hmac.compare_digest`.
- `secrets` — Rust shim that pulls from `OsRng` (via
  `rand_core`). `token_bytes`, `token_hex`, `token_urlsafe`,
  `choice`, `randbelow`, `compare_digest`.
- `uuid` — frozen Python on top of `os.urandom` for `uuid4`,
  Rust core for the version-1/3/5 variants and the bit
  fiddling.
- `base64` — Rust core for the four common encodings (`b64`,
  `b32`, `b16`, `b85`) and the URL-safe variants.
- `binascii` — Rust core for `b2a_hex` / `a2b_hex` /
  `b2a_base64` / `a2b_base64` / CRC32 / hexlify / unhexlify.
- `zlib` — Rust core via `flate2`. `compress` / `decompress` /
  `compressobj` / `decompressobj`, plus the streaming API
  exposed via the user-visible flush mode constants.

### `urllib`, `http`, `socketserver`

All four ship as **frozen Python** on top of the Rust modules
above. The implementations are intentional rewrites — smaller
than CPython's — covering what real programs reach for:

- `urllib.parse`: `urlparse`, `urlunparse`, `urljoin`, `urldefrag`,
  `urlencode`, `quote`, `unquote`, `quote_plus`, `unquote_plus`,
  `quote_from_bytes`, `unquote_to_bytes`, `parse_qs`, `parse_qsl`,
  `ParseResult`, `SplitResult`.
- `urllib.request`: `urlopen`, `Request`, `OpenerDirector`,
  `build_opener`, `HTTPHandler`, `HTTPSHandler`, `HTTPRedirectHandler`,
  `HTTPBasicAuthHandler`. `urlopen` returns a file-like
  `HTTPResponse` object on top of an `http.client.HTTPConnection`.
- `urllib.error`: `URLError`, `HTTPError`, `ContentTooShortError`.
- `urllib.response`: `addinfourl`, `addbase`, `addclosehook`.
- `http.client`: `HTTPConnection`, `HTTPSConnection`,
  `HTTPResponse`, the request/header helpers, the
  `HTTPException` hierarchy.
- `http.server`: `HTTPServer`, `BaseHTTPRequestHandler`,
  `SimpleHTTPRequestHandler`. Enough for `python -m http.server`.
- `http.cookies`: `SimpleCookie`, `Morsel`.
- `socketserver`: `TCPServer`, `UDPServer`, the *MixIn* classes
  (collapsed cooperative under our threading story),
  `BaseRequestHandler`, `StreamRequestHandler`,
  `DatagramRequestHandler`.

### `email` (lite)

A small parser sufficient for HTTP's header bag and basic MIME
header value parsing. `email.message.Message`, `email.parser`,
`email.utils.formatdate` / `parsedate_to_datetime`. The full
multi-part email surface (`email.mime.*`, `email.policy`,
`email.contentmanager`) is out of scope.

### `csv`, `mimetypes`, `ipaddress`

- `csv` is Rust-backed because the dialect machinery is fiddly
  and frequently called. `reader`, `writer`, `DictReader`,
  `DictWriter`, `Sniffer.has_header`, `Sniffer.sniff`,
  `excel`/`excel_tab`/`unix` dialects, `QUOTE_*` constants.
- `mimetypes` is frozen Python with a builtin extension map
  (the common ~120 entries).
- `ipaddress` is frozen Python: `IPv4Address`, `IPv6Address`,
  `IPv4Network`, `IPv6Network`, `IPv4Interface`,
  `IPv6Interface`, `ip_address`, `ip_network`, `ip_interface`,
  the operator overloads.

### Asyncio integration

The frozen `asyncio.py` gains a fourth section between the
event loop and the synchronisation primitives:

```python
# ---- I/O integration -----------------------------------------

# Real reader/writer registry — keyed by fd. Each value is a
# `_FdCallbacks(reader=cb_or_None, writer=cb_or_None)`.
# `_run_once` consults this between draining `_ready` and
# parking on the next deadline: if there's nothing to do
# immediately, we hand the unused timeout to a real selector.

class _FdCallbacks: ...

# The loop owns a single `_selectors.DefaultSelector()` instance.
# `add_reader(fd, cb, *args)` registers EVENT_READ;
# `add_writer(fd, cb, *args)` registers EVENT_WRITE;
# `remove_reader(fd)` clears.

# ---- streams ---------------------------------------------------

class StreamReader: ...      # buffered read with line/exact APIs
class StreamWriter: ...      # transport.write + drain coalescing
class _SocketTransport: ...  # bridges a `socket.socket` to the loop

# ---- connection helpers ----------------------------------------

async def open_connection(host=None, port=None, *, ssl=None, **kw): ...
async def start_server(client_connected_cb, host=None, port=None, **kw): ...

# ---- subprocess helpers ----------------------------------------

class Process: ...
async def create_subprocess_exec(*args, stdin=None, stdout=None, stderr=None, **kw): ...
async def create_subprocess_shell(cmd, **kw): ...
```

`_run_once` is patched: when `_ready` is empty *and* `_fd_callbacks`
is non-empty, we park on `_selector.select(timeout)` and dispatch
ready callbacks before returning. The deadline-only mode (sleep
until next timer) is preserved when no fds are registered, so
existing async code paths see no regression.

### Errors that map to Python exceptions

| Rust error | Python exception |
|------------|------------------|
| `io::ErrorKind::NotFound` | `FileNotFoundError` |
| `io::ErrorKind::PermissionDenied` | `PermissionError` |
| `io::ErrorKind::AddrInUse` | `OSError` (errno EADDRINUSE) |
| `io::ErrorKind::ConnectionRefused` | `ConnectionRefusedError` |
| `io::ErrorKind::ConnectionReset` | `ConnectionResetError` |
| `io::ErrorKind::ConnectionAborted` | `ConnectionAbortedError` |
| `io::ErrorKind::BrokenPipe` | `BrokenPipeError` |
| `io::ErrorKind::TimedOut` | `TimeoutError` |
| `io::ErrorKind::WouldBlock` | `BlockingIOError` |
| `io::ErrorKind::Interrupted` | `InterruptedError` |
| `io::ErrorKind::AlreadyExists` | `FileExistsError` |

All other variants fall through to `OSError(errno, message)`.

## Drawbacks

- **Cooperative-only threading still in force.** `ThreadingMixIn`
  in `socketserver` runs handlers in-process under our single-
  thread model. A high-concurrency server using
  `ThreadingHTTPServer` will not actually parallelise. Honest
  about WeavePy's stage; lifted by the `Rc → Arc` refactor.
- **No `asyncio.SSL` start_tls mid-stream.** TLS sockets work
  via `ssl.wrap_socket` (or by passing `ssl=ctx` to
  `open_connection`); upgrading an existing async stream to TLS
  in-place is not supported. The CPython `loop.start_tls`
  surface is absent.
- **rustls only.** No OpenSSL backend, no legacy TLS, no
  obscure cipher suites. Some real-world endpoints (older
  servers, custom intermediate CAs distributed via OpenSSL-
  shaped `cafile`s) won't connect. The PEM-loading path is
  here; the user is expected to provide modern certificates.
- **`subprocess` on Windows is best-effort.** We use
  `std::process::Command` which handles the cross-platform
  basics. The deeper Windows-specific surface (job objects,
  `creationflags`, `startupinfo`) is approximate.
- **No `multiprocessing`.** Out of scope; the `_thread` story
  still serializes Python execution. `multiprocessing`'s
  process-based parallelism would technically work
  (`subprocess` is shipped) but the IPC machinery
  (`multiprocessing.Queue`, `Pipe`, `Manager`) is not.
- **`select.kqueue` / `epoll` aren't full ports of CPython's
  surface.** Both are exposed as `mio`-backed wrappers; the
  long tail of CPython-specific quirks (e.g. `kevent.fflags`
  for vnode events) is not.
- **No `cgi`, `http.server.CGIHTTPRequestHandler`, `wsgiref`.**
  Out of scope; users who need CGI today can ship their own.
- **No `xmlrpc`, `xml.etree`, `xml.sax`, `xml.dom`.** XML
  processing is a separate domain; out of scope.
- **`socket.socket.fileno()` returns the underlying raw fd**,
  but our `os.read` / `os.write` do not see a "real fd" the way
  CPython's do — they see what `os` produced. Mixing
  `socket.fileno()` into `os.write(fd, ...)` works on POSIX
  hosts where we hand back the OS fd; on Windows the fd is a
  socket handle (compatible at the system level, observable
  divergence at the Python level).
- **`hashlib.new("md5", ...)` is supported, but the
  `usedforsecurity=False` parameter (3.9+) is accepted-and-
  ignored.** We don't ship FIPS-mode handling because we don't
  link OpenSSL.
- **`urllib.request.urlopen` follows redirects through 10 hops
  by default.** No cookie jar is attached by default; for
  cookie-aware fetches, the user builds an opener.

## Alternatives

- **Wrap CPython's actual `Lib/socket.py`, `Lib/ssl.py`,
  `Lib/subprocess.py`.** Rejected: those modules layer on top
  of CPython's C accelerators (`_socket`, `_ssl`,
  `_posixsubprocess`) which we don't have. The pure-Python
  layer we *would* keep is small; the wrappers we'd have to
  re-implement are the bulk of the work either way.
- **Defer TLS.** Tempting — `rustls` adds ~3 MB to the binary
  and is the largest single dependency. Rejected because
  almost every real-world Python network use today is HTTPS,
  not HTTP.
- **Implement subprocess in Rust directly without the Python
  wrapper.** Rejected: the wrapper layer (mode handling,
  encoding, `communicate`, `run` ergonomics) is substantial
  Python that CPython itself ships in Python. We follow the
  precedent.
- **Use a pure-Rust selector implementation (no `mio`).**
  Rejected: `mio` is mature, cross-platform, and well-tested.
  Re-implementing kqueue/epoll abstractions in WeavePy is
  pointless reinvention.
- **Ship `multiprocessing` here.** Rejected for scope reasons.
  Requires either a real `fork()` story (POSIX-only) or
  `spawn`-with-pickling, both of which want their own RFC.

## Prior art

- **CPython 3.13** — the conformance target. `Lib/socket.py`,
  `Lib/ssl.py`, `Lib/subprocess.py`, the `urllib` and `http`
  packages.
- **RustPython** — similar trajectory; their `socket` is also a
  thin wrapper around `socket2`, and they wrap `rustls` for
  TLS. Their `subprocess` is closer to CPython's.
- **PyPy** — ships a near-complete socket / ssl / subprocess
  surface via cffi against the host C libraries; we don't have
  that machinery so we use Rust libraries instead.
- **MicroPython** — `usocket`, `ussl`, `usubprocess` are all
  much smaller subsets. Useful comparison for "the minimum
  viable Python OS interface."
- **GraalPy** — runs CPython's exact stdlib via Truffle's C
  emulation. Closest to "wear CPython's behaviour" but at the
  cost of a giant JVM-flavoured runtime.

## Unresolved questions

- **IPv6 scope-id parsing.** `socket.getaddrinfo("fe80::1%en0",
  ...)` works for the common case (scope id passed through to
  the OS resolver). The CPython parser's full ABNF is not
  exercised.
- **`ssl.SSLContext.set_ciphers(...)`** is accepted but partly
  ignored: rustls picks a fixed safe cipher suite, and we don't
  expose the underlying cipher table. Code that hard-codes
  cipher suites for compliance reasons will need attention.
- **Async subprocess on Windows.** The
  `create_subprocess_exec` path uses `std::process::Command`
  which works on Windows, but the asyncio-side pipe driving
  (overlap I/O, IOCP) is approximated with a worker that
  polls. Functional, not native.
- **`hashlib.scrypt` / `hashlib.shake_*`.** Listed as future
  work; the streaming-output and memory-hard variants need
  separate dependencies (`scrypt`-the-crate, the SHA3 family
  for shake).
- **Socket buffer reuse between sync and async paths.** A
  `socket.socket` that's passed to `loop.sock_recv(...)` is
  observed at the OS level; if user code mixes blocking
  `recv` and `sock_recv` on the same socket, behaviour is
  defined but slightly different from CPython's. Documented.

## Future work

- **`multiprocessing`** — process-based parallelism on top of
  `subprocess` and a pickling story.
- **`xmlrpc.client` / `xmlrpc.server`** if anyone misses
  them.
- **`asyncio.start_tls` mid-stream** — requires plumbing the
  TLS state machine through the existing transport.
- **`aiohttp`-style ergonomics in the stdlib** — out of scope
  here, but the building blocks exist now.
- **Native Windows async (IOCP-based) loop.** Today we use
  the same `mio`-on-Windows path as cross-platform code; an
  IOCP-native loop would close the perf gap.
- **`ssl.MemoryBIO`** for libraries that want to drive TLS
  without a real socket (some custom protocols do this).
- **`socket.if_nameindex`** with full POSIX interface details
  on Linux/BSD.
- **`logging` integration with `signal.SIGUSR1` log rotation
  patterns** when `logging` lands.
