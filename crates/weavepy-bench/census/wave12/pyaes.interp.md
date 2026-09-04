# WeavePy profile census: pyaes (work=400, 4s, WEAVEPY_JIT=0)

interpreter-thread samples: 3099

| samples | share | symbol |
|---|---|---|
| 408 | 13.2% | `<weavepy_vm::Interpreter>::step` |
| 332 | 10.7% | `<weavepy_vm::Interpreter>::run_until_yield_or_return_impl` |
| 201 | 6.5% | `core::ptr::drop_glue::<weavepy_vm::object::Object>` |
| 135 | 4.4% | `_xzm_free` |
| 131 | 4.2% | `_xzm_xzone_malloc_tiny` |
| 118 | 3.8% | `<alloc::vec::Vec<weavepy_vm::object::Object>>::push_mut` |
| 89 | 2.9% | `<weavepy_vm::object::Object as core::clone::Clone>::clone` |
| 85 | 2.7% | `<weavepy_vm::gc_trace::GcState>::reap_dead_finalizable_locked` |
| 71 | 2.3% | `<weavepy_vm::Interpreter>::load_fast_value` |
| 69 | 2.2% | `<indexmap::map::IndexMap<u64, (alloc::sync::Arc<weavepy_vm::gc_trace::TrackedHandle>, u8)>>::retain::<weavepy_vm::gc_trace::take_dead_suspects::{closure#0}>::{closure#0}` |
| 67 | 2.2% | `<weavepy_vm::Interpreter>::dispatch_binary_op` |
| 66 | 2.1% | `<weavepy_vm::Frame>::pop` |
| 63 | 2.0% | `<weavepy_vm::Interpreter>::dispatch_call` |
| 58 | 1.9% | `_malloc_zone_malloc` |
| 55 | 1.8% | `weavepy_vm::i64_op` |
| 51 | 1.6% | `<deduplicated_symbol>` |
| 46 | 1.5% | `<weavepy_vm::Interpreter>::specialized_binary_op` |
| 46 | 1.5% | `_platform_memmove` |
| 45 | 1.5% | `<weavepy_vm::object::PyIterator>::next_value` |
| 44 | 1.4% | `_tlv_get_addr` |
| 40 | 1.3% | `_free` |
| 40 | 1.3% | `weavepy_vm::binary_op` |
| 36 | 1.2% | `weavepy_vm::builtins::list_append` |
| 34 | 1.1% | `_platform_memset` |
| 30 | 1.0% | `<weavepy_vm::Interpreter>::specialized_load_attr` |
| 29 | 0.9% | `_xzm_xzone_malloc` |
| 29 | 0.9% | `weavepy_vm::dict_view_set_elems` |
| 27 | 0.9% | `<weavepy_vm::Interpreter>::binary_subscr_basic` |
| 26 | 0.8% | `<weavepy_vm::Interpreter>::specialized_for_iter` |
| 26 | 0.8% | `weavepy_vm::metaclass_method` |
| 23 | 0.7% | `<weavepy_vm::Interpreter>::dispatch_binary_op::{closure#5}` |
| 23 | 0.7% | `weavepy_vm::bignum_op` |
| 23 | 0.7% | `weavepy_vm::instance_method` |
| 20 | 0.6% | `weavepy_vm::tier2::note_backedge` |
| 19 | 0.6% | `<alloc::sync::Arc<[weavepy_vm::object::Object]>>::drop_slow` |
| 19 | 0.6% | `<weavepy_vm::Interpreter>::recycle_scratch` |
| 18 | 0.6% | `<weavepy_vm::Interpreter>::maybe_bind` |
| 18 | 0.6% | `<weavepy_vm::gc_trace::GcState>::reap_dead_finalizable_locked::{closure#1}` |
| 17 | 0.5% | `DYLD-STUB$$free` |
| 16 | 0.5% | `<weavepy_vm::Interpreter>::pooled_scratch` |
| 16 | 0.5% | `weavepy_vm::gc_trace::take_dead_suspects` |
| 15 | 0.5% | `<weavepy_vm::Interpreter>::load_attr_inner` |
| 15 | 0.5% | `<weavepy_vm::Interpreter>::specialized_unpack_sequence` |
| 14 | 0.5% | `<weavepy_vm::Interpreter>::specialized_binary_subscr` |
| 14 | 0.5% | `weavepy_vm::builtin_needs_interp` |
| 12 | 0.4% | `<weavepy_vm::Interpreter>::load_attr` |
| 12 | 0.4% | `<weavepy_vm::Interpreter>::reap_dead_subgraph::{closure#0}` |
| 12 | 0.4% | `<weavepy_vm::sync::GilCell<weavepy_vm::object::PyIterator>>::borrow_mut` |
| 12 | 0.4% | `weavepy_vm::code_const_objects` |
| 11 | 0.4% | `DYLD-STUB$$malloc` |
| 10 | 0.3% | `<alloc::vec::Vec<weavepy_vm::object::Object>>::extend_trusted::<core::iter::adapters::cloned::Cloned<core::slice::iter::Iter<weavepy_vm::object::Object>>>` |
| 10 | 0.3% | `<weavepy_vm::object::Object>::as_i64` |
| 10 | 0.3% | `core::ptr::drop_glue::<[weavepy_vm::object::Object]>` |
| 10 | 0.3% | `weavepy_vm::numeric_data_attr` |
| 10 | 0.3% | `weavepy_vm::specialize::record_hit` |
| 9 | 0.3% | `<alloc::vec::Vec<weavepy_vm::object::Object>>::remove` |
| 9 | 0.3% | `<weavepy_vm::Interpreter>::call` |
| 9 | 0.3% | `DYLD-STUB$$_platform_bzero` |
| 9 | 0.3% | `weavepy_vm::builtins::byte_item_value` |
| 8 | 0.3% | `<weavepy_vm::Interpreter>::reap_call_receiver` |
