# WeavePy profile census: pickle_bench (work=500, 4s, WEAVEPY_JIT=0)

interpreter-thread samples: 2460

| samples | share | symbol |
|---|---|---|
| 320 | 13.0% | `<weavepy_vm::Interpreter>::run_until_yield_or_return_impl` |
| 178 | 7.2% | `<weavepy_vm::Interpreter>::step` |
| 105 | 4.3% | `core::ptr::drop_glue::<weavepy_vm::object::Object>` |
| 79 | 3.2% | `_xzm_xzone_malloc_tiny` |
| 69 | 2.8% | `_xzm_free` |
| 67 | 2.7% | `<weavepy_vm::object::Object as core::clone::Clone>::clone` |
| 59 | 2.4% | `<weavepy_vm::Interpreter>::dispatch_call` |
| 53 | 2.2% | `<weavepy_vm::Interpreter>::run_frame` |
| 49 | 2.0% | `_tlv_get_addr` |
| 48 | 2.0% | `<weavepy_vm::gc_trace::GcState>::handle_for` |
| 40 | 1.6% | `weavepy_vm::gc_trace::note_dropped` |
| 37 | 1.5% | `_platform_memmove` |
| 34 | 1.4% | `<alloc::vec::Vec<weavepy_vm::object::Object>>::push_mut` |
| 34 | 1.4% | `_malloc_zone_malloc` |
| 33 | 1.3% | `<weavepy_vm::Interpreter>::load_fast_value` |
| 29 | 1.2% | `<weavepy_vm::Interpreter>::call` |
| 26 | 1.1% | `<weavepy_vm::Interpreter>::push_frame_shell` |
| 26 | 1.1% | `weavepy_vm::builtins::dict_lookup` |
| 25 | 1.0% | `_free` |
| 25 | 1.0% | `_xzm_xzone_malloc` |
| 24 | 1.0% | `<deduplicated_symbol>` |
| 22 | 0.9% | `<weavepy_vm::Frame>::pop` |
| 22 | 0.9% | `<weavepy_vm::Interpreter>::call_python_owned` |
| 22 | 0.9% | `<weavepy_vm::sync::GilCell<core::option::Option<alloc::sync::Arc<weavepy_vm::object::PyFrame>>>>::borrow` |
| 19 | 0.8% | `<core::hash::sip::Hasher<core::hash::sip::Sip13Rounds> as core::hash::Hasher>::write` |
