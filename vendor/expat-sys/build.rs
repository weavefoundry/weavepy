fn main() {
    let mut build = cc::Build::new();
    build
        .include("expat-2.6.4/lib")
        .file("expat-2.6.4/lib/xmlparse.c")
        .file("expat-2.6.4/lib/xmlrole.c")
        .file("expat-2.6.4/lib/xmltok.c")
        // CPython's bundled-expat feature set (Modules/expat).
        .define("XML_NS", "1")
        .define("XML_DTD", "1")
        .define("XML_GE", "1")
        .define("XML_CONTEXT_BYTES", "1024")
        // Satisfies expat's compile-time entropy requirement only; every
        // parser is explicitly salted via XML_SetHashSalt (see README).
        .define("XML_POOR_ENTROPY", "1")
        .warnings(false);
    if !cfg!(windows) {
        build.define("HAVE_MEMMOVE", "1");
    }
    build.compile("expat_weavepy");
    println!("cargo:rerun-if-changed=expat-2.6.4/lib");
}
