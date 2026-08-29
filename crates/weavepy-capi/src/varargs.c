/*
 * varargs.c — variadic helpers for the WeavePy C-API.
 *
 * These functions exist in C because Rust on stable does not
 * support receiving `va_list` arguments. The implementations are
 * deliberately tiny: they walk the format string, peel off each
 * unit, and dispatch to a non-variadic Rust helper that does the
 * actual conversion.
 *
 * Format-string compatibility is a strict subset of CPython's
 * documented surface. The supported units are:
 *
 *   PyArg_ParseTuple / PyArg_ParseTupleAndKeywords:
 *     i      → int*
 *     I      → unsigned int*
 *     l      → long*
 *     L      → long long*
 *     n      → Py_ssize_t*
 *     f      → float*
 *     d      → double*
 *     s      → const char**
 *     s#     → const char**, Py_ssize_t*
 *     y      → const char**           (bytes)
 *     y#     → const char**, Py_ssize_t*
 *     O      → PyObject **            (any object, no type check)
 *     O!     → PyTypeObject*, PyObject**  (with type check)
 *     p      → int*                   (boolean)
 *
 *   Format-string control characters:
 *     |      separator: subsequent units are optional
 *     :name  trailing message-context for error reports (parsed but ignored)
 *     ;text  trailing message-context (parsed but ignored)
 *
 *   Py_BuildValue:
 *     i / I / l / L / n   → int family
 *     f / d              → float family
 *     s                  → const char* (str)
 *     s#                 → const char*, Py_ssize_t (str)
 *     y / y#             → bytes
 *     O                  → PyObject*  (steals ref unless 'N' is used)
 *     N                  → PyObject*  (steals ref)
 *     (...)              → tuple
 *     [...]              → list
 *     {...}              → dict (alternating key, value pairs)
 *     z / z#             → str-or-None (None if pointer is NULL)
 *
 *   Py_BuildValue is forgiving — unknown units yield None.
 */

#include "../include/Python.h"

#include <ctype.h>
#include <signal.h>
#include <stdarg.h>
#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

/* --------------------------------------------------------------
 * Debug crash handler (RFC 0046, wave 4).
 *
 * Dumping a native backtrace on SIGSEGV/SIGBUS/SIGABRT is invaluable
 * when a freshly-loaded C extension (e.g. numpy's `_multiarray_umath`
 * `Py_mod_exec`) faults deep inside its own initialiser, where lldb is
 * unavailable. The handler uses async-signal-safe `backtrace*` and then
 * re-raises with the default disposition so the real exit status is
 * preserved. Installed only when `WEAVEPY_CRASH_BT` is set.
 *
 * `execinfo.h`/`backtrace*`, `<unistd.h>`, and signals such as `SIGBUS`
 * are POSIX-only, so on Windows the installer is a no-op that still
 * resolves the `extern` symbol referenced from `interp.rs`.
 * -------------------------------------------------------------- */

#if !defined(_WIN32)

#include <execinfo.h>
#include <unistd.h>
#include <sys/ucontext.h>

/* Async-signal-safe hex writer for the fault diagnostic below. */
static void weavepy_write_hex(const char *label, unsigned long long v) {
    char buf[32];
    int i = 0;
    buf[i++] = ' ';
    static const char hex[] = "0123456789abcdef";
    buf[i++] = '0';
    buf[i++] = 'x';
    for (int shift = 60; shift >= 0; shift -= 4) {
        buf[i++] = hex[(v >> shift) & 0xf];
    }
    buf[i++] = '\n';
    write(2, label, strlen(label));
    write(2, buf, i);
}

static void weavepy_crash_handler_si(int sig, siginfo_t *info, void *ucv) {
    const char *msg = "\n[weavepy] FAULTV2 caught fatal signal; native backtrace:\n";
    write(2, msg, strlen(msg));
    weavepy_write_hex("[weavepy] fault addr:",
                      (unsigned long long)(uintptr_t)(info ? info->si_addr : (void *)0));
    void *frames[512];
    int n = 0;
#if defined(__APPLE__) && defined(__aarch64__)
    if (ucv) {
        ucontext_t *uc = (ucontext_t *)ucv;
        if (uc->uc_mcontext) {
            unsigned long long pc = (unsigned long long)uc->uc_mcontext->__ss.__pc;
            unsigned long long lr = (unsigned long long)uc->uc_mcontext->__ss.__lr;
            unsigned long long fp = (unsigned long long)uc->uc_mcontext->__ss.__fp;
            weavepy_write_hex("[weavepy] pc:", pc);
            weavepy_write_hex("[weavepy] lr:", lr);
            /* Manually walk the arm64 frame-pointer chain from the
             * interrupted context. backtrace() from a signal handler on
             * macOS only sees the handler's own (alt-stack) frames, so to
             * capture the *faulting* stack (e.g. a recursion cycle that
             * overflowed) we chase [fp] = {saved_fp, saved_lr}. */
            frames[n++] = (void *)pc;
            if (lr) frames[n++] = (void *)lr;
            unsigned long long cur = fp;
            unsigned long long prev = 0;
            while (cur && cur > prev && n < 500) {
                unsigned long long next = *(unsigned long long *)cur;
                unsigned long long ret = *(unsigned long long *)(cur + 8);
                if (!ret) break;
                frames[n++] = (void *)ret;
                prev = cur;
                cur = next;
            }
        }
    }
#endif
    if (n == 0) {
        n = backtrace(frames, 512);
    }
    backtrace_symbols_fd(frames, n, 2);
    signal(sig, SIG_DFL);
    raise(sig);
}

/* Alternate signal stack so the handler can run even when the main
 * stack is exhausted (the recursion-driven stack-overflow case). */
static char weavepy_altstack[SIGSTKSZ > 65536 ? SIGSTKSZ : 65536];

void weavepy_install_crash_handler(void) {
    stack_t ss;
    memset(&ss, 0, sizeof(ss));
    ss.ss_sp = weavepy_altstack;
    ss.ss_size = sizeof(weavepy_altstack);
    ss.ss_flags = 0;
    sigaltstack(&ss, NULL);

    struct sigaction sa;
    memset(&sa, 0, sizeof(sa));
    sa.sa_sigaction = weavepy_crash_handler_si;
    sa.sa_flags = SA_SIGINFO | SA_ONSTACK;
    sigemptyset(&sa.sa_mask);
    sigaction(SIGSEGV, &sa, NULL);
    sigaction(SIGBUS, &sa, NULL);
    sigaction(SIGABRT, &sa, NULL);
    sigaction(SIGILL, &sa, NULL);
}

#else /* _WIN32 */

void weavepy_install_crash_handler(void) {}

#endif /* _WIN32 */

/* --------------------------------------------------------------
 * Forward declarations of Rust helpers (matching argparse.rs).
 * -------------------------------------------------------------- */

extern int _WeavePy_Arg_Length(PyObject *args);
extern PyObject *_WeavePy_Arg_Item(PyObject *args, int i);
extern int _WeavePy_Arg_Long(PyObject *arg, long long *dest);
extern int _WeavePy_Arg_Int(PyObject *arg, int *dest);
extern int _WeavePy_Arg_Double(PyObject *arg, double *dest);
extern int _WeavePy_Arg_String(PyObject *arg, const char **dest);
extern int _WeavePy_Arg_StringAndSize(PyObject *arg, const char **dest, Py_ssize_t *len);
extern int _WeavePy_Arg_Object(PyObject *arg, PyObject **dest);
extern int _WeavePy_Arg_Bool(PyObject *arg, int *dest);
extern PyObject *_WeavePy_Kwargs_Pop(PyObject *kwargs, const char *key);
extern int _WeavePy_Kwargs_Len(PyObject *kwargs);
extern const char *_WeavePy_Kwargs_KeyAt(PyObject *kwargs, int i);
extern void _WeavePy_Arg_Tether(PyObject *owner, PyObject *arg);

/* Defined in wave4.rs / strings.rs; the local single-header Python.h
 * doesn't declare these. */
extern PyObject *PySys_GetObject(const char *name);
extern PyObject *PyUnicode_DecodeUTF8(const char *s, Py_ssize_t size,
                                      const char *errors);
extern PyTypeObject PyType_Type;

extern PyObject *_WeavePy_Build_None(void);
extern PyObject *_WeavePy_Build_FromI64(long long v);
extern PyObject *_WeavePy_Build_FromU64(unsigned long long v);
extern PyObject *_WeavePy_Build_FromDouble(double v);
extern PyObject *_WeavePy_Build_FromString(const char *s);
extern PyObject *_WeavePy_Build_FromStringAndSize(const char *s, Py_ssize_t n);
extern PyObject *_WeavePy_Build_FromBytesAndSize(const char *s, Py_ssize_t n);
extern PyObject *_WeavePy_Build_TupleFromArray(Py_ssize_t n, PyObject **items);
extern PyObject *_WeavePy_Build_ListFromArray(Py_ssize_t n, PyObject **items);
extern PyObject *_WeavePy_Build_DictFromArrays(Py_ssize_t n, PyObject **keys, PyObject **values);
extern void _WeavePy_Format_Set(PyObject *ty, const char *msg, Py_ssize_t len);
extern PyObject *_WeavePy_TuplePackFromArray(Py_ssize_t n, PyObject **items);

/* --------------------------------------------------------------
 * Format-string parser shared between PyArg_ParseTuple and
 * PyArg_ParseTupleAndKeywords.
 * -------------------------------------------------------------- */

typedef struct {
    const char *fmt;          /* pointer into the format string */
    bool optional;            /* set once we've crossed `|` */
    int min_count;            /* args required so far */
    int total_count;          /* slots seen so far */
} fmt_state;

static void fmt_init(fmt_state *st, const char *fmt) {
    st->fmt = fmt;
    st->optional = false;
    st->min_count = 0;
    st->total_count = 0;
}

/* Skip over format meta-characters (`:`, `;`, whitespace). The
 * trailing `:funcname` / `;message` are reported in errors but we
 * don't propagate them — yet. */
static void fmt_skip_meta(fmt_state *st) {
    while (*st->fmt) {
        char c = *st->fmt;
        if (c == ' ' || c == '\t') {
            st->fmt++;
            continue;
        }
        if (c == ':' || c == ';') {
            /* Consume the rest of the format string silently. */
            while (*st->fmt) st->fmt++;
            return;
        }
        return;
    }
}

/* Pull one argument from the args tuple at `index`, returning a
 * borrowed reference (caller must Py_DECREF when done). */
static PyObject *fetch_arg(PyObject *args, int index) {
    return _WeavePy_Arg_Item(args, index);
}

/* Nested-sequence group support (CPython `converttuple`). Forward-
 * declared here because `parse_one` and `parse_group` are mutually
 * recursive (a group element may itself be a group). */
static int parse_group(fmt_state *st, PyObject *arg, va_list *ap);
static int count_group_units(const char *p);

/* The `s*`/`z*`/`y*`/`w*` units fill a caller-owned `Py_buffer` through
 * the buffer protocol (CPython's `getbuffer`). Pillow's codec loop is
 * the canonical consumer — `ImagingDecoder.decode` parses its bytes
 * argument with "y*". `s*`/`z*` additionally accept a str (a read-only
 * view over its UTF-8 bytes), `z*` maps None to a NULL buffer, and `w*`
 * demands a writable exporter. The caller releases the view with
 * `PyBuffer_Release`, exactly the CPython contract. */
