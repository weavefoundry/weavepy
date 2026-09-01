# PY314-GAP: the measured CPython 3.14 gap analysis

**RFC 0076 WS13.** This document is the charter for the version-switch
wave (RFC 0077, committed by the version policy RFC 0076 adopts): the
enumerated delta between WeavePy's 3.13 surface and CPython 3.14,
grounded in **one measured sweep** of 3.14's own test suite — not in
release notes. No 3.14 expectations baseline is committed; the sweep
is a measurement, not a gate.

## How this was measured

- **Tree**: CPython **3.14.7** `Lib/` vendored at
  `vendor/cpython314/Lib` (gitignored, like the 3.13 tree; re-fetch
  with `curl -L https://github.com/python/cpython/archive/refs/tags/v3.14.7.tar.gz`
  and extract `cpython-3.14.7/Lib`).
- **Sweep** (2026-08-29, macOS aarch64, release build):

  ```sh
  weavepy-conformance --report-dir target/conformance-314 regrtest \
      --cpython-dir vendor/cpython314/Lib/test --all-cpython \
      --no-check --mode subprocess --jobs 8 --stream
  ```

- **Result**: **467 labels** scheduled from the 3.14 tree —
  **177 pass / 287 fail / 2 timeout / 1 skip** (38% green with a
  3.13-target engine, zero crash-class failures observed).
