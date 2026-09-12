# Datetime component baseline

Twelve components isolate construction, arithmetic, properties, formatting, and ISO parsing on the retained e910f2dd release. Each performs 2,000 operations after a workload-sized warmup. Seven alternating paired samples compare JIT, interpreted, and CPython execution outside the tool filesystem sandbox. Process elapsed time, CPU, and peak RSS are actual OS measurements. Values are checked before and after warmup in separate CPython, JIT, interpreted, and GIL-disabled processes. There is no preceding-build comparison or new optimization in this baseline.

The source snapshot retains 134 sources and five scripts, including the previously unchanged _pydatetime.py. Both loaded constructor filenames resolve to source bytes matching the repository. The measured executable is identified by its SHA-256 in every report.

| Component | Mode | Work time/CPython | Process time/CPython | CPU/CPython | Peak RSS/CPython | Work CPU/CPython |
|---|---|---:|---:|---:|---:|---:|
| datetime_construct | jit | 51.9002 | 3.2714 | 3.4563 | 2.2594 | 52.1699 |
| datetime_construct | interp | 51.8405 | 3.2130 | 3.3829 | 1.8902 | 51.9409 |
| timedelta_construct | jit | 7.5322 | 2.0804 | 2.1806 | 2.1540 | 7.5495 |
| timedelta_construct | interp | 7.4829 | 1.9655 | 2.0584 | 1.8865 | 7.4899 |
| datetime_add | jit | 931.6054 | 9.8927 | 10.6983 | 2.2936 | 944.3404 |
| datetime_add | interp | 884.9158 | 9.4479 | 10.3247 | 1.8954 | 900.1149 |
| datetime_subtract | jit | 142.0506 | 2.8786 | 3.0584 | 2.1689 | 143.2111 |
| datetime_subtract | interp | 136.6491 | 2.7057 | 2.8640 | 1.8985 | 137.2472 |
| timedelta_multiply | jit | 21.6139 | 2.2765 | 2.3823 | 2.0437 | 21.6657 |
| timedelta_multiply | interp | 21.8184 | 2.1867 | 2.2880 | 1.8865 | 21.8736 |
| datetime_fields | jit | 29.2707 | 2.4382 | 2.5800 | 2.0782 | 29.3789 |
| datetime_fields | interp | 21.5694 | 2.1076 | 2.2115 | 1.8916 | 21.5930 |
| datetime_weekday | jit | 62.3007 | 2.0737 | 2.1890 | 2.0553 | 63.8056 |
| datetime_weekday | interp | 35.4110 | 1.7752 | 1.8541 | 1.8923 | 36.1250 |
| datetime_isoformat | jit | 64.8502 | 8.1628 | 8.9159 | 2.0606 | 64.9236 |
| datetime_isoformat | interp | 64.7952 | 8.0603 | 8.7510 | 1.8901 | 65.5545 |
| datetime_strftime | jit | 15.0160 | 4.5802 | 4.8690 | 2.0553 | 15.0216 |
| datetime_strftime | interp | 13.6183 | 4.1318 | 4.3817 | 1.8904 | 13.6270 |
| datetime_fromisoformat | jit | 817.5199 | 15.8060 | 17.2546 | 2.3062 | 826.8135 |
| datetime_fromisoformat | interp | 714.9404 | 14.0057 | 15.3317 | 1.9103 | 718.7056 |
| date_construct | jit | 61.4495 | 2.5274 | 2.6724 | 2.1314 | 61.8696 |
| date_construct | interp | 60.0283 | 2.4324 | 2.5456 | 1.8972 | 60.3711 |
| date_arithmetic | jit | 84.9940 | 4.1439 | 4.5332 | 2.1709 | 85.3607 |
| date_arithmetic | interp | 83.7126 | 3.9542 | 4.2832 | 1.8994 | 83.1410 |

## Profiles

Every component has a separate JIT-counter run and a five-second CPU sample capture after three workload-sized warmups. No compilation or competing benchmarks run during measurement or profiling. The CLI main thread waits for the weavepy-main worker; its __ulock_wait samples are not worker CPU hotspots. Sample physical-footprint fields are diagnostic and must not replace the paired OS peak-RSS measurements.

Addition and ISO parsing have the largest isolated workload gaps, about 932 and 818 times CPython with JIT enabled. Construction is about 52 times CPython for datetime and 7.5 times for timedelta. CPU samples show extensive interpreter dispatch and call-frame work, with allocation/free costs also visible. Slot initialization is one contributor; this evidence does not justify attributing the entire gap to slot storage.

The JIT compiles small property getters and ASCII-digit callbacks, but compilation does not remove the surrounding Python parsing and construction work. Some components, including weekday and property access, take longer with JIT enabled than in interpreted mode. All counters, rejection traces, sample stacks, and raw timing data remain available. GIL-disabled correctness does not establish free-threaded performance.