static int parse_star_buffer(char unit, PyObject *arg, va_list *ap) {
    Py_buffer *view = va_arg(*ap, Py_buffer *);
    if (unit == 'z' && arg == Py_None) {
        return PyBuffer_FillInfo(view, NULL, NULL, 0, 1, PyBUF_SIMPLE);
    }
    if ((unit == 's' || unit == 'z') && PyUnicode_Check(arg)) {
        Py_ssize_t len = 0;
        const char *p = PyUnicode_AsUTF8AndSize(arg, &len);
        if (p == NULL) {
            return -1;
        }
        return PyBuffer_FillInfo(view, arg, (void *)p, len, 1, PyBUF_SIMPLE);
    }
    return PyObject_GetBuffer(arg, view, unit == 'w' ? PyBUF_WRITABLE : PyBUF_SIMPLE);
}

/* Convert a single format unit into the va_arg destination(s).
 * Returns 0 on success, -1 on failure (with an exception set). */
static int parse_one(fmt_state *st, PyObject *arg, va_list *ap) {
    char unit = *st->fmt;
    if (unit == 0) return -1;

    /* A `(...)` group binds to *one* argument that must itself be a
     * sequence; its units are unpacked against that sequence's items
     * (CPython `converttuple`). Extensions lean on this heavily —
     * numpy's `array_setstate` parses its pickle state with
     * `"(iO!O!iO):__setstate__"`, so without group support every
     * ndarray (hence Index/Series/DataFrame) failed to unpickle with
     * "function requires more arguments than were given". */
    if (unit == '(') {
        return parse_group(st, arg, ap);
    }

    /* The 's#'/'y#'/'z#' family takes both a buffer pointer and a length. */
    bool has_len_flag = (st->fmt[1] == '#');
    /* The 's*'/'z*'/'y*'/'w*' family fills a caller-owned `Py_buffer`. */
    bool has_buf_flag = (st->fmt[1] == '*');

    switch (unit) {
        case 'i': {
            int *dest = va_arg(*ap, int *);
            if (_WeavePy_Arg_Int(arg, dest) != 0) return -1;
            st->fmt++;
            return 0;
        }
        case 'I': {
            unsigned int *dest = va_arg(*ap, unsigned int *);
            long long tmp = 0;
            if (_WeavePy_Arg_Long(arg, &tmp) != 0) return -1;
            *dest = (unsigned int)tmp;
            st->fmt++;
            return 0;
        }
        case 'h': {
            short *dest = va_arg(*ap, short *);
            int tmp = 0;
            if (_WeavePy_Arg_Int(arg, &tmp) != 0) return -1;
            *dest = (short)tmp;
            st->fmt++;
            return 0;
        }
        case 'H': {
            unsigned short *dest = va_arg(*ap, unsigned short *);
            long long tmp = 0;
            if (_WeavePy_Arg_Long(arg, &tmp) != 0) return -1;
            *dest = (unsigned short)tmp;
            st->fmt++;
            return 0;
        }
        case 'b': case 'B': {
            unsigned char *dest = va_arg(*ap, unsigned char *);
            int tmp = 0;
            if (_WeavePy_Arg_Int(arg, &tmp) != 0) return -1;
            *dest = (unsigned char)tmp;
            st->fmt++;
            return 0;
        }
        case 'l': {
            long *dest = va_arg(*ap, long *);
            long long tmp = 0;
            if (_WeavePy_Arg_Long(arg, &tmp) != 0) return -1;
            *dest = (long)tmp;
            st->fmt++;
            return 0;
        }
        case 'k': {
            /* unsigned long. numpy's `arraydescr_setstate` parses its
             * pickle state with `"(iOOOOnnkO)"` — the `k` slot is the
             * dtype's `flags` word. A missing case here fell through to
             * `default` WITHOUT consuming the `unsigned long *`, which
             * desynced the trailing `O` (datetime `metadata`) onto the
             * flags pointer and left numpy dereferencing an uninitialised
             * `metadata` — SIGSEGV unpickling any `datetime64`/`timedelta64`
             * dtype (hence every DatetimeIndex/TimedeltaIndex/Period). */
            unsigned long *dest = va_arg(*ap, unsigned long *);
            long long tmp = 0;
            if (_WeavePy_Arg_Long(arg, &tmp) != 0) return -1;
            *dest = (unsigned long)tmp;
            st->fmt++;
            return 0;
        }
        case 'L': case 'q': {
            long long *dest = va_arg(*ap, long long *);
            if (_WeavePy_Arg_Long(arg, dest) != 0) return -1;
            st->fmt++;
            return 0;
        }
        case 'K': case 'Q': {
            unsigned long long *dest = va_arg(*ap, unsigned long long *);
            long long tmp = 0;
            if (_WeavePy_Arg_Long(arg, &tmp) != 0) return -1;
            *dest = (unsigned long long)tmp;
            st->fmt++;
            return 0;
        }
        case 'n': {
            Py_ssize_t *dest = va_arg(*ap, Py_ssize_t *);
            long long tmp = 0;
            if (_WeavePy_Arg_Long(arg, &tmp) != 0) return -1;
            *dest = (Py_ssize_t)tmp;
            st->fmt++;
            return 0;
        }
        case 'f': {
            float *dest = va_arg(*ap, float *);
            double tmp = 0.0;
            if (_WeavePy_Arg_Double(arg, &tmp) != 0) return -1;
            *dest = (float)tmp;
            st->fmt++;
            return 0;
        }
        case 'd': {
            double *dest = va_arg(*ap, double *);
            if (_WeavePy_Arg_Double(arg, dest) != 0) return -1;
            st->fmt++;
            return 0;
        }
        case 's': case 'z': {
            if (has_buf_flag) {
                if (parse_star_buffer(unit, arg, ap) != 0) return -1;
                st->fmt += 2;
                return 0;
            }
            const char **dest = va_arg(*ap, const char **);
            /* `z`/`z#` map None to a NULL pointer (CPython `convertsimple`);
             * Pillow's jpeg encoder parses its optional comment/EXIF tail
             * with "…Oz#y#" and passes None when absent. */
            if (unit == 'z' && arg == Py_None) {
                *dest = NULL;
                if (has_len_flag) {
                    Py_ssize_t *plen = va_arg(*ap, Py_ssize_t *);
                    *plen = 0;
                    st->fmt += 2;
                } else {
                    st->fmt++;
                }
                return 0;
            }
            if (has_len_flag) {
                Py_ssize_t *plen = va_arg(*ap, Py_ssize_t *);
                if (_WeavePy_Arg_StringAndSize(arg, dest, plen) != 0) return -1;
                st->fmt += 2;
            } else {
                if (_WeavePy_Arg_String(arg, dest) != 0) return -1;
                st->fmt++;
            }
            return 0;
        }
        case 'y': {
            if (has_buf_flag) {
                if (parse_star_buffer(unit, arg, ap) != 0) return -1;
                st->fmt += 2;
                return 0;
            }
            const char **dest = va_arg(*ap, const char **);
            if (has_len_flag) {
                Py_ssize_t *plen = va_arg(*ap, Py_ssize_t *);
                if (_WeavePy_Arg_StringAndSize(arg, dest, plen) != 0) return -1;
                st->fmt += 2;
            } else {
                if (_WeavePy_Arg_String(arg, dest) != 0) return -1;
                st->fmt++;
            }
            return 0;
        }
        case 'w': {
            /* Only `w*` exists in CPython 3.13 (writable buffer). */
            if (has_buf_flag) {
                if (parse_star_buffer(unit, arg, ap) != 0) return -1;
                st->fmt += 2;
                return 0;
            }
            PyErr_SetString(PyExc_SystemError, "invalid use of 'w' format unit");
            return -1;
        }
        case 'p': {
            int *dest = va_arg(*ap, int *);
            if (_WeavePy_Arg_Bool(arg, dest) != 0) return -1;
            st->fmt++;
            return 0;
        }
        case 'O': {
            char modifier = st->fmt[1];
            if (modifier == '!') {
                /* O! takes a type and an object pointer. CPython's
                 * `convertsimple` REJECTS a mismatched argument with a
                 * TypeError — silently accepting it hands the callee a
                 * pointer it will treat as its own struct. Pillow's
                 * `profile_tobytes` parses "O!" against CmsProfile_Type
                 * and immediately reads `profile->profile`; an `int`
                 * passed through crashed inside liblcms
                 * (test_profile_typesafety — literally named "does not
                 * segfault"; RFC 0075 WS9, Pillow selftest lane). */
                PyTypeObject *type = va_arg(*ap, PyTypeObject *);
                PyObject **dest = va_arg(*ap, PyObject **);
                if (type != NULL && !PyObject_TypeCheck(arg, type)) {
                    /* `PyTypeObject` is opaque to this TU; go through
                     * `PyType_GetName` for the message names. */
                    PyObject *want = PyType_GetName(type);
                    PyObject *got = PyType_GetName(Py_TYPE(arg));
                    PyErr_Format(PyExc_TypeError, "must be %S, not %S",
                                 want ? want : Py_None, got ? got : Py_None);
                    Py_XDECREF(want);
                    Py_XDECREF(got);
                    return -1;
                }
                if (_WeavePy_Arg_Object(arg, dest) != 0) return -1;
                st->fmt += 2;
            } else if (modifier == '&') {
                /* O& takes a converter function plus a void* dest.
                 * MSVC's `va_arg` cannot accept a parenthesised
                 * function-pointer type directly, so we route through
                 * a typedef. */
                typedef int (*converter_fn)(PyObject *, void *);
                converter_fn converter = va_arg(*ap, converter_fn);
                void *dest = va_arg(*ap, void *);
                if (converter(arg, dest) == 0) return -1;
                st->fmt += 2;
            } else {
                PyObject **dest = va_arg(*ap, PyObject **);
                if (_WeavePy_Arg_Object(arg, dest) != 0) return -1;
                st->fmt++;
            }
            return 0;
        }
        case 'U': {
            PyObject **dest = va_arg(*ap, PyObject **);
            if (!PyUnicode_Check(arg)) {
                PyErr_SetString(PyExc_TypeError, "expected str");
                return -1;
            }
            if (_WeavePy_Arg_Object(arg, dest) != 0) return -1;
            st->fmt++;
            return 0;
        }
        case 'S': case 'Y': {
            /* CPython `convertsimple`: 'S' stores a *bytes* object (and
             * 'Y' a bytearray) through `PyObject **`, rejecting anything
             * else. Falling to `default` consumed the destination slot
             * without writing it, so Pillow's `_anim_decoder_new`
             * ("S" → `PyBytesObject *webp_string`) read uninitialised
             * stack and every webp decode failed with "could not create
             * decoder object" (RFC 0075 WS9, Pillow selftest lane). */
            PyObject **dest = va_arg(*ap, PyObject **);
            bool ok = (unit == 'S') ? PyBytes_Check(arg) : PyByteArray_Check(arg);
            if (!ok) {
                PyErr_SetString(PyExc_TypeError,
                                unit == 'S' ? "expected bytes"
                                            : "expected bytearray");
                return -1;
            }
            if (_WeavePy_Arg_Object(arg, dest) != 0) return -1;
            st->fmt++;
            return 0;
        }
        case 'e': {
            /* CPython's encoded-string converters `es`/`et` (+`#`)
             * (getargs.c `convertsimple`): two C varargs — the encoding
             * *by value* (NULL = default) and a `char **` destination
             * that receives a NUL-terminated PyMem_Malloc'd copy the
             * CALLER must PyMem_Free; `#` adds a `Py_ssize_t *` length.
             * `es` accepts only str; `et` passes bytes/bytearray through
             * untouched (assumed already in the requested encoding).
             * Pillow's getfont parses "etf|nsy#n" and frees the filename
             * with PyMem_Free — before this case existed the unit fell
             * to `default`, desyncing every later slot (RFC 0075 WS9,
             * Pillow selftest lane). */
            char sub = st->fmt[1];
            if (sub != 's' && sub != 't') {
                PyErr_SetString(PyExc_SystemError,
                                "unknown parser marker combination");
                return -1;
            }
            bool e_len = (st->fmt[2] == '#');
            const char *encoding = va_arg(*ap, const char *);
            char **buffer = va_arg(*ap, char **);
            Py_ssize_t *plen = e_len ? va_arg(*ap, Py_ssize_t *) : NULL;
            if (buffer == NULL) {
                PyErr_SetString(PyExc_SystemError, "(buffer is NULL)");
                return -1;
            }
            const char *src = NULL;
            Py_ssize_t srclen = 0;
            PyObject *encoded = NULL;
            if (sub == 't' && PyBytes_Check(arg)) {
                if (PyBytes_AsStringAndSize(arg, (char **)&src, &srclen) != 0) {
                    return -1;
                }
            } else if (sub == 't' && PyByteArray_Check(arg)) {
                src = PyByteArray_AsString(arg);
                srclen = PyByteArray_Size(arg);
                if (src == NULL) return -1;
            } else if (PyUnicode_Check(arg)) {
                encoded = PyUnicode_AsEncodedString(
                    arg, encoding ? encoding : "utf-8", NULL);
                if (!encoded) return -1;
                if (PyBytes_AsStringAndSize(encoded, (char **)&src, &srclen) != 0) {
                    Py_DECREF(encoded);
                    return -1;
                }
            } else {
                PyErr_SetString(PyExc_TypeError,
                                sub == 't'
                                    ? "argument must be str, bytes or bytearray"
                                    : "argument must be str");
                return -1;
            }
            /* Without `#` there is no length out-slot, so an embedded
             * NUL would silently truncate — CPython raises. */
            if (!e_len && srclen > 0 && memchr(src, '\0', (size_t)srclen) != NULL) {
                Py_XDECREF(encoded);
                PyErr_SetString(PyExc_ValueError, "embedded null character");
                return -1;
            }
            char *out = (char *)PyMem_Malloc((size_t)srclen + 1);
            if (!out) {
                Py_XDECREF(encoded);
                PyErr_SetString(PyExc_MemoryError, "out of memory");
                return -1;
            }
            memcpy(out, src, (size_t)srclen);
            out[srclen] = '\0';
            Py_XDECREF(encoded);
            *buffer = out;
            if (plen) *plen = srclen;
            st->fmt += e_len ? 3 : 2;
            return 0;
        }
        default:
            /* Unknown *conversion* code. Every CPython single-letter
             * conversion writes through exactly one pointer destination,
             * so consume one `void *` to keep the `va_list` in sync — a
             * silent skip here desyncs every later unit and segfaults the
             * caller (this is exactly how the missing `k` above corrupted
             * numpy's dtype `__setstate__`). Punctuation (which carries no
             * va_arg) is filtered out before we reach the switch, so only
             * alpha codes hit this branch. */
            if (isalpha((unsigned char)unit)) {
                (void)va_arg(*ap, void *);
            }
            st->fmt++;
            return 0;
    }
}

