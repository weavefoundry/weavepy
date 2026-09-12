# Checkpoint profile

Baseline: `9a69c4161ade6527a5f4457d7e7272075cb1f0d9`, interpreter mode.
The pickle fixture ran at work 10,000 to keep it active during a four-second
macOS `sample` capture at a one-millisecond interval. Sampling began at launch,
so initialization can appear. This is a diagnostic profile, not a benchmark.

The main thread waits for the interpreter thread. Its `__ulock_wait` samples
don't identify a VM bottleneck. The active thread spends substantial time in
bytecode dispatch, object cloning and destruction, allocation, and call setup.

```text
__ulock_wait  (in libsystem_kernel.dylib)        2821
        _RNvMs2_CsgYlizD6EG2z_10weavepy_vmNtB5_11Interpreter30run_until_yield_or_return_impl  (in weavepy-perf-9a69c41)        387
        _RNvMs2_CsgYlizD6EG2z_10weavepy_vmNtB5_11Interpreter4step  (in weavepy-perf-9a69c41)        206
        _RINvNtCs8Mbv00yxnRz_4core3ptr9drop_glueNtNtCsgYlizD6EG2z_10weavepy_vm6object6ObjectEBF_  (in weavepy-perf-9a69c41)        151
        _xzm_free  (in libsystem_malloc.dylib)        129
        _RNvXsT_NtCsgYlizD6EG2z_10weavepy_vm6objectNtB5_6ObjectNtNtCs8Mbv00yxnRz_4core5clone5Clone5clone  (in weavepy-perf-9a69c41)        85
        _xzm_xzone_malloc_tiny  (in libsystem_malloc.dylib)        83
        _RNvMsF_NtCshxvaOLs88l5_5alloc3vecINtB5_3VecNtNtCsgYlizD6EG2z_10weavepy_vm6object6ObjectE8push_mutBJ_  (in weavepy-perf-9a69c41)        64
        _RNvMs2_CsgYlizD6EG2z_10weavepy_vmNtB5_11Interpreter9run_frame  (in weavepy-perf-9a69c41)        52
        _tlv_get_addr  (in libdyld.dylib)        52
        _RNvMs2_CsgYlizD6EG2z_10weavepy_vmNtB5_11Interpreter15load_fast_value  (in weavepy-perf-9a69c41)        49
        _RNvNtCsgYlizD6EG2z_10weavepy_vm8gc_trace12note_dropped  (in weavepy-perf-9a69c41)        44
        _RNvMs2_CsgYlizD6EG2z_10weavepy_vmNtB5_11Interpreter13dispatch_call  (in weavepy-perf-9a69c41)        42
        _RNvMs1_NtCsgYlizD6EG2z_10weavepy_vm8gc_traceNtB5_7GcState10handle_for  (in weavepy-perf-9a69c41)        38
        _free  (in libsystem_malloc.dylib)        36
        _platform_memmove  (in libsystem_platform.dylib)        34
        _RNvMs2_CsgYlizD6EG2z_10weavepy_vmNtB5_11Interpreter16push_frame_shell  (in weavepy-perf-9a69c41)        33
        _malloc_zone_malloc  (in libsystem_malloc.dylib)        31
        _RNvMs2_CsgYlizD6EG2z_10weavepy_vmNtB5_11Interpreter19recycle_frame_shell  (in weavepy-perf-9a69c41)        29
        _RNvMs2_CsgYlizD6EG2z_10weavepy_vmNtB5_11Interpreter20recycle_frame_allocs  (in weavepy-perf-9a69c41)        29
        _xzm_xzone_malloc  (in libsystem_malloc.dylib)        29
        _RNvNtCsgYlizD6EG2z_10weavepy_vm8builtins11dict_lookup  (in weavepy-perf-9a69c41)        28
        _RNCINvMs4_NtCskjxBf4UiEDI_8indexmap3mapINtB8_8IndexMapyNtNtCsgYlizD6EG2z_10weavepy_vm8gc_trace7SuspectE6retainNCNvBT_18take_dead_suspects0E0BV_  (in weavepy-perf-9a69c41)        27
        _RNvXs3_NtNtCs8Mbv00yxnRz_4core4hash3sipINtB5_6HasherNtB5_11Sip13RoundsENtB7_6Hasher5writeCsgYlizD6EG2z_10weavepy_vm  (in weavepy-perf-9a69c41)        27
        <deduplicated_symbol>  (in libsystem_malloc.dylib)        26
```
