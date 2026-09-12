# Compact instance storage checkpoint

This stage adds 16-byte atomic hash caches and inline single-slot storage. Its executable is `target/release/weavepy-runtime-compact-instances`, SHA-256 `f88e10c6b55a1d0c779588e8da3c65676e389bdf55371987362e414bd37debb9`, containing 44,100,256 bytes.

The ten focused instance and hash workloads use nine paired cycles against the preceding native-list-cleanup build and CPython. Single-slot allocation uses 0.788 times the preceding peak RSS and 0.950 times the workload time. Larger instances show little RSS change; repeated multi-slot access is about 2 percent slower in this run. Full process RSS remains above CPython in every focused probe.

The Rust checks and compiled-path checks pass. All 172 compatibility checks pass, including the CPython threading, weak-reference, pickle, and I/O suites. The next complete standard-suite census will include the tuple-enumeration change; this archive does not claim a separate complete standard-suite measurement for compact storage alone. The source and environment were captured before tuple-enumeration source edits.