/* Count the top-level format units inside a `(...)` group. `p` points
 * just past the opening `(`; scanning stops at the matching `)` (or a
 * `:`/`;`/NUL terminator for a malformed format). Nested groups count
 * as a single unit; only alpha unit-letters (bar the exponent flag
 * `e`) and `(` groups consume a sequence element. Mirrors the counting
 * pass of CPython's `converttuple`. */
static int count_group_units(const char *p) {
    int level = 0;
    int n = 0;
    for (;; p++) {
        char c = *p;
        if (c == '\0' || c == ':' || c == ';') {
            break;
        }
        if (c == '(') {
            if (level == 0) n++;
            level++;
            continue;
        }
        if (c == ')') {
            if (level == 0) break;
            level--;
            continue;
        }
        if (level == 0 && isalpha((unsigned char)c) && c != 'e') {
            n++;
        }
    }
    return n;
}

/* Parse a `(...)` nested-sequence group. On entry `st->fmt` points at
 * the `(`. The bound argument must be a sequence whose length equals
 * the number of top-level units inside the group; each element is then
 * converted against the corresponding inner unit (recursing through
 * `parse_one`, so groups nest). Mirrors CPython's `converttuple`. */
static int parse_group(fmt_state *st, PyObject *arg, va_list *ap) {
    int n = count_group_units(st->fmt + 1);
    int len = _WeavePy_Arg_Length(arg);
    if (len < 0) {
        char buf[96];
        snprintf(buf, sizeof(buf), "expected a sequence (%d-tuple)", n);
        PyErr_SetString(PyExc_TypeError, buf);
        return -1;
    }
    if (len != n) {
        char buf[96];
        snprintf(buf, sizeof(buf),
                 "must be sequence of length %d, not %d", n, len);
        PyErr_SetString(PyExc_TypeError, buf);
        return -1;
    }
    st->fmt++; /* consume '(' */
    int i = 0;
    while (*st->fmt && *st->fmt != ')') {
        char c = *st->fmt;
        /* Whitespace / commas are cosmetic separators inside a group. */
        if (c == ' ' || c == '\t' || c == ',') {
            st->fmt++;
            continue;
        }
        PyObject *elem = fetch_arg(arg, i);
        if (!elem) {
            PyErr_SetString(PyExc_RuntimeError,
                            "PyArg_ParseTuple: NULL group element");
            return -1;
        }
        int rc = parse_one(st, elem, ap);
        if (rc != 0) {
            Py_DECREF(elem);
            return -1;
        }
        /* An inner `O` unit stored `elem` borrowed (CPython semantics);
         * transfer the fetch reference to the group sequence's lifetime
         * instead of dropping it, so the borrow stays valid until the
         * call's argument objects are released. */
        _WeavePy_Arg_Tether(arg, elem);
        i++;
    }
    if (*st->fmt == ')') st->fmt++;
    return 0;
}

/* Advance `st->fmt` over one format unit *and* consume the matching
 * number of `va_arg` destination pointers WITHOUT storing anything —
 * CPython's `skipitem()`. This must run for every optional slot the
 * caller did not supply, otherwise the `va_list` desynchronises and
 * every later unit writes through the wrong destination. (pandas'
 * `ujson_dumps` uses `O|OiOssOOi` and omits `encode_html_chars`; without
 * this the kw `date_unit='ms'` landed in the `orient` pointer, raising
 * "Invalid value 'ms' for option 'orient'".)
 *
 * Every PyArg parse destination is a pointer (the `#` length slots and
 * `O&`'s converter are all pointer-sized), so reading each skipped slot
 * as `void *` is ABI-correct on every supported target. The fmt-cursor
 * advancement mirrors `parse_one` exactly. */
static void skip_one(fmt_state *st, va_list *ap) {
    char unit = *st->fmt;
    if (unit == 0) return;
    char modifier = st->fmt[1];
    switch (unit) {
        case 'O':
            if (modifier == '!' || modifier == '&') {
                (void)va_arg(*ap, void *);
                (void)va_arg(*ap, void *);
                st->fmt += 2;
            } else {
                (void)va_arg(*ap, void *);
                st->fmt++;
            }
            return;
        case 's': case 'z': case 'y':
            (void)va_arg(*ap, void *); /* buffer pointer */
            if (modifier == '#') {
                (void)va_arg(*ap, void *); /* length pointer */
                st->fmt += 2;
            } else {
                st->fmt++;
            }
            return;
        case 'i': case 'I': case 'h': case 'H': case 'b': case 'B':
        case 'l': case 'k': case 'L': case 'q': case 'K': case 'Q':
        case 'n': case 'f': case 'd': case 'p': case 'U':
        case 'S': case 'Y': case 'c': case 'C':
            (void)va_arg(*ap, void *);
            st->fmt++;
            return;
        case 'e':
            /* `es`/`et` (+`#`): encoding value + buffer dest (+ length). */
            (void)va_arg(*ap, void *);
            (void)va_arg(*ap, void *);
            if ((modifier == 's' || modifier == 't') && st->fmt[2] == '#') {
                (void)va_arg(*ap, void *);
                st->fmt += 3;
            } else {
                st->fmt += 2;
            }
            return;
        default:
            /* Unknown conversion code: consume one pointer to stay in sync
             * (see the matching note in `parse_one`). Only alpha codes carry
             * a va_arg; punctuation is handled by the callers. */
            if (isalpha((unsigned char)unit)) {
                (void)va_arg(*ap, void *);
            }
            st->fmt++;
            return;
    }
}

/* NB: `va_list` is an *array type* on the x86_64 SysV ABI
 * (`__va_list_tag[1]`). Passing it by value to a function makes the
 * parameter decay to `__va_list_tag *`, so `&ap` inside the callee
 * is `__va_list_tag **` — NOT the `__va_list_tag (*)[1]` that the
 * `va_list *` parameter of nested helpers expects. Reading a
 * variadic argument through that wrong pointer pulls random bytes
 * out of the stack and the caller then writes through a bogus
 * destination, which is exactly the SIGSEGV that was tripping the
 * `capi_loader` test on Linux CI.
 *
 * The fix is the CPython convention: take the va_list **by
 * pointer** all the way down so the pointer arithmetic stays
 * type-correct.
 */