- **One structural caveat, itself part of the gap**: WeavePy *bundles*
  a 3.13-era `test.support` package in its runtime stdlib, and that
  copy shadows the 3.14 tree's `test/support/` for `import
  test.support`. Labels red only because the bundled support package
  predates 3.14 are enumerated under **support-drift** below — the
  fix (re-vendoring the support package) is mechanical and belongs to
  the switch wave, but it is a real cost and is counted, not excused.

## The delta by category

### 1. Grammar / tokenizer

| Change | Status in WeavePy |
|---|---|
| PEP 750 template strings (`t"…"`, `string.templatelib`) | **Landed this wave behind `-X lang=next`** (RFC 0076 WS15); the switch wave flips the default and deletes the flag. `test_tstring` red by default, by design. |
| PEP 758 parenthesis-free `except A, B:` / `except* A, B:` | **Landed this wave behind `-X lang=next`**; same default-flip in the switch wave. |
| PEP 649/749 deferred annotations (`__annotate__`, `annotationlib`, `VALUE_WITH_FAKE_GLOBALS`) | **Not started — the switch wave's largest single item.** Changes observable 3.13 semantics (class/function annotation evaluation moves to lazy `__annotate__` thunks), so it cannot land behind a preview flag without forking the conformance baseline. Reds: `test_annotationlib`, `test_type_annotations`, and shares of `test_typing` / `test_dataclasses` / `test_functools` / `test_inspect` / `test_type_params`. |
| f-string grammar refinements (3.14 folds further `test_fstring` additions) | `test_fstring` red on new-syntax legs ("Perhaps you forgot a comma" class). |

### 2. Bytecode / magic / marshal

- **pyc magic: 3571 (3.13) → 3627 (3.14 final).** The 3.14 run of
  bumps is 3600–3627; `MAGIC_NUMBER` also moved out of
  `importlib._bootstrap_external` into C
  (`_imp.pyc_magic_number_token`, `Include/internal/pycore_magic_number.h`).
- **Base opcode delta** (from `Lib/_opcode_metadata.py`, 3.13 vs
  3.14.7, specializations excluded):
  - **Added (15)**: `ANNOTATIONS_PLACEHOLDER`, `BUILD_INTERPOLATION`,
    `BUILD_TEMPLATE` (t-strings), `INSTRUMENTED_END_ASYNC_FOR`,
    `INSTRUMENTED_NOT_TAKEN`, `INSTRUMENTED_POP_ITER`,
    `JUMP_IF_FALSE`, `JUMP_IF_TRUE` (pseudo), `LOAD_COMMON_CONSTANT`,
    `LOAD_FAST_BORROW`, `LOAD_FAST_BORROW_LOAD_FAST_BORROW`,
    `LOAD_SMALL_INT`, `LOAD_SPECIAL`, `NOT_TAKEN`, `POP_ITER`.
  - **Removed (11)**: `BEFORE_ASYNC_WITH`, `BEFORE_WITH` (→
    `LOAD_SPECIAL`), `BINARY_SUBSCR` (folded into `BINARY_OP` /
    `NB_SUBSCR`), `BUILD_CONST_KEY_MAP`, `INSTRUMENTED_RETURN_CONST`,
    `LOAD_ASSERTION_ERROR` (→ `LOAD_COMMON_CONSTANT`), `LOAD_METHOD`,
    `LOAD_SUPER_METHOD`, `LOAD_ZERO_SUPER_ATTR`,
    `LOAD_ZERO_SUPER_METHOD`, `RETURN_CONST`.
  - 68 of 69 surviving base opcodes are **renumbered** (`RESUME`
    moved 149 → 128; `ENTER_EXECUTOR` to 255).
  - `CALL_FUNCTION_EX` always takes a kwargs argument; `END_ASYNC_FOR`
    gains an oparg; while-loop test duplication and genexp
    `all`/`any`/`tuple` shapes changed codegen.
- **What this costs WeavePy**: `cpython_code`'s opcode table, the
  compiler's emission for `with` / `async with` / `assert` /
  const-key maps / method loads, marshal/pyc identity
  (`test_marshal`, `test_compileall`, `test_zipimport` red), and the
  `dis`/`_opcode` mirrors (`test_dis`, `test__opcode`,
  `test_peepholer`, `test_compiler_codegen`, `test_opcodes` red —
  e.g. `'<151>' != 'BINARY_OP_ADD_INT'` is the renumbering measured
  directly).

### 3. Stdlib additions / removals / moves

Top-level delta between the vendored trees (`Lib/` file-set diff):

- **Added**: `annotationlib.py` (PEP 649/749), `compression/`
  (PEP 784 — **WeavePy landed `compression.*` + `compression.zstd`
  this wave**, ungated), `string/` (module → package; gains
  `string.templatelib` for t-strings — **landed this wave**),
  `_py_warnings.py` (warnings moved to a C-accelerated split),
  `_ast_unparse.py` (unparse split out of `ast.py`).
- **Removed/moved**: `string.py` (→ package), `_compression.py`
  (→ `compression._common`).
- **In-module additions the sweep measured directly** (each is one
  first-failure reason on a red label): `complex.from_number` /
  `Fraction.from_number` (`test_complex`, `test_fractions`),
  `heapq.heapify_max` family (`test_heapq`), `fnmatch.filterfalse`
  (`test_fnmatch`), float thousands-separator format specs `'.,_f'`
  (`test_format`, `test_float`), `re` `\z` anchor + `\Z` deprecation
  (`test_re`), `memoryview` PEP 688 subscriptability
  (`test_memoryview`), `getpass._check_echo_char` (`test_getpass`),
  `faulthandler.dump_c_stack` (`test_faulthandler`),
  `sys.flags.thread_inherit_context` + PEP 758-adjacent context
  inheritance for threads (`test_context`), `_codecs._unregister_error`
  (`test_codeccallbacks`), `concurrent.interpreters` (PEP 734 —
  `test_interpreters`, `test__interpreters`, `test__interpchannels`,
  `test_crossinterp`), `asyncio.tools` + the **event-loop-policy
  deprecation refactor** (`_DefaultEventLoopPolicy` /
  `_set_event_loop_policy` renames — all 31 asyncio-family reds),
  `_colorize.Theme` (`test__colorize`, `test_pyrepl` shares),
  `http.server.HTTPSServer` + CLI TLS (`test_httpservers`),
  `unittest`'s new assert methods `assertHasAttr` /
  `assertNotHasAttr` / `assertStartsWith` / `assertEndsWith` /
  `assertIsSubclass` / `assertNotIsSubclass` (55 labels red on this
  alone — the single cheapest big win in the switch wave),
  `test.support` 3.14 helpers (`ensure_lazy_imports`,
  `thread_unsafe`, `run_with_limited_c_stack`, `nomemtest`,
  `is_wasm32`, `requires_specialization_ft`, `skip_wasi_stack_overflow`,
  `skip_emscripten_stack_overflow`, `requires_zstd`, …) — the
  support-drift class.
- **PEP 594 dead batteries**: already absent from 3.13; no new
  removals measured.

### 4. C-API

- **PEP 741 `PyInitConfig` — landed this wave** (RFC 0076 WS14):
  `PyInitConfig_Create/Free`, `PyInitConfig_Set{Int,Str,StrList}`,
  `PyInitConfig_GetError`, `PyInitConfig_HasOption`,
  `Py_InitializeFromInitConfig`, plus the runtime read side
  (`PyConfig_Get`, `PyConfig_GetInt`, `PyConfig_Names`), exercised by
  the `_testembed` twin.
- **Counted for the switch wave** (surfaced by the sweep's red
  labels rather than enumerated from headers): the 3.14
  `_testcapi`/`_testinternalcapi` surface growth
  (`code_offset_to_line`, `get_tracked_heap_size`, …) behind
  `test_capi`/`test_embed`/`test_code` shares; `python3.14`
  dylib/tag identity (`python314.dll`, `libpython3.14`, cp314 wheel
  tags); `Py_mod_gil` is already handled (RFC 0076 WS11). A full
  header-level count belongs to RFC 0077's opening survey.

## First-failure-reason census (every red label)

287 fail + 2 timeout across 467 scheduled labels. Classes are
assigned by first failure reason, in this precedence order; the full
sweep detail is reproducible from the command above.

| class | count | what it is | fix shape |
|---|---|---|---|
| support-drift | 74 | 3.14 test files importing `test.support` helpers the bundled 3.13-era support package lacks (`ensure_lazy_imports`, `thread_unsafe`, `run_with_limited_c_stack`, …) | re-vendor `test/support` at 3.14 (mechanical) |
| unittest-3.14-asserts | 55 | `assertHasAttr` / `assertStartsWith` / `assertIsSubclass` family missing from the bundled `unittest` | add six assert methods (small) |
| asyncio-policy-3.14 | 31 | the 3.14 event-loop-policy deprecation refactor (`_DefaultEventLoopPolicy`, `_set_event_loop_policy`, `_py_all_tasks`, `asyncio.tools`) | port the bundled asyncio to the 3.14 policy split |
| new-module-3.14 | 14 | `annotationlib`, `concurrent.interpreters`, `_colorize.Theme`, `HTTPSServer` and their dependents | per-module ports; `annotationlib` is the PEP 649/749 anchor |
| syntax | 2 | `test_fstring` / `test_tstring` new-grammar legs (t-strings default-on in 3.14) | flip `-X lang=next` default in the switch wave |
| timeout | 2 | `test_configparser`, `test_unpack` exceeded the 60 s budget | re-measure under the switch wave's budgets |
| other | 111 | the long tail enumerated in §3 (per-module 3.14 API additions, bytecode-identity tests, PEP 649 semantic shares, plus some environment-shaped reds: `openpty`, `PermissionError` under sandbox, network-dependent labels) | per-cluster burn-down in RFC 0077 |

Label lists per class (as measured, `test_` prefixes and `.py`
suffixes trimmed in prose but kept here verbatim from the sweep):

- **support-drift (74)**: `test_ast`, `test_atexit`, `test_audit`,
  `test_base64`, `test_build_details`, `test_builtin`, `test_call`,
  `test_class`, `test_cmd`, `test_compile`, `test_contextlib_async`,
  `test_copy`, `test_csv`, `test_descr`, `test_dict`,
  `test_dictviews`, `test_dis`, `test_dynamic`, `test_enum`,
  `test_exception_group`, `test_exceptions`, `test_frame`,
  `test_gettext`, `test_hashlib`, `test_hmac`, `test_import`,
  `test_int`, `test_ioctl`, `test_isinstance`, `test_list`,
  `test_locale`, `test_mimetypes`, `test_mmap`, `test_monitoring`,
  `test_opcache`, `test_operator`, `test_optparse`,
  `test_ordered_dict`, `test_os`, `test_patma`, `test_pickle`,
  `test_pprint`, `test_pstats`, `test_pty`, `test_pydoc`,
  `test_pyexpat`, `test_re`, `test_regrtest`, `test_remote_pdb`,
  `test_repl`, `test_shelve`, `test_shutil`, `test_socket`,
  `test_str`, `test_string`, `test_support`, `test_syntax`,
  `test_sys`, `test_sys_settrace`, `test_tarfile`, `test_termios`,
  `test_thread_local_bytecode`, `test_threading`, `test_tokenize`,
  `test_type_cache`, `test_types`, `test_urllib2`, `test_userdict`,
  `test_venv`, `test_warnings`, `test_weakref`, `test_xml_etree`,
  `test_xml_etree_c`, `test_zipimport_support`
- **unittest-3.14-asserts (55)**: `test__osx_support`, `test_abc`,
  `test_abstract_numbers`, `test_asyncio/test_protocols`,
  `test_baseexception`, `test_binascii`, `test_binop`, `test_buffer`,
  `test_bytes`, `test_bz2`, `test_calendar`, `test_cmd_line`,
  `test_cmd_line_script`, `test_code_module`, `test_collections`,
  `test_compiler_assemble`, `test_contextlib`, `test_dbm`,
  `test_dbm_sqlite3`, `test_deque`, `test_dynamicclassattribute`,
  `test_errno`, `test_fileinput`, `test_genericpath`, `test_gzip`,
  `test_http_cookiejar`, `test_httplib`, `test_mailbox`,
  `test_memoryio`, `test_ntpath`, `test_poplib`, `test_posixpath`,
  `test_property`, `test_pulldom`, `test_pyclbr`, `test_random`,
  `test_rlcompleter`, `test_runpy`, `test_scope`,
  `test_script_helper`, `test_site`, `test_source_encoding`,
  `test_stat`, `test_statistics`, `test_structseq`, `test_tempfile`,
  `test_time`, `test_timeit`, `test_type_comments`,
  `test_urllib2_localnet`, `test_urlparse`, `test_weakset`,
  `test_with`, `test_zipapp`, `test_zoneinfo`
- **asyncio-policy-3.14 (31)**: `test_asyncio/test_base_events`,
  `test_asyncio/test_buffered_proto`, `test_asyncio/test_context`,
  `test_asyncio/test_eager_task_factory`, `test_asyncio/test_events`,
  `test_asyncio/test_free_threading`, `test_asyncio/test_futures`,
  `test_asyncio/test_futures2`, `test_asyncio/test_graph`,
  `test_asyncio/test_pep492`, `test_asyncio/test_proactor_events`,
  `test_asyncio/test_runners`, `test_asyncio/test_selector_events`,
  `test_asyncio/test_sendfile`, `test_asyncio/test_server`,
  `test_asyncio/test_sock_lowlevel`, `test_asyncio/test_ssl`,
  `test_asyncio/test_sslproto`, `test_asyncio/test_staggered`,
  `test_asyncio/test_streams`, `test_asyncio/test_subprocess`,
  `test_asyncio/test_taskgroups`, `test_asyncio/test_tasks`,
  `test_asyncio/test_threads`, `test_asyncio/test_timeouts`,
  `test_asyncio/test_tools`, `test_asyncio/test_transports`,
  `test_asyncio/test_unix_events`, `test_asyncio/test_waitfor`,
  `test_coroutines`, `test_pdb`
- **new-module-3.14 (14)**: `test__colorize`, `test__interpchannels`,
  `test_annotationlib`, `test_dataclasses`, `test_functools`,
  `test_httpservers`, `test_inspect`, `test_interpreters`,
  `test_reprlib`, `test_traceback`, `test_type_annotations`,
  `test_type_params`, `test_typing`, `test_wsgiref`
- **syntax (2)**: `test_fstring`, `test_tstring`
- **timeout (2)**: `test_configparser`, `test_unpack`
- **other (111)**: `test__interpreters`, `test__opcode`,
  `test_apple`, `test_argparse`, `test_asyncgen`,
  `test_asyncio/test_locks`, `test_asyncio/test_queues`, `test_capi`,
  `test_cmath`, `test_code`, `test_codeccallbacks`, `test_codecs`,
  `test_codeop`, `test_compileall`, `test_compiler_codegen`,
  `test_complex`, `test_concurrent_futures`, `test_contains`,
  `test_context`, `test_cprofile`, `test_crossinterp`, `test_ctypes`,
  `test_datetime`, `test_decimal`, `test_difflib`, `test_doctest`,
  `test_email`, `test_embed`, `test_faulthandler`, `test_file`,
  `test_float`, `test_fnmatch`, `test_format`, `test_fractions`,
  `test_ftplib`, `test_future_stmt`, `test_gc`, `test_generators`,
  `test_genericalias`, `test_genexps`, `test_getopt`,
  `test_getpass`, `test_getpath`, `test_glob`, `test_grammar`,
  `test_graphlib`, `test_grp`, `test_heapq`, `test_htmlparser`,
  `test_http_cookies`, `test_imaplib`, `test_importlib`, `test_io`,
  `test_ipaddress`, `test_json`, `test_linecache`,
  `test_listcomps`, `test_logging`, `test_lzma`, `test_marshal`,
  `test_math`, `test_memoryview`, `test_module`,
  `test_multibytecodec`, `test_multiprocessing_forkserver`,
  `test_multiprocessing_main_handling`,
  `test_multiprocessing_spawn`, `test_nturl2path`, `test_opcodes`,
  `test_openpty`, `test_pathlib`, `test_peepholer`,
  `test_perf_profiler`, `test_pickletools`, `test_platform`,
  `test_positional_only_arg`, `test_posix`, `test_pwd`,
  `test_pyrepl`, `test_robotparser`, `test_sax`, `test_set`,
  `test_signal`, `test_smtplib`, `test_sqlite3`, `test_ssl`,
  `test_string_literals`, `test_strtod`, `test_struct`,
  `test_subprocess`, `test_super`, `test_symtable`,
  `test_sys_setprofile`, `test_sysconfig`, `test_textwrap`,
  `test_tomllib`, `test_trace`, `test_tty`, `test_type_aliases`,
  `test_ucn`, `test_unicodedata`, `test_unittest`, `test_unparse`,
  `test_urllib`, `test_urllib2net`, `test_urllibnet`, `test_uuid`,
  `test_webbrowser`, `test_xmlrpc`, `test_zipfile`, `test_zipimport`

Notes on individual labels:

- `test_zstd` reported **skip** ("`_zstd` is not a built-in module"):
  WeavePy's PEP 784 implementation (RFC 0076 WS15) ships
  `compression.zstd` over a Rust backend rather than a CPython-shaped
  `_zstd` extension module; the 3.14 test imports `_zstd` directly.
  The bundled `test_zstd` fixture (adapted) grades the surface
  in-tree instead.
- New 3.14-only test files (vs the 3.13 tree): `test_annotationlib`,
  `test_build_details` (PEP 739), `test_crossinterp`,
  `test_nturl2path`, `test_remote_pdb` (PEP 768),
  `test_thread_local_bytecode`, `test_tstring`, `test_zstd`;
  removed: `test_dict_version` (dict versioning ended),
  `test_string` (→ `test_string/` package).

## The switch-wave charter (RFC 0077), in measured order

1. **Mechanical un-shadowing** (129 labels): re-vendor the bundled
   `test.support` at 3.14 and add the six new `unittest` assert
   methods. Nearly half the red count, near-zero risk.
2. **Bytecode/magic flip**: magic 3627, the ±15/−11 opcode delta,
   renumbering, `LOAD_SPECIAL`-shaped `with` codegen, marshal/pyc
   identity. Gates `test_dis`/`test_marshal`/`test_compileall`/…
3. **asyncio policy refactor** (31 labels): one coherent port of the
   bundled asyncio to the 3.14 policy-deprecation split.
4. **PEP 649/749 deferred annotations**: `annotationlib`,
   `__annotate__` thunks, `ANNOTATIONS_PLACEHOLDER` — the deepest
   semantic item, scheduled behind the mechanical tiers.
5. **Long-tail per-module APIs** (§3's in-module additions), each a
   small, testable fix.
6. **Identity flip**: `sys.version` 3.14, cp314 tags,
   `libpython3.14` naming, t-strings/PEP 758 default-on and
   `-X lang=next` deleted, `weavepy-3.13` maintenance branch cut.
