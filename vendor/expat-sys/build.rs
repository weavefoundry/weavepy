fn main() {
    // Feature macros live in expat-2.6.4/lib/expat_config.h (CPython-style
    // hand-written config). MSVC has no system expat_config.h, so the
    // `#include "expat_config.h"` in xmlparse/xmltok/xmlrole must resolve
    // to our vendored header via the same-dir / -I search path.
    cc::Build::new()
        .include("expat-2.6.4/lib")
        .file("expat-2.6.4/lib/xmlparse.c")
        .file("expat-2.6.4/lib/xmlrole.c")
        .file("expat-2.6.4/lib/xmltok.c")
        .warnings(false)
        .compile("expat_weavepy");
    println!("cargo:rerun-if-changed=expat-2.6.4/lib");
    println!("cargo:rerun-if-changed=expat-2.6.4/lib/expat_config.h");
}