static int parse_args_from(PyObject *args, const char *fmt, va_list *ap) {
    fmt_state st;
    fmt_init(&st, fmt);
    int n_args = _WeavePy_Arg_Length(args);
    int idx = 0;
    int min_required = 0;
    /* First pass: count required slots (units before `|`). A `(...)`
     * group is a *single* argument (a nested sequence), so its inner
     * unit-letters must not be counted individually — otherwise a
     * format like numpy's `"(iO!O!iO)"` demands 5 args when the caller
     * legitimately passes 1 (the state tuple). Track paren depth and
     * count only depth-0 units. */
    int max_slots = 0;
    {
        int level = 0;
        int optional = 0;
        for (const char *p = fmt; *p; p++) {
            char c = *p;
            if (level == 0 && c == '|') { optional = 1; continue; }
            if (c == ':' || c == ';') break;
            if (c == '(') {
                if (level == 0) { if (!optional) min_required++; max_slots++; }
                level++;
                continue;
            }
            if (c == ')') {
                if (level > 0) level--;
                continue;
            }
            /* CPython `vgetargs1`: every alphabetic unit is one argument,
             * except the `e` encoding prefix (its trailing `s`/`t` is the
             * counted unit). Modifiers like `#`, `!`, `&`, `*` consume
             * extra C varargs but no extra Python arguments. */
            if (level > 0) continue;
            if (isalpha((unsigned char)c) && c != 'e') { if (!optional) min_required++; max_slots++; }
        }
    }
    if (n_args < 0 || n_args < min_required) {
        PyErr_SetString(PyExc_TypeError, "function requires more arguments than were given");
        return 0;
    }
    /* CPython `vgetargs1`: surplus positional arguments are a TypeError
     * ("function takes at most N arguments (M given)") — `_testbuffer`'s
     * `get_contiguous(1,2,3,4,5)` and `staticarray(1,2,3)` rely on it. */
    if (n_args > max_slots) {
        PyErr_Format(PyExc_TypeError,
                     "function takes at most %d argument%s (%d given)",
                     max_slots, max_slots == 1 ? "" : "s", n_args);
        return 0;
    }

    while (*st.fmt) {
        char c = *st.fmt;
        if (c == '|') { st.optional = true; st.fmt++; continue; }
        if (c == ':' || c == ';') { fmt_skip_meta(&st); break; }
        if (c == ' ' || c == '\t') { st.fmt++; continue; }
        if (idx >= n_args) {
            if (!st.optional) {
                PyErr_SetString(PyExc_TypeError, "missing required argument");
                return 0;
            }
            /* CPython `skipitem`: advance over the missing unit AND its
             * va_arg destination slots (multi-char units like `et#`
             * carry several; a bare `st.fmt++` would leave the trailing
             * `t`/`#` to be re-parsed as fresh units). */
            skip_one(&st, ap);
            continue;
        }
        PyObject *arg = fetch_arg(args, idx);
        if (!arg) {
            PyErr_SetString(PyExc_RuntimeError, "PyArg_ParseTuple: NULL arg");
            return 0;
        }
        int rc = parse_one(&st, arg, ap);
        if (rc != 0) {
            Py_DECREF(arg);
            return 0;
        }
        /* An `O`/`U` unit stored `arg` borrowed (CPython semantics);
         * transfer the fetch reference to the args tuple's lifetime so
         * the borrow stays valid until the bridge releases the tuple
         * after the C call returns — exactly CPython's contract. */
        _WeavePy_Arg_Tether(args, arg);
        idx++;
    }
    return 1;
}

int PyArg_ParseTuple(PyObject *args, const char *fmt, ...) {
    va_list ap;
    va_start(ap, fmt);
    int rc = parse_args_from(args, fmt, &ap);
    va_end(ap);
    return rc;
}

int PyArg_Parse(PyObject *args, const char *fmt, ...) {
    va_list ap;
    va_start(ap, fmt);
    int rc = parse_args_from(args, fmt, &ap);
    va_end(ap);
    return rc;
}

int PyArg_VaParse(PyObject *args, const char *fmt, va_list ap) {
    /* Re-establish a *real* va_list local (not a decayed pointer)
     * so `&local` has the correct `va_list *` ABI shape. See the
     * note above `parse_args_from`. */
    va_list local;
    va_copy(local, ap);
    int rc = parse_args_from(args, fmt, &local);
    va_end(local);
    return rc;
}

/* --------------------------------------------------------------
 * Keyword-aware parse.
 *
 * `kwlist` is a NULL-terminated array of `char *` names — one per
 * format slot, in order. CPython lets the caller pass each
 * argument either positionally or by keyword. We mirror that:
 *
 *   1. Walk the format string and `kwlist` together.
 *   2. For each slot, prefer the positional arg if present;
 *      otherwise look the slot's name up in `kwargs`.
 *   3. After consuming all slots, if any kwargs are left over,
 *      raise TypeError("unexpected keyword").
 *
 * Format-string conventions: a leading `$` (CPython 3.8+) makes
 * subsequent units keyword-only. We honour it.
 * -------------------------------------------------------------- */
static int parse_args_kw_from(PyObject *args, PyObject *kwargs, const char *fmt,
                              char **kwlist, va_list *ap) {
    fmt_state st;
    fmt_init(&st, fmt);
    int n_args = _WeavePy_Arg_Length(args);
    int kw_remaining = _WeavePy_Kwargs_Len(kwargs);
    int positional_idx = 0;
    int slot_idx = 0;
    bool keyword_only = false;
    int n_consumed_kw = 0;

    while (*st.fmt) {
        char c = *st.fmt;
        if (c == '|') { st.optional = true; st.fmt++; continue; }
        if (c == '$') { keyword_only = true; st.optional = true; st.fmt++; continue; }
        if (c == ':' || c == ';') { fmt_skip_meta(&st); break; }
        if (c == ' ' || c == '\t') { st.fmt++; continue; }

        const char *name = kwlist ? kwlist[slot_idx] : NULL;
        PyObject *arg = NULL;
        bool got_positional = false;
        if (!keyword_only && positional_idx < n_args) {
            arg = fetch_arg(args, positional_idx);
            positional_idx++;
            got_positional = true;
        } else if (name && kwargs) {
            arg = _WeavePy_Kwargs_Pop(kwargs, name);
            if (arg) n_consumed_kw++;
        }
        if (!arg) {
            if (!st.optional) {
                PyErr_SetString(PyExc_TypeError, "missing required argument");
                return 0;
            }
            /* Optional slot not supplied: advance the format AND consume
             * the matching va_arg destination(s) (CPython's skipitem),
             * so a later keyword-supplied unit still writes through its
             * own pointer rather than this skipped slot's. */
            skip_one(&st, ap);
            slot_idx++;
            continue;
        }
        /* If a name was provided AND a positional arg is consumed,
         * CPython treats a kw with the same name as TypeError. We
         * implement that by additionally popping the kw and erroring
         * out if present. */
        if (got_positional && name && kwargs) {
            PyObject *dup = _WeavePy_Kwargs_Pop(kwargs, name);
            if (dup) {
                PyErr_SetString(PyExc_TypeError, "argument given by name and position");
                Py_DECREF(dup);
                Py_DECREF(arg);
                return 0;
            }
        }
        int rc = parse_one(&st, arg, ap);
        if (rc != 0) {
            Py_DECREF(arg);
            return 0;
        }
        /* Same borrowed-`O` contract as the positional loop: park the
         * fetch reference on the args tuple (present for both positional
         * and keyword-sourced values — the bridge frees args and kwargs
         * together after the call). */
        _WeavePy_Arg_Tether(args ? args : kwargs, arg);
        slot_idx++;
    }

    /* CPython `vgetargskeywords`: surplus positional arguments are a
     * TypeError (`staticarray(1, 2, 3)` with a "|O" format must refuse). */
    if (positional_idx < n_args) {
        PyErr_Format(PyExc_TypeError,
                     "function takes at most %d positional argument%s (%d given)",
                     positional_idx, positional_idx == 1 ? "" : "s", n_args);
        return 0;
    }

    /* Detect the CPython "invalid keyword argument" TypeError, with its
     * exact message shape (getargs.c): the function name comes from the
     * format's ":name" suffix, else "this function". pandas'
     * `pytest.raises(..., match=...)` tests match on this text
     * (np.datetime64(..., dtype=...) in test_td_floordiv_invalid_scalar). */
    if (kwargs && n_consumed_kw < kw_remaining) {
        const char *bad = _WeavePy_Kwargs_KeyAt(kwargs, 0);
        const char *fname = strchr(fmt, ':');
        if (fname) fname++;
        char buf[256];
        if (fname && *fname) {
            snprintf(buf, sizeof(buf),
                     "'%s' is an invalid keyword argument for %s()",
                     bad ? bad : "?", fname);
        } else {
            snprintf(buf, sizeof(buf),
                     "'%s' is an invalid keyword argument for this function",
                     bad ? bad : "?");
        }
        PyErr_SetString(PyExc_TypeError, buf);
        return 0;
    }
    return 1;
}

int PyArg_ParseTupleAndKeywords(PyObject *args, PyObject *kwargs, const char *fmt,
                                char **kwlist, ...) {
    va_list ap;
    va_start(ap, kwlist);
    int rc = parse_args_kw_from(args, kwargs, fmt, kwlist, &ap);
    va_end(ap);
    return rc;
}

int PyArg_VaParseTupleAndKeywords(PyObject *args, PyObject *kwargs, const char *fmt,
                                  char **kwlist, va_list ap) {
    /* Re-establish a real va_list local (see `PyArg_VaParse`). */
    va_list local;
    va_copy(local, ap);
    int rc = parse_args_kw_from(args, kwargs, fmt, kwlist, &local);
    va_end(local);
    return rc;
}

int PyArg_UnpackTuple(PyObject *args, const char *name, Py_ssize_t min,
                      Py_ssize_t max, ...) {
    (void)name;
    int n = _WeavePy_Arg_Length(args);
    if (n < min || (max >= 0 && n > max)) {
        PyErr_SetString(PyExc_TypeError, "PyArg_UnpackTuple: arg count mismatch");
        return 0;
    }
    va_list ap;
    va_start(ap, max);
    for (Py_ssize_t i = 0; i < n; i++) {
        PyObject **dest = va_arg(ap, PyObject **);
        PyObject *item = fetch_arg(args, (int)i);
        if (!item) {
            va_end(ap);
            return 0;
        }
        /* CPython hands out borrowed references backed by the args
         * tuple. Our fetch may have minted a fresh box the tuple does
         * not own, so park the fetch reference on the tuple's lifetime
         * rather than dropping it (a plain DECREF could free the box
         * while the caller still holds the borrowed pointer). */
        _WeavePy_Arg_Tether(args, item);
        *dest = item;
    }
    va_end(ap);
    return 1;
}

/* --------------------------------------------------------------
 * Py_BuildValue family.
 * -------------------------------------------------------------- */

static PyObject *build_one(const char **fmt, va_list *ap);

static int collect_until(const char **fmt, char terminator,
                         PyObject ***out_items, Py_ssize_t *out_n,
                         va_list *ap) {
    Py_ssize_t cap = 4;
    Py_ssize_t n = 0;
    PyObject **items = (PyObject **)malloc(cap * sizeof(PyObject *));
    if (!items) return -1;
    while (**fmt && **fmt != terminator) {
        if (n == cap) {
            cap *= 2;
            PyObject **resized = (PyObject **)realloc(items, cap * sizeof(PyObject *));
            if (!resized) {
                free(items);
                return -1;
            }
            items = resized;
        }
        PyObject *p = build_one(fmt, ap);
        if (!p) {
            for (Py_ssize_t i = 0; i < n; i++) Py_DECREF(items[i]);
            free(items);
            return -1;
        }
        items[n++] = p;
    }
    if (**fmt == terminator) (*fmt)++;
    *out_items = items;
    *out_n = n;
    return 0;
}

