//! Canonical frozen-table contents (RFC 0060).
//!
//! One source of truth for the module rows that appear in the exported
//! `_PyImport_Frozen{Bootstrap,Stdlib,Test}` C arrays (defined in
//! `weavepy-capi::frozen_table`, which depends on this crate) and in
//! `_imp._frozen_module_names()`. `test_ctypes.test_values.test_frozentable`
//! asserts the two views agree.

/// `(dotted name, is_package)` rows grouped like the exported tables:
/// bootstrap, stdlib, test.
pub const FROZEN_TABLE_NAMES: (&[(&str, bool)], &[(&str, bool)], &[(&str, bool)]) = (
    // Bootstrap: WeavePy's import machinery is native Rust; the frozen
    // bootstrap table carries only its terminator.
    &[],
    // Stdlib: frozen through `FrozenSource` registration, not this table.
    &[],
    // Test: the CPython-verbatim frozen fixture modules (RFC 0057 WS3).
    &[
        ("__hello__", false),
        ("__hello_alias__", false),
        ("__phello_alias__", true),
        ("__phello_alias__.spam", false),
        ("__phello__", true),
        ("__phello__.ham", true),
        ("__phello__.ham.eggs", false),
        ("__phello__.spam", false),
    ],
);

/// All frozen-table module names in table order (bootstrap, stdlib, test).
pub fn frozen_module_names() -> Vec<&'static str> {
    let (boot, std_, test) = FROZEN_TABLE_NAMES;
    boot.iter()
        .chain(std_)
        .chain(test)
        .map(|(name, _)| *name)
        .collect()
}
