# Checkpoint sampling

Baseline: `4177e85`, interpreter mode (`WEAVEPY_JIT=0`), three seconds at a one-millisecond sampling interval with macOS `sample`. Both fixtures ran at work 2,000,000 so the process stayed active for sampling. The deque sample waited for a readiness signal after imports. These are diagnostic profiles, not timed benchmark results.

The main thread waits for the interpreter thread; its `__ulock_wait` samples do not identify a VM bottleneck. The active thread spends substantial time in bytecode dispatch, object ownership, and attribute access.

## deque_ops

```text
Sort by top of stack, same collapsed (when >= 5):
        __ulock_wait  (in libsystem_kernel.dylib)        2476
        <weavepy_vm::Interpreter>::run_until_yield_or_return_impl  (in weavepy-perf-4177e85)        411
        <weavepy_vm::Interpreter>::step  (in weavepy-perf-4177e85)        164
        core::ptr::drop_glue::<weavepy_vm::object::Object>  (in weavepy-perf-4177e85)        161
        <weavepy_vm::object::Object as core::clone::Clone>::clone  (in weavepy-perf-4177e85)        96
        weavepy_vm::object::py_str_hash  (in weavepy-perf-4177e85)        74
        <weavepy_vm::types::PyInstance>::slot_get  (in weavepy-perf-4177e85)        63
        <alloc::vec::Vec<weavepy_vm::object::Object>>::push_mut  (in weavepy-perf-4177e85)        61
        _xzm_free  (in libsystem_malloc.dylib)        59
        _xzm_xzone_malloc_tiny  (in libsystem_malloc.dylib)        57
        <weavepy_vm::Interpreter>::anchors_tracked_child::walk::{closure#0}  (in weavepy-perf-4177e85)        54
        weavepy_vm::object::py_hash_bytes_slice  (in weavepy-perf-4177e85)        51
        _platform_memcmp  (in libsystem_platform.dylib)        46
        <weavepy_vm::Interpreter>::dispatch_call  (in weavepy-perf-4177e85)        45
        <weavepy_vm::Interpreter>::load_fast_value  (in weavepy-perf-4177e85)        45
        weavepy_vm::gc_trace::note_dropped  (in weavepy-perf-4177e85)        40
        <weavepy_vm::Interpreter>::load_attr_instance_default  (in weavepy-perf-4177e85)        38
        <indexmap::inner::Core<weavepy_vm::object::DictKey, weavepy_vm::object::Object>>::get_index_of::<weavepy_vm::object::StrKey>  (in weavepy-perf-4177e85)        37
        <weavepy_vm::Interpreter>::recycle_scratch  (in weavepy-perf-4177e85)        37
        <weavepy_vm::Frame>::pop  (in weavepy-perf-4177e85)        36
        _tlv_get_addr  (in libdyld.dylib)        33
        <indexmap::map::IndexMap<weavepy_vm::object::DictKey, weavepy_vm::object::Object, weavepy_vm::fasthash::FxBuildHasher>>::get_index_of::<weavepy_vm::object::StrKey>  (in weavepy-perf-4177e85)        32
        <weavepy_vm::Interpreter>::specialized_load_attr  (in weavepy-perf-4177e85)        30
        _platform_memmove  (in libsystem_platform.dylib)        30
        <weavepy_vm::types::TypeObject>::lookup_cached  (in weavepy-perf-4177e85)        28
```

## datetime_ops

```text
Sort by top of stack, same collapsed (when >= 5):
        __ulock_wait  (in libsystem_kernel.dylib)        2342
        <weavepy_vm::Interpreter>::run_until_yield_or_return_impl  (in weavepy-perf-4177e85)        421
        <weavepy_vm::Interpreter>::step  (in weavepy-perf-4177e85)        165
        core::ptr::drop_glue::<weavepy_vm::object::Object>  (in weavepy-perf-4177e85)        113
        <alloc::vec::Vec<weavepy_vm::object::Object>>::push_mut  (in weavepy-perf-4177e85)        81
        <weavepy_vm::object::Object as core::clone::Clone>::clone  (in weavepy-perf-4177e85)        66
        _xzm_xzone_malloc_tiny  (in libsystem_malloc.dylib)        60
        _xzm_free  (in libsystem_malloc.dylib)        57
        <weavepy_vm::Interpreter>::load_fast_value  (in weavepy-perf-4177e85)        53
        <weavepy_vm::Interpreter>::specialized_load_global  (in weavepy-perf-4177e85)        48
        <weavepy_vm::gc_trace::GcState>::handle_for  (in weavepy-perf-4177e85)        46
        <weavepy_vm::Interpreter>::run_frame  (in weavepy-perf-4177e85)        41
        _platform_memcmp  (in libsystem_platform.dylib)        39
        <weavepy_vm::Interpreter>::dispatch_call  (in weavepy-perf-4177e85)        37
        weavepy_vm::builtin_types::builtin_types::{closure#0}  (in weavepy-perf-4177e85)        35
        _tlv_get_addr  (in libdyld.dylib)        35
        <weavepy_vm::Frame>::pop  (in weavepy-perf-4177e85)        32
        weavepy_vm::object::py_str_hash  (in weavepy-perf-4177e85)        30
        _platform_memmove  (in libsystem_platform.dylib)        30
        weavepy_vm::object::py_hash_bytes_slice  (in weavepy-perf-4177e85)        28
        weavepy_vm::gc_trace::note_dropped  (in weavepy-perf-4177e85)        27
        _malloc_zone_malloc  (in libsystem_malloc.dylib)        27
        <indexmap::map::IndexMap<weavepy_vm::object::DictKey, weavepy_vm::object::Object, weavepy_vm::fasthash::FxBuildHasher>>::get_index_of::<weavepy_vm::object::StrKey>  (in weavepy-perf-4177e85)        26
        _platform_memset  (in libsystem_platform.dylib)        26
        <weavepy_vm::sync::GilCell<core::option::Option<alloc::sync::Arc<weavepy_vm::object::PyFrame>>>>::borrow  (in weavepy-perf-4177e85)        25
```