static PyObject *build_one(const char **fmt, va_list *ap) {
    char unit = **fmt;
    if (unit == 0) {
        return _WeavePy_Build_None();
    }
    (*fmt)++;
    bool has_len = (**fmt == '#');
    switch (unit) {
        case 'i': case 'h': case 'b': case 'B': {
            int v = va_arg(*ap, int);
            return _WeavePy_Build_FromI64((long long)v);
        }
        case 'I': {
            unsigned int v = va_arg(*ap, unsigned int);
            return _WeavePy_Build_FromU64((unsigned long long)v);
        }
        case 'l': {
            long v = va_arg(*ap, long);
            return _WeavePy_Build_FromI64((long long)v);
        }
        case 'L': case 'q': {
            long long v = va_arg(*ap, long long);
            return _WeavePy_Build_FromI64(v);
        }
        case 'K': case 'Q': {
            unsigned long long v = va_arg(*ap, unsigned long long);
            return _WeavePy_Build_FromU64(v);
        }
        case 'k': {
            unsigned long v = va_arg(*ap, unsigned long);
            return _WeavePy_Build_FromU64((unsigned long long)v);
        }
        case 'n': {
            Py_ssize_t v = va_arg(*ap, Py_ssize_t);
            return _WeavePy_Build_FromI64((long long)v);
        }
        case 'f': case 'd': {
            double v = va_arg(*ap, double);
            return _WeavePy_Build_FromDouble(v);
        }
        case 's': {
            const char *s = va_arg(*ap, const char *);
            if (has_len) {
                Py_ssize_t n = va_arg(*ap, Py_ssize_t);
                (*fmt)++;
                return _WeavePy_Build_FromStringAndSize(s, n);
            }
            return _WeavePy_Build_FromString(s);
        }
        case 'z': {
            const char *s = va_arg(*ap, const char *);
            if (has_len) {
                Py_ssize_t n = va_arg(*ap, Py_ssize_t);
                (*fmt)++;
                if (!s) return _WeavePy_Build_None();
                return _WeavePy_Build_FromStringAndSize(s, n);
            }
            if (!s) return _WeavePy_Build_None();
            return _WeavePy_Build_FromString(s);
        }
        case 'y': {
            const char *s = va_arg(*ap, const char *);
            if (has_len) {
                Py_ssize_t n = va_arg(*ap, Py_ssize_t);
                (*fmt)++;
                return _WeavePy_Build_FromBytesAndSize(s, n);
            }
            return _WeavePy_Build_FromBytesAndSize(s, (Py_ssize_t)strlen(s ? s : ""));
        }
        case 'O': case 'N': {
            PyObject *p = va_arg(*ap, PyObject *);
            if (!p) {
                /* CPython would set an exception here; for the
                 * foundation we substitute None. */
                return _WeavePy_Build_None();
            }
            if (unit == 'O') Py_INCREF(p);
            return p;
        }
        case 'S': case 'U': {
            PyObject *p = va_arg(*ap, PyObject *);
            if (!p) return _WeavePy_Build_None();
            Py_INCREF(p);
            return p;
        }
        case '(': {
            PyObject **items = NULL;
            Py_ssize_t n = 0;
            if (collect_until(fmt, ')', &items, &n, ap) != 0) return NULL;
            PyObject *t = _WeavePy_Build_TupleFromArray(n, items);
            free(items);
            return t;
        }
        case '[': {
            PyObject **items = NULL;
            Py_ssize_t n = 0;
            if (collect_until(fmt, ']', &items, &n, ap) != 0) return NULL;
            PyObject *l = _WeavePy_Build_ListFromArray(n, items);
            free(items);
            return l;
        }
        case '{': {
            PyObject **items = NULL;
            Py_ssize_t n = 0;
            if (collect_until(fmt, '}', &items, &n, ap) != 0) return NULL;
            PyObject **keys = (PyObject **)malloc((n / 2) * sizeof(PyObject *));
            PyObject **vals = (PyObject **)malloc((n / 2) * sizeof(PyObject *));
            for (Py_ssize_t i = 0; i + 1 < n; i += 2) {
                keys[i / 2] = items[i];
                vals[i / 2] = items[i + 1];
            }
            PyObject *d = _WeavePy_Build_DictFromArrays(n / 2, keys, vals);
            free(keys);
            free(vals);
            free(items);
            return d;
        }
        case ',': case ' ': case ':':
            return build_one(fmt, ap);
        default:
            return _WeavePy_Build_None();
    }
}

/* Shared core for Py_BuildValue / Py_VaBuildValue.
 *
 * CPython's `va_build_value` semantics: a format string with a single
 * top-level unit yields *that* unit; two or more top-level units yield
 * a *tuple* of them. Both the `...` and `va_list` entry points must
 * agree — a previous version open-coded the single-unit case in
 * `Py_VaBuildValue`, so `PyObject_CallFunction(c, "ll", a, b)` (which
 * routes through `Py_VaBuildValue`) silently dropped every argument
 * past the first and called `c` with a 1-tuple. */
static PyObject *build_value_impl(const char *fmt, va_list *ap) {
    const char *p = fmt;
    /* Count top-level units. A unit at depth 0 is either an alpha
     * format code (`i`, `s`, `O`, …) or an opening bracket that begins
     * a nested tuple/list/dict. */
    int top_units = 0;
    int depth = 0;
    for (const char *q = fmt; *q; q++) {
        if (depth == 0 && (*q == '(' || *q == '[' || *q == '{')) {
            top_units++;
            depth++;
        } else if (depth == 0 && isalpha((unsigned char)*q)) {
            top_units++;
        } else if (*q == '(' || *q == '[' || *q == '{') {
            depth++;
        } else if (*q == ')' || *q == ']' || *q == '}') {
            depth--;
        }
    }
    if (top_units == 1) {
        return build_one(&p, ap);
    }
    PyObject **items = NULL;
    Py_ssize_t n = 0;
    Py_ssize_t cap = top_units > 0 ? top_units : 1;
    items = (PyObject **)malloc((size_t)cap * sizeof(PyObject *));
    if (!items) {
        return NULL;
    }
    while (*p) {
        PyObject *one = build_one(&p, ap);
        if (!one) {
            for (Py_ssize_t i = 0; i < n; i++) Py_DECREF(items[i]);
            free(items);
            return NULL;
        }
        items[n++] = one;
    }
    PyObject *result = _WeavePy_Build_TupleFromArray(n, items);
    free(items);
    return result;
}

PyObject *Py_BuildValue(const char *fmt, ...) {
    if (!fmt) return _WeavePy_Build_None();
    va_list ap;
    va_start(ap, fmt);
    PyObject *result = build_value_impl(fmt, &ap);
    va_end(ap);
    return result;
}

PyObject *Py_VaBuildValue(const char *fmt, va_list ap) {
    if (!fmt) return _WeavePy_Build_None();
    /* Re-establish a real va_list local (see `PyArg_VaParse`). */
    va_list local;
    va_copy(local, ap);
    PyObject *result = build_value_impl(fmt, &local);
    va_end(local);
    return result;
}

PyObject *PyTuple_Pack(Py_ssize_t n, ...) {
    va_list ap;
    va_start(ap, n);
    if (n < 0) n = 0;
    PyObject **items = (PyObject **)malloc((size_t)(n > 0 ? n : 1) * sizeof(PyObject *));
    for (Py_ssize_t i = 0; i < n; i++) {
        items[i] = va_arg(ap, PyObject *);
    }
    PyObject *t = _WeavePy_TuplePackFromArray(n, items);
    free(items);
    va_end(ap);
    return t;
}

/* --------------------------------------------------------------
 * PyBytes_FromFormat / PyBytes_FromFormatV.
 *
 * Mirrors CPython's `bytesobject.c` grammar exactly (which is NOT
 * C's printf): supported units are `%%`, `%c`, `%d`, `%u`, `%i`,
 * `%x`, `%s`, `%p`, with the `l`/`z` length flags on `%d`/`%u`,
 * a parsed-and-ignored width, and a `%.Ns` precision that truncates
 * the C string. `%c` range-checks [0; 255] with OverflowError. An
 * unrecognised unit copies the rest of the format verbatim from the
 * `%` and stops (so `b"%"` → `b"%"` and `b"x=%i y=%"` → `b"x=2 y=%"`).
 * `%p` is guaranteed to start `0x` regardless of the platform printf.
 * -------------------------------------------------------------- */

typedef struct {
    char *buf;
    size_t len;
    size_t cap;
} bytes_writer;

static int bw_reserve(bytes_writer *w, size_t extra) {
    if (w->len + extra <= w->cap) return 0;
    size_t cap = w->cap ? w->cap * 2 : 64;
    while (cap < w->len + extra) cap *= 2;
    char *nb = (char *)realloc(w->buf, cap);
    if (!nb) return -1;
    w->buf = nb;
    w->cap = cap;
    return 0;
}

static int bw_write(bytes_writer *w, const char *s, size_t n) {
    if (bw_reserve(w, n) != 0) return -1;
    memcpy(w->buf + w->len, s, n);
    w->len += n;
    return 0;
}

PyObject *PyBytes_FromFormatV(const char *format, va_list vargs) {
    bytes_writer w = {NULL, 0, 0};
    char buffer[64];
    const char *f;

    for (f = format; *f; f++) {
        if (*f != '%') {
            if (bw_write(&w, f, 1) != 0) goto nomem;
            continue;
        }
        const char *p = f;
        f++;
        /* ignore the width (ex: 10 in "%10s") */
        while (isdigit((unsigned char)*f)) f++;
        /* parse the precision (ex: 10 in "%.10s") */
        size_t prec = 0;
        int has_prec = 0;
        if (*f == '.') {
            has_prec = 1;
            f++;
            for (; isdigit((unsigned char)*f); f++) {
                prec = prec * 10 + (size_t)(*f - '0');
            }
        }
        /* length flags, only for the integer units (CPython parity) */
        int longflag = 0, size_tflag = 0;
        if (*f == 'l' && (f[1] == 'd' || f[1] == 'u')) { longflag = 1; f++; }
        else if (*f == 'z' && (f[1] == 'd' || f[1] == 'u')) { size_tflag = 1; f++; }

        switch (*f) {
        case '%':
            if (bw_write(&w, "%", 1) != 0) goto nomem;
            break;
        case 'c': {
            int c = va_arg(vargs, int);
            if (c < 0 || c > 255) {
                PyErr_SetString(PyExc_OverflowError,
                                "PyBytes_FromFormat(): %c format "
                                "expected an integer in range [0; 255]");
                goto error;
            }
            buffer[0] = (char)(unsigned char)c;
            if (bw_write(&w, buffer, 1) != 0) goto nomem;
            break;
        }
        case 'd': {
            int n;
            if (longflag) n = snprintf(buffer, sizeof(buffer), "%ld", va_arg(vargs, long));
            else if (size_tflag) n = snprintf(buffer, sizeof(buffer), "%zd", va_arg(vargs, Py_ssize_t));
            else n = snprintf(buffer, sizeof(buffer), "%d", va_arg(vargs, int));
            if (n > 0 && bw_write(&w, buffer, (size_t)n) != 0) goto nomem;
            break;
        }
        case 'u': {
            int n;
            if (longflag) n = snprintf(buffer, sizeof(buffer), "%lu", va_arg(vargs, unsigned long));
            else if (size_tflag) n = snprintf(buffer, sizeof(buffer), "%zu", va_arg(vargs, size_t));
            else n = snprintf(buffer, sizeof(buffer), "%u", va_arg(vargs, unsigned int));
            if (n > 0 && bw_write(&w, buffer, (size_t)n) != 0) goto nomem;
            break;
        }
        case 'i': {
            int n = snprintf(buffer, sizeof(buffer), "%i", va_arg(vargs, int));
            if (n > 0 && bw_write(&w, buffer, (size_t)n) != 0) goto nomem;
            break;
        }
        case 'x': {
            int n = snprintf(buffer, sizeof(buffer), "%x", va_arg(vargs, int));
            if (n > 0 && bw_write(&w, buffer, (size_t)n) != 0) goto nomem;
            break;
        }
        case 's': {
            const char *s = va_arg(vargs, const char *);
            size_t n = strlen(s ? s : "");
            if (has_prec && n > prec) n = prec;
            if (s && bw_write(&w, s, n) != 0) goto nomem;
            break;
        }
        case 'p': {
            int n = snprintf(buffer, sizeof(buffer), "%p", va_arg(vargs, void *));
            if (n <= 0) break;
            /* CPython guarantees a leading "0x" whatever the libc does. */
            if (buffer[1] == 'X') {
                buffer[1] = 'x';
            }
            else if (buffer[1] != 'x') {
                memmove(buffer + 2, buffer, (size_t)n);
                buffer[0] = '0';
                buffer[1] = 'x';
                n += 2;
            }
            if (bw_write(&w, buffer, (size_t)n) != 0) goto nomem;
            break;
        }
        default: {
            /* Invalid format unit: copy the rest verbatim and stop. */
            size_t n = strlen(p);
            if (bw_write(&w, p, n) != 0) goto nomem;
            goto done;
        }
        }
    }
done:;
    PyObject *result = _WeavePy_Build_FromBytesAndSize(w.buf ? w.buf : "", (Py_ssize_t)w.len);
    free(w.buf);
    return result;
nomem:
    PyErr_SetString(PyExc_MemoryError, "PyBytes_FromFormat(): out of memory");
error:
    free(w.buf);
    return NULL;
}

