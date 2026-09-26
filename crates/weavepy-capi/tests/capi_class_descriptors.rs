//! Exercise descriptor evaluation through the exported C attribute APIs.

#[test]
fn class_descriptors_follow_python_lookup() {
    weavepy_capi::force_link();
    std::thread::Builder::new()
        .stack_size(64 * 1024 * 1024)
        .spawn(|| {
            let source = include_str!("../../../tests/regrtest/test_capi_class_descriptors.py");
            let options = weavepy::RunOptions::new("<capi-class-descriptors>");
            if let Err(error) = weavepy::run_source_with_options(source, &options) {
                panic!("{}", error.format(source, "<capi-class-descriptors>"));
            }
        })
        .expect("spawn descriptor test")
        .join()
        .expect("descriptor test panicked");
}
