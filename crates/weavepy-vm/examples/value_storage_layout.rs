//! Print storage sizes without treating layout as a runtime memory measurement.
use std::mem::{align_of, size_of, size_of_val};
use std::sync::Arc;
use weavepy_vm::object::{DictKey, Object, SharedTuple, TupleStorage};
use weavepy_vm::shared_value::{SharedSlice, SharedStr};

fn main() {
    let tuples: Vec<_> = [0, 1, 2, 8, 16, 33]
        .into_iter()
        .map(|len| {
            let value = TupleStorage::from_vec(vec![Object::None; len]);
            serde_json::json!({"length": len, "payload_bytes": size_of_val(&*value)})
        })
        .collect();
    println!(
        "{}",
        serde_json::json!({
            "pointer_bytes": size_of::<usize>(),
            "Object": size_of::<Object>(),
            "Object_alignment": align_of::<Object>(),
            "Option<Object>": size_of::<Option<Object>>(),
            "DictKey": size_of::<DictKey>(),
            "SharedStr": size_of::<SharedStr>(),
            "SharedSlice<u8>": size_of::<SharedSlice<u8>>(),
            "SharedSlice<u32>": size_of::<SharedSlice<u32>>(),
            "SharedTuple": size_of::<SharedTuple>(),
            "Arc<str>": size_of::<Arc<str>>(),
            "Arc<[u8]>": size_of::<Arc<[u8]>>(),
            "Arc<TupleStorage>": size_of::<Arc<TupleStorage>>(),
            "tuples_excluding_arc_reference_counts": tuples,
        })
    );
}