PyObject *PyBytes_FromFormat(const char *format, ...) {
    va_list ap;
    va_start(ap, format);
    PyObject *r = PyBytes_FromFormatV(format, ap);
    va_end(ap);
    return r;
}

/* --------------------------------------------------------------
 * String / error formatters.
 * -------------------------------------------------------------- */

static void wpy_append(char *buf, size_t bufsize, size_t *pos, const char *s, size_t len) {
    if (*pos + 1 >= bufsize || s == NULL) {
        return;
    }
    size_t room = bufsize - 1 - *pos;
    size_t copy = len < room ? len : room;
    memcpy(buf + *pos, s, copy);
    *pos += copy;
    buf[*pos] = '\0';
}

/* CPython's `PyUnicode_FromFormat` / `PyErr_Format` accept a printf-like
 * grammar that is *not* C's printf: it adds object conversions (`%S` str,
 * `%R` repr, `%A` ascii, `%U` unicode, `%V` unicode-or-fallback, `%T`
 * fully-qualified type name) and only a documented subset of the integer
 * family. C's `vsnprintf` mangles `%R` (prints a literal `R` and consumes
 * no argument), so we must walk the format ourselves: object specs are
 * rendered by calling the object protocol and the result is spliced in
 * (honouring width/precision); standard specs are reconstructed verbatim
 * and handed to `snprintf` one directive at a time with the correctly
 * typed argument peeled off the `va_list`. */
