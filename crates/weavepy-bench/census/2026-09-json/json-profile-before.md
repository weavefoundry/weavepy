# WeavePy profile census: json_bench (work=5000, 4s, WEAVEPY_JIT=0)

interpreter-thread samples: 3164

| samples | share | symbol |
|---|---|---|
| 477 | 15.1% | `_xzm_free` |
| 457 | 14.4% | `_xzm_xzone_malloc_tiny` |
| 175 | 5.5% | `_xzm_xzone_malloc` |
| 170 | 5.4% | `<weavepy_vm::object::Object>::str_codepoints` |
| 145 | 4.6% | `<deduplicated_symbol>` |
| 143 | 4.5% | `_malloc_zone_malloc` |
| 120 | 3.8% | `_free` |
| 114 | 3.6% | `_platform_memmove` |
| 77 | 2.4% | `weavepy_vm::stdlib::json_accel::obj_to_string` |
| 68 | 2.1% | `_platform_memset` |
| 67 | 2.1% | `<alloc::raw_vec::RawVecInner>::finish_grow` |
| 64 | 2.0% | `xzm_realloc` |
| 50 | 1.6% | `core::ptr::drop_glue::<weavepy_vm::object::Object>` |
| 42 | 1.3% | `<weavepy_vm::object::Object>::str_from_codepoints` |
| 41 | 1.3% | `weavepy_vm::stdlib::json_accel::encode_value_impl` |
| 38 | 1.2% | `xzm_malloc_zone_size` |
| 33 | 1.0% | `_tlv_get_addr` |
| 33 | 1.0% | `weavepy_vm::stdlib::json_accel::json_encode_basestring_ascii` |
| 30 | 0.9% | `<alloc::vec::Vec<(usize, char)> as alloc::vec::spec_from_iter_nested::SpecFromIterNested<(usize, char), core::str::iter::CharIndices>>::from_iter` |
| 29 | 0.9% | `weavepy_vm::stdlib::json_accel::scan_string` |
| 28 | 0.9% | `<alloc::raw_vec::RawVecInner<_>>::reserve::do_reserve_and_handle::<alloc::alloc::Global>` |
| 27 | 0.9% | `weavepy_vm::stdlib::json_accel::scan_once` |
| 26 | 0.8% | `<indexmap::inner::Core<weavepy_vm::object::DictKey, weavepy_vm::object::Object>>::insert_full` |
| 26 | 0.8% | `<weavepy_vm::object::Object as core::clone::Clone>::clone` |
| 26 | 0.8% | `_realloc` |