static int weavepy_format_into(char *buf, size_t bufsize, const char *fmt, va_list ap) {
    if (bufsize == 0) {
        return 0;
    }
    size_t pos = 0;
    buf[0] = '\0';
    const char *p = fmt;
    char tmp[8192];
    while (*p) {
        if (*p != '%') {
            wpy_append(buf, bufsize, &pos, p, 1);
            p++;
            continue;
        }
        const char *start = p;
        p++; /* skip '%' */
        if (*p == '%') {
            wpy_append(buf, bufsize, &pos, "%", 1);
            p++;
            continue;
        }
        /* flags — CPython accepts '-' (left-justify), '0' (zero-pad) and
         * '#' (the %#T / %#N separator); anything else ('+', ' ') lands
         * on the conversion switch and raises SystemError below
         * (test_from_format "%+i"). */
        char flags[8];
        int nf = 0;
        while (*p && strchr("-0#", *p)) {
            if (nf < 7) flags[nf++] = *p;
            p++;
        }
        flags[nf] = '\0';
        /* width */
        char width[16];
        int nw = 0;
        int width_star = 0;
        if (*p == '*') {
            width_star = 1;
            p++;
        } else {
            while (isdigit((unsigned char)*p)) {
                if (nw < 15) width[nw++] = *p;
                p++;
            }
        }
        width[nw] = '\0';
        /* precision */
        char prec[16];
        int npr = 0;
        int prec_star = 0;
        int has_prec = 0;
        if (*p == '.') {
            has_prec = 1;
            p++;
            if (*p == '*') {
                prec_star = 1;
                p++;
            } else {
                while (isdigit((unsigned char)*p)) {
                    if (npr < 15) prec[npr++] = *p;
                    p++;
                }
            }
        }
        prec[npr] = '\0';
        /* length modifiers */
        char length[4];
        int nl = 0;
        while (*p && strchr("hljztL", *p)) {
            if (nl < 3) length[nl++] = *p;
            p++;
        }
        length[nl] = '\0';
        char conv = *p;
        if (conv == '\0') {
            /* Dangling '%' (or trailing width/precision/length):
             * CPython raises SystemError (test_from_format "%", "%0",
             * "%.", "%l", ...). */
            PyErr_Format(PyExc_SystemError, "invalid format string: %s",
                         start);
            return -2;
        }
        p++;

        /* Object conversions: render via the object protocol, then apply
         * width/precision by reformatting the resulting C string with a
         * synthesised `%[flags][width][.prec]s` directive. */
        if (conv == 'S' || conv == 'R' || conv == 'A' || conv == 'U' ||
            conv == 'V' || conv == 'T' || conv == 'N') {
            int wv = 0, pv = 0;
            if (width_star) wv = va_arg(ap, int);
            if (prec_star) pv = va_arg(ap, int);
            PyObject *owned = NULL;
            const char *cs = NULL;
            /* A %V that fell back to its C string behaves like %s:
             * precision counts *bytes* of the raw UTF-8 (decoded with
             * "replace" at the end), not code points. */
            int cs_is_cstr = 0;
            char wtmp[8192];
            if (conv == 'V') {
                PyObject *o = va_arg(ap, PyObject *);
                if (nl >= 1 && length[0] == 'l') {
                    /* %lV — the fallback is a wchar_t* (CPython's
                     * longflag → PyUnicode_FromWideChar); precision
                     * counts code points like %ls. */
                    const wchar_t *wfb = va_arg(ap, const wchar_t *);
                    if (o) {
                        owned = PyObject_Str(o);
                        cs = owned ? PyUnicode_AsUTF8(owned) : NULL;
                    } else if (wfb != NULL) {
                        size_t tn = 0;
                        for (const wchar_t *w = wfb;
                             *w && tn + 4 < sizeof(wtmp); w++) {
                            unsigned int cp = (unsigned int)*w;
                            if (cp < 0x80) {
                                wtmp[tn++] = (char)cp;
                            } else if (cp < 0x800) {
                                wtmp[tn++] = (char)(0xC0 | (cp >> 6));
                                wtmp[tn++] = (char)(0x80 | (cp & 0x3F));
                            } else if (cp < 0x10000) {
                                wtmp[tn++] = (char)(0xE0 | (cp >> 12));
                                wtmp[tn++] = (char)(0x80 | ((cp >> 6) & 0x3F));
                                wtmp[tn++] = (char)(0x80 | (cp & 0x3F));
                            } else {
                                wtmp[tn++] = (char)(0xF0 | (cp >> 18));
                                wtmp[tn++] = (char)(0x80 | ((cp >> 12) & 0x3F));
                                wtmp[tn++] = (char)(0x80 | ((cp >> 6) & 0x3F));
                                wtmp[tn++] = (char)(0x80 | (cp & 0x3F));
                            }
                        }
                        wtmp[tn] = '\0';
                        cs = wtmp;
                    }
                } else {
                    const char *fb = va_arg(ap, const char *);
                    if (o) {
                        owned = PyObject_Str(o);
                        cs = owned ? PyUnicode_AsUTF8(owned) : fb;
                    } else {
                        cs = fb;
                        cs_is_cstr = 1;
                    }
                }
            } else if (conv == 'T' || conv == 'N') {
                /* %T — the fully-qualified name of the argument's type;
                 * %N — same, but the argument *is* the type. The name is
                 * module.qualname with a "builtins" module omitted, and
                 * ':' instead of '.' under the '#' flag (CPython 3.13's
                 * _PyType_GetFullyQualifiedName). */
                PyObject *o = va_arg(ap, PyObject *);
                PyObject *t = NULL;
                if (conv == 'N') {
                    int is_type =
                        o ? PyObject_IsInstance(o, (PyObject *)&PyType_Type)
                          : 0;
                    if (is_type <= 0) {
                        PyErr_Clear();
                        PyErr_SetString(PyExc_TypeError,
                                        "%N argument must be a type");
                        return -2;
                    }
                    t = o;
                } else {
                    t = o ? (PyObject *)Py_TYPE(o) : NULL;
                }
                if (t == NULL) {
                    cs = "NULL";
                } else {
                    PyObject *mod = PyObject_GetAttrString(t, "__module__");
                    PyObject *qual = PyObject_GetAttrString(t, "__qualname__");
                    const char *mods = mod ? PyUnicode_AsUTF8(mod) : NULL;
                    const char *quals = qual ? PyUnicode_AsUTF8(qual) : NULL;
                    if (mods == NULL || quals == NULL) PyErr_Clear();
                    if (quals == NULL) {
                        owned = PyType_GetName((PyTypeObject *)t);
                        cs = owned ? PyUnicode_AsUTF8(owned) : "NULL";
                    } else if (mods == NULL || strcmp(mods, "builtins") == 0) {
                        snprintf(wtmp, sizeof(wtmp), "%s", quals);
                        cs = wtmp;
                    } else {
                        char sep = (strchr(flags, '#') != NULL) ? ':' : '.';
                        snprintf(wtmp, sizeof(wtmp), "%s%c%s", mods, sep,
                                 quals);
                        cs = wtmp;
                    }
                    Py_XDECREF(mod);
                    Py_XDECREF(qual);
                }
            } else {
                PyObject *o = va_arg(ap, PyObject *);
                if (o == NULL) {
                    cs = "NULL";
                } else if (conv == 'S') {
                    owned = PyObject_Str(o);
                    cs = owned ? PyUnicode_AsUTF8(owned) : NULL;
                } else if (conv == 'R') {
                    owned = PyObject_Repr(o);
                    cs = owned ? PyUnicode_AsUTF8(owned) : NULL;
                } else if (conv == 'A') {
                    owned = PyObject_ASCII(o);
                    cs = owned ? PyUnicode_AsUTF8(owned) : NULL;
                } else { /* 'U' */
                    cs = PyUnicode_AsUTF8(o);
                }
            }
            if (cs == NULL) cs = "<error>";
            /* Width and precision count *code points*, not bytes (CPython
             * measures the unicode result — "%.3R" of '\u20acABCDEF' is
             * "'\u20acA", test_from_format), so truncate and pad along
             * UTF-8 character boundaries. */
            {
                size_t cs_len = strlen(cs);
                if (has_prec) {
                    int limit = prec_star ? pv : atoi(prec);
                    if (limit < 0) limit = 0;
                    if (cs_is_cstr) {
                        if ((size_t)limit < cs_len) cs_len = (size_t)limit;
                    } else {
                        size_t i = 0;
                        int count = 0;
                        while (i < cs_len && count < limit) {
                            i++;
                            while (i < cs_len &&
                                   ((unsigned char)cs[i] & 0xC0) == 0x80) {
                                i++;
                            }
                            count++;
                        }
                        cs_len = i;
                    }
                }
                int nchars = 0;
                for (size_t i = 0; i < cs_len; i++) {
                    if (((unsigned char)cs[i] & 0xC0) != 0x80) nchars++;
                }
                int want = width_star ? wv : (nw ? atoi(width) : 0);
                int padn = want > nchars ? want - nchars : 0;
                int left_align = (strchr(flags, '-') != NULL);
                if (!left_align) {
                    for (int k = 0; k < padn; k++)
                        wpy_append(buf, bufsize, &pos, " ", 1);
                }
                wpy_append(buf, bufsize, &pos, cs, cs_len);
                if (left_align) {
                    for (int k = 0; k < padn; k++)
                        wpy_append(buf, bufsize, &pos, " ", 1);
                }
            }
            Py_XDECREF(owned);
            continue;
        }

        /* Standard C conversions: rebuild the directive verbatim and hand
         * it to snprintf with a correctly typed argument. */
        char dir[48];
        {
            size_t dl = (size_t)(p - start);
            if (dl >= sizeof(dir)) dl = sizeof(dir) - 1;
            memcpy(dir, start, dl);
            dir[dl] = '\0';
        }
        int wv = 0, pv = 0;
        if (width_star) wv = va_arg(ap, int);
        if (prec_star) pv = va_arg(ap, int);
        int n = 0;
        int is_ll = (nl >= 2 && length[0] == 'l' && length[1] == 'l');
        int is_l = (nl == 1 && length[0] == 'l');
        int is_z = (nl >= 1 && length[0] == 'z');
        int is_j = (nl >= 1 && length[0] == 'j');
        int is_t = (nl >= 1 && length[0] == 't');
#define WPY_SNPRINTF(argexpr)                                                  \
    do {                                                                       \
        if (width_star && prec_star)                                           \
            n = snprintf(tmp, sizeof(tmp), dir, wv, pv, argexpr);              \
        else if (width_star || prec_star)                                      \
            n = snprintf(tmp, sizeof(tmp), dir, (width_star ? wv : pv),        \
                         argexpr);                                             \
        else                                                                   \
            n = snprintf(tmp, sizeof(tmp), dir, argexpr);                      \
    } while (0)
        switch (conv) {
            case 'd':
            case 'i': {
                if (is_ll) {
                    WPY_SNPRINTF(va_arg(ap, long long));
                } else if (is_l) {
                    WPY_SNPRINTF(va_arg(ap, long));
                } else if (is_z) {
                    WPY_SNPRINTF(va_arg(ap, Py_ssize_t));
                } else if (is_j) {
                    WPY_SNPRINTF(va_arg(ap, intmax_t));
                } else if (is_t) {
                    WPY_SNPRINTF(va_arg(ap, ptrdiff_t));
                } else {
                    WPY_SNPRINTF(va_arg(ap, int));
                }
                break;
            }
            case 'u':
            case 'o':
            case 'x':
            case 'X': {
                if (is_ll) {
                    WPY_SNPRINTF(va_arg(ap, unsigned long long));
                } else if (is_l) {
                    WPY_SNPRINTF(va_arg(ap, unsigned long));
                } else if (is_z) {
                    WPY_SNPRINTF(va_arg(ap, size_t));
                } else if (is_j) {
                    WPY_SNPRINTF(va_arg(ap, uintmax_t));
                } else if (is_t) {
                    WPY_SNPRINTF(va_arg(ap, size_t));
                } else {
                    WPY_SNPRINTF(va_arg(ap, unsigned int));
                }
                break;
            }
            case 'c': {
                /* CPython's %c takes a Unicode ordinal, not a C char:
                 * range-check against MAX_UNICODE and splice the UTF-8
                 * encoding in (test_from_format checks U+ABCD and
                 * U+10FFFF, and OverflowError past the range). */
                int ordinal = va_arg(ap, int);
                if (ordinal < 0 || ordinal > 0x10FFFF) {
                    PyErr_SetString(PyExc_OverflowError,
                                    "character argument not in range(0x110000)");
                    return -2;
                }
                unsigned int cp = (unsigned int)ordinal;
                n = 0;
                if (cp < 0x80) {
                    tmp[n++] = (char)cp;
                } else if (cp < 0x800) {
                    tmp[n++] = (char)(0xC0 | (cp >> 6));
                    tmp[n++] = (char)(0x80 | (cp & 0x3F));
                } else if (cp < 0x10000) {
                    tmp[n++] = (char)(0xE0 | (cp >> 12));
                    tmp[n++] = (char)(0x80 | ((cp >> 6) & 0x3F));
                    tmp[n++] = (char)(0x80 | (cp & 0x3F));
                } else {
                    tmp[n++] = (char)(0xF0 | (cp >> 18));
                    tmp[n++] = (char)(0x80 | ((cp >> 12) & 0x3F));
                    tmp[n++] = (char)(0x80 | ((cp >> 6) & 0x3F));
                    tmp[n++] = (char)(0x80 | (cp & 0x3F));
                }
                break;
            }
            case 'e':
            case 'E':
            case 'f':
            case 'F':
            case 'g':
            case 'G': {
                WPY_SNPRINTF(va_arg(ap, double));
                break;
            }
            case 's': {
                if (is_ll || is_z || is_j || is_t) {
                    /* Only the wide 'l' modifier is valid on %s
                     * (test_from_format "%lls", "%zs"). */
                    PyErr_Format(PyExc_SystemError,
                                 "invalid format string: %s", start);
                    return -2;
                }
                if (is_l) {
                    /* %ls — a wchar_t* argument (CPython's longflag):
                     * precision counts wide chars, width pads by code
                     * points (test_from_format "%5ls"). wchar_t is
                     * UTF-32 on unix. */
                    const wchar_t *ws = va_arg(ap, const wchar_t *);
                    if (ws == NULL) ws = L"";
                    int limit = -1;
                    if (has_prec) limit = prec_star ? pv : atoi(prec);
                    size_t tn = 0;
                    int nchars = 0;
                    for (const wchar_t *w = ws;
                         *w && (limit < 0 || nchars < limit) &&
                         tn + 4 < sizeof(tmp);
                         w++, nchars++) {
                        unsigned int cp = (unsigned int)*w;
                        if (cp < 0x80) {
                            tmp[tn++] = (char)cp;
                        } else if (cp < 0x800) {
                            tmp[tn++] = (char)(0xC0 | (cp >> 6));
                            tmp[tn++] = (char)(0x80 | (cp & 0x3F));
                        } else if (cp < 0x10000) {
                            tmp[tn++] = (char)(0xE0 | (cp >> 12));
                            tmp[tn++] = (char)(0x80 | ((cp >> 6) & 0x3F));
                            tmp[tn++] = (char)(0x80 | (cp & 0x3F));
                        } else {
                            tmp[tn++] = (char)(0xF0 | (cp >> 18));
                            tmp[tn++] = (char)(0x80 | ((cp >> 12) & 0x3F));
                            tmp[tn++] = (char)(0x80 | ((cp >> 6) & 0x3F));
                            tmp[tn++] = (char)(0x80 | (cp & 0x3F));
                        }
                    }
                    int want = width_star ? wv : (nw ? atoi(width) : 0);
                    int padn = want > nchars ? want - nchars : 0;
                    int left_align = (strchr(flags, '-') != NULL);
                    if (!left_align) {
                        for (int k = 0; k < padn; k++)
                            wpy_append(buf, bufsize, &pos, " ", 1);
                    }
                    wpy_append(buf, bufsize, &pos, tmp, tn);
                    if (left_align) {
                        for (int k = 0; k < padn; k++)
                            wpy_append(buf, bufsize, &pos, " ", 1);
                    }
                    n = 0; /* already appended */
                } else {
                    WPY_SNPRINTF(va_arg(ap, const char *));
                }
                break;
            }
            case 'p': {
                WPY_SNPRINTF(va_arg(ap, void *));
                break;
            }
            default: {
                /* Unknown conversion (including '%' after flags/width,
                 * "%+i", "%1abc"): SystemError, like CPython. */
                PyErr_Format(PyExc_SystemError, "invalid format string: %s",
                             start);
                return -2;
            }
        }
#undef WPY_SNPRINTF
        /* CPython zero-pads a numeric conversion to *width* even when a
         * precision is present ("%010.7i" of 123 is "0000000123"); C's
         * printf switches to space padding there, so re-pad by hand. */
        if (n > 0 && has_prec && strchr(flags, '0') != NULL &&
            (conv == 'd' || conv == 'i' || conv == 'u' || conv == 'o' ||
             conv == 'x' || conv == 'X')) {
            int want = width_star ? wv : (nw ? atoi(width) : 0);
            int lead = 0;
            while (lead < n && tmp[lead] == ' ') lead++;
            int body = n - lead;
            if (want > body && lead > 0) {
                int sign = (tmp[lead] == '-' || tmp[lead] == '+') ? 1 : 0;
                if (sign) {
                    wpy_append(buf, bufsize, &pos, tmp + lead, 1);
                    lead++;
                    body--;
                }
                for (int k = body; k < want - sign; k++)
                    wpy_append(buf, bufsize, &pos, "0", 1);
                wpy_append(buf, bufsize, &pos, tmp + lead, (size_t)body);
                n = 0; /* consumed */
            }
        }
        if (n > 0) {
            wpy_append(buf, bufsize, &pos, tmp, (size_t)n);
        }
    }
    return (int)pos;
}

PyObject *PyUnicode_FromFormatV(const char *fmt, va_list ap) {
    /* CPython validates the format string up front: it must be pure
     * ASCII (unicode_fromformat_arg rejects the first byte >= 0x80 with
     * this exact ValueError — test_capi.test_unicode.test_from_format). */
    for (const unsigned char *q = (const unsigned char *)fmt; *q; q++) {
        if (*q >= 0x80) {
            PyErr_Format(PyExc_ValueError,
                         "PyUnicode_FromFormatV() expects an ASCII-encoded "
                         "format string, got a non-ASCII byte: 0x%02x",
                         (int)*q);
            return NULL;
        }
    }
    char buf[8192];
    int n = weavepy_format_into(buf, sizeof(buf), fmt, ap);
    if (n == -2) {
        /* A directive raised (e.g. %c ordinal out of range). */
        return NULL;
    }
    if (n < 0) {
        return _WeavePy_Build_None();
    }
    /* CPython decodes %s splices with errors="replace"
     * (unicode_fromformat_arg), so a precision-truncated UTF-8 sequence
     * becomes U+FFFD instead of raising (test_from_format "%.5s"). */
    return PyUnicode_DecodeUTF8(buf, (Py_ssize_t)n, "replace");
}

PyObject *PyUnicode_FromFormat(const char *fmt, ...) {
    va_list ap;
    va_start(ap, fmt);
    PyObject *p = PyUnicode_FromFormatV(fmt, ap);
    va_end(ap);
    return p;
}

PyObject *PyErr_FormatV(PyObject *ty, const char *fmt, va_list ap) {
    /* CPython routes through PyUnicode_FromFormatV: a non-ASCII format
     * string raises ValueError, and a directive that raises (e.g. %c
     * ordinal out of range -> OverflowError) leaves *that* exception
     * set instead of `ty` (test_capi.test_exceptions test_format). */
    for (const unsigned char *q = (const unsigned char *)fmt; *q; q++) {
        if (*q >= 0x80) {
            PyErr_Format(PyExc_ValueError,
                         "PyUnicode_FromFormatV() expects an ASCII-encoded "
                         "format string, got a non-ASCII byte: 0x%02x",
                         (int)*q);
            return NULL;
        }
    }
    char buf[4096];
    int n = weavepy_format_into(buf, sizeof(buf), fmt, ap);
    if (n == -2) {
        /* A directive raised; keep its exception. */
        return NULL;
    }
    if (n < 0) {
        _WeavePy_Format_Set(ty, "<format error>", 14);
    } else {
        _WeavePy_Format_Set(ty, buf, (Py_ssize_t)n);
    }
    return NULL;
}

PyObject *PyErr_Format(PyObject *ty, const char *fmt, ...) {
    va_list ap;
    va_start(ap, fmt);
    PyObject *r = PyErr_FormatV(ty, fmt, ap);
    va_end(ap);
    return r;
}

/* PySys_WriteStdout / PySys_WriteStderr / PySys_FormatStdout /
 * PySys_FormatStderr — printf-formatted writes to the *Python-level*
 * sys.stdout / sys.stderr (so `support.captured_output` sees them),
 * falling back to the C stream when the attribute is missing or the
 * write fails (test_capi.test_sys). The Write* pair mirrors CPython's
 * `sys_write`: C vsnprintf into a 1001-byte buffer, with a literal
 * "... truncated" tail on overflow. The Format* pair mirrors
 * `sys_format`: the PyUnicode_FromFormatV grammar, unlimited length. */
static void weavepy_sys_write_str(const char *name, FILE *fp, const char *text) {
    PyObject *file = PySys_GetObject(name); /* borrowed */
    if (file != NULL && file != Py_None) {
        PyObject *res = PyObject_CallMethod(file, "write", "s", text);
        if (res != NULL) {
            Py_DECREF(res);
            return;
        }
        PyErr_Clear();
    }
    fputs(text, fp);
}

static void weavepy_sys_write(const char *name, FILE *fp, const char *format, va_list va) {
    char buffer[1001];
    int written = vsnprintf(buffer, sizeof(buffer), format, va);
    weavepy_sys_write_str(name, fp, buffer);
    if (written < 0 || (size_t)written >= sizeof(buffer)) {
        weavepy_sys_write_str(name, fp, "... truncated");
    }
}

void PySys_WriteStdout(const char *format, ...) {
    va_list va;
    va_start(va, format);
    weavepy_sys_write("stdout", stdout, format, va);
    va_end(va);
}

void PySys_WriteStderr(const char *format, ...) {
    va_list va;
    va_start(va, format);
    weavepy_sys_write("stderr", stderr, format, va);
    va_end(va);
}

static void weavepy_sys_format(const char *name, FILE *fp, const char *format, va_list va) {
    PyObject *text = PyUnicode_FromFormatV(format, va);
    if (text == NULL) {
        PyErr_Clear();
        return;
    }
    PyObject *file = PySys_GetObject(name); /* borrowed */
    if (file != NULL && file != Py_None) {
        PyObject *res = PyObject_CallMethod(file, "write", "O", text);
        if (res != NULL) {
            Py_DECREF(res);
            Py_DECREF(text);
            return;
        }
        PyErr_Clear();
    }
    const char *cs = PyUnicode_AsUTF8(text);
    if (cs != NULL) {
        fputs(cs, fp);
    } else {
        PyErr_Clear();
    }
    Py_DECREF(text);
}

void PySys_FormatStdout(const char *format, ...) {
    va_list va;
    va_start(va, format);
    weavepy_sys_format("stdout", stdout, format, va);
    va_end(va);
}

void PySys_FormatStderr(const char *format, ...) {
    va_list va;
    va_start(va, format);
    weavepy_sys_format("stderr", stderr, format, va);
    va_end(va);
}

/* PyErr_FormatUnraisable (3.13) — report-and-swallow: CPython routes the
 * formatted message plus the pending exception to sys.unraisablehook.
 * WeavePy prints the message to stderr and discards the pending error,
 * matching the "must not propagate" contract (cffi teardown paths). */
void PyErr_FormatUnraisable(const char *fmt, ...) {
    char buf[4096];
    va_list ap;
    va_start(ap, fmt);
    int n = weavepy_format_into(buf, sizeof(buf), fmt, ap);
    va_end(ap);
    if (n > 0) {
        fprintf(stderr, "%s\n", buf);
    }
    PyErr_Clear();
}

/* _PyErr_FormatFromCause — raise a freshly-formatted exception whose
 * __cause__/__context__ is the previously-pending one (CPython's
 * "raise X from err" shape, used by mypyc's import-failure paths).
 * The detach/re-attach pair lives in Rust (mypyc_tail.rs). */
extern PyObject *_WeavePy_FetchForCause(void);
extern void _WeavePy_ApplyCause(PyObject *cause);

PyObject *_PyErr_FormatFromCause(PyObject *ty, const char *fmt, ...) {
    PyObject *cause = _WeavePy_FetchForCause();
    va_list ap;
    va_start(ap, fmt);
    (void)PyErr_FormatV(ty, fmt, ap);
    va_end(ap);
    _WeavePy_ApplyCause(cause);
    return NULL;
}

/* --------------------------------------------------------------
 * Variadic convenience callers.
 * -------------------------------------------------------------- */

PyObject *PyObject_CallFunction(PyObject *callable, const char *fmt, ...) {
    va_list ap;
    va_start(ap, fmt);
    PyObject *args;
    if (!fmt || !*fmt) {
        args = _WeavePy_Build_TupleFromArray(0, NULL);
    } else {
        args = Py_VaBuildValue(fmt, ap);
        /* Wrap a single value as a 1-tuple. */
        if (args && !PyTuple_Check(args)) {
            PyObject *one[1] = { args };
            PyObject *tup = _WeavePy_TuplePackFromArray(1, one);
            Py_DECREF(args);
            args = tup;
        }
    }
    va_end(ap);
    PyObject *result = PyObject_Call(callable, args, NULL);
    Py_XDECREF(args);
    return result;
}

PyObject *PyObject_CallMethod(PyObject *target, const char *name, const char *fmt, ...) {
    va_list ap;
    va_start(ap, fmt);
    PyObject *callable = PyObject_GetAttrString(target, name);
    if (!callable) { va_end(ap); return NULL; }
    PyObject *args;
    if (!fmt || !*fmt) {
        args = _WeavePy_Build_TupleFromArray(0, NULL);
    } else {
        args = Py_VaBuildValue(fmt, ap);
        if (args && !PyTuple_Check(args)) {
            PyObject *one[1] = { args };
            PyObject *tup = _WeavePy_TuplePackFromArray(1, one);
            Py_DECREF(args);
            args = tup;
        }
    }
    va_end(ap);
    PyObject *result = PyObject_Call(callable, args, NULL);
    Py_DECREF(callable);
    Py_XDECREF(args);
    return result;
}

PyObject *PyObject_CallMethodObjArgs(PyObject *target, PyObject *name, ...) {
    if (!target || !name) return NULL;
    const char *cname = PyUnicode_AsUTF8(name);
    if (!cname) return NULL;
    PyObject *callable = PyObject_GetAttrString(target, cname);
    if (!callable) return NULL;
    /* Walk varargs until NULL. */
    va_list ap;
    va_start(ap, name);
    Py_ssize_t cap = 8;
    Py_ssize_t n = 0;
    PyObject **items = (PyObject **)malloc(cap * sizeof(PyObject *));
    while (1) {
        PyObject *p = va_arg(ap, PyObject *);
        if (!p) break;
        if (n == cap) {
            cap *= 2;
            items = (PyObject **)realloc(items, cap * sizeof(PyObject *));
        }
        items[n++] = p;
    }
    va_end(ap);
    PyObject *args = _WeavePy_TuplePackFromArray(n, items);
    free(items);
    PyObject *result = PyObject_Call(callable, args, NULL);
    Py_DECREF(callable);
    Py_DECREF(args);
    return result;
}

PyObject *PyObject_CallFunctionObjArgs(PyObject *callable, ...) {
    if (!callable) return NULL;
    va_list ap;
    va_start(ap, callable);
    Py_ssize_t cap = 8;
    Py_ssize_t n = 0;
    PyObject **items = (PyObject **)malloc(cap * sizeof(PyObject *));
    while (1) {
        PyObject *p = va_arg(ap, PyObject *);
        if (!p) break;
        if (n == cap) {
            cap *= 2;
            items = (PyObject **)realloc(items, cap * sizeof(PyObject *));
        }
        items[n++] = p;
    }
    va_end(ap);
    PyObject *args = _WeavePy_TuplePackFromArray(n, items);
    free(items);
    PyObject *result = PyObject_Call(callable, args, NULL);
    Py_DECREF(args);
    return result;
}

/* --------------------------------------------------------------
 * RFC 0046 (wave 4): variadic tail numpy links.
 * -------------------------------------------------------------- */

/* PyOS_snprintf — a thin, locale-independent vsnprintf wrapper, matching
 * CPython's behaviour of always NUL-terminating the buffer. */
int PyOS_snprintf(char *str, size_t size, const char *format, ...) {
    va_list ap;
    va_start(ap, format);
    int n = vsnprintf(str, size, format, ap);
    va_end(ap);
    if (size > 0) {
        str[size - 1] = '\0';
    }
    return n;
}

/* PyErr_WarnFormat — format the message and route it through the
 * non-variadic PyErr_WarnEx. Warnings are advisory; a failure to render
 * the warning never aborts the caller. */
int PyErr_WarnFormat(PyObject *category, Py_ssize_t stack_level,
                     const char *format, ...) {
    char buf[1024];
    va_list ap;
    va_start(ap, format);
    vsnprintf(buf, sizeof(buf), format, ap);
    va_end(ap);
    buf[sizeof(buf) - 1] = '\0';
    return PyErr_WarnEx(category, buf, stack_level);
}
