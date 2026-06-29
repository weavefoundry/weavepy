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

/* Convert a single format unit into the va_arg destination(s).
 * Returns 0 on success, -1 on failure (with an exception set). */
static int parse_one(fmt_state *st, PyObject *arg, va_list *ap) {
    char unit = *st->fmt;
    if (unit == 0) return -1;

    /* The 's#'/'y#'/'z#' family takes both a buffer pointer and a length. */
    bool has_len_flag = (st->fmt[1] == '#');

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
        case 'y': {
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
        case 'p': {
            int *dest = va_arg(*ap, int *);
            if (_WeavePy_Arg_Bool(arg, dest) != 0) return -1;
            st->fmt++;
            return 0;
        }
        case 'O': {
            char modifier = st->fmt[1];
            if (modifier == '!') {
                /* O! takes a type and an object pointer. */
                /* discard the type */
                (void)va_arg(*ap, PyTypeObject *);
                PyObject **dest = va_arg(*ap, PyObject **);
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
        default:
            /* Unknown unit — log and skip the slot. */
            st->fmt++;
            return 0;
    }
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
        case 'i': case 'I': case 'h': case 'b': case 'B':
        case 'l': case 'L': case 'q': case 'K': case 'Q':
        case 'n': case 'f': case 'd': case 'p': case 'U':
            (void)va_arg(*ap, void *);
            st->fmt++;
            return;
        default:
            /* Unknown unit: `parse_one` advances the cursor without a
             * `va_arg`, so mirror that here too. */
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
    /* First pass: count required slots (units before `|`). */
    for (const char *p = fmt; *p; p++) {
        if (*p == '|') break;
        if (*p == ':' || *p == ';') break;
        if (isalpha((unsigned char)*p)) min_required++;
        if (*p == '#') min_required--; /* `#` is paired with the previous unit */
    }
    if (n_args < 0 || n_args < min_required) {
        PyErr_SetString(PyExc_TypeError, "function requires more arguments than were given");
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
            /* Consume the missing format unit so the va_list is left
             * untouched (no more args to read). */
            st.fmt++;
            if (*st.fmt == '#') st.fmt++;
            continue;
        }
        PyObject *arg = fetch_arg(args, idx);
        if (!arg) {
            PyErr_SetString(PyExc_RuntimeError, "PyArg_ParseTuple: NULL arg");
            return 0;
        }
        int rc = parse_one(&st, arg, ap);
        Py_DECREF(arg);
        if (rc != 0) return 0;
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
        Py_DECREF(arg);
        if (rc != 0) return 0;
        slot_idx++;
    }

    /* Detect "unexpected keyword argument". */
    if (kwargs && n_consumed_kw < kw_remaining) {
        const char *bad = _WeavePy_Kwargs_KeyAt(kwargs, 0);
        char buf[128];
        snprintf(buf, sizeof(buf),
                 "unexpected keyword argument '%s'",
                 bad ? bad : "?");
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
        Py_DECREF(item); /* convert the +1 from fetch_arg into a borrowed ref */
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
        /* flags */
        char flags[8];
        int nf = 0;
        while (*p && strchr("-+ 0#", *p)) {
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
            /* dangling '%': emit verbatim. */
            wpy_append(buf, bufsize, &pos, start, (size_t)(p - start));
            break;
        }
        p++;

        /* Object conversions: render via the object protocol, then apply
         * width/precision by reformatting the resulting C string with a
         * synthesised `%[flags][width][.prec]s` directive. */
        if (conv == 'S' || conv == 'R' || conv == 'A' || conv == 'U' ||
            conv == 'V' || conv == 'T') {
            int wv = 0, pv = 0;
            if (width_star) wv = va_arg(ap, int);
            if (prec_star) pv = va_arg(ap, int);
            PyObject *owned = NULL;
            const char *cs = NULL;
            if (conv == 'V') {
                PyObject *o = va_arg(ap, PyObject *);
                const char *fb = va_arg(ap, const char *);
                if (o) {
                    owned = PyObject_Str(o);
                    cs = owned ? PyUnicode_AsUTF8(owned) : fb;
                } else {
                    cs = fb;
                }
            } else if (conv == 'T') {
                PyObject *o = va_arg(ap, PyObject *);
                cs = o ? PyType_GetName(Py_TYPE(o)) : "NULL";
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
            /* Reformat with width/precision if either was requested. */
            if (nf || nw || width_star || has_prec) {
                char sspec[48];
                if (width_star && prec_star) {
                    snprintf(sspec, sizeof(sspec), "%%%s%d.%ds", flags, wv, pv);
                } else if (width_star) {
                    snprintf(sspec, sizeof(sspec), "%%%s%d%s%ss", flags, wv,
                             has_prec ? "." : "", has_prec ? prec : "");
                } else if (prec_star) {
                    snprintf(sspec, sizeof(sspec), "%%%s%s.%ds", flags, width, pv);
                } else {
                    snprintf(sspec, sizeof(sspec), "%%%s%s%s%ss", flags, width,
                             has_prec ? "." : "", has_prec ? prec : "");
                }
                int n = snprintf(tmp, sizeof(tmp), sspec, cs);
                if (n > 0) wpy_append(buf, bufsize, &pos, tmp, (size_t)n);
            } else {
                wpy_append(buf, bufsize, &pos, cs, strlen(cs));
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
                WPY_SNPRINTF(va_arg(ap, int));
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
                WPY_SNPRINTF(va_arg(ap, const char *));
                break;
            }
            case 'p': {
                WPY_SNPRINTF(va_arg(ap, void *));
                break;
            }
            default: {
                /* Unknown spec: emit verbatim, consume nothing. */
                wpy_append(buf, bufsize, &pos, start, (size_t)(p - start));
                n = -1;
                break;
            }
        }
#undef WPY_SNPRINTF
        if (n > 0) {
            wpy_append(buf, bufsize, &pos, tmp, (size_t)n);
        }
    }
    return (int)pos;
}

PyObject *PyUnicode_FromFormatV(const char *fmt, va_list ap) {
    char buf[8192];
    int n = weavepy_format_into(buf, sizeof(buf), fmt, ap);
    if (n < 0) {
        return _WeavePy_Build_None();
    }
    return _WeavePy_Build_FromStringAndSize(buf, (Py_ssize_t)n);
}

PyObject *PyUnicode_FromFormat(const char *fmt, ...) {
    va_list ap;
    va_start(ap, fmt);
    PyObject *p = PyUnicode_FromFormatV(fmt, ap);
    va_end(ap);
    return p;
}

PyObject *PyErr_FormatV(PyObject *ty, const char *fmt, va_list ap) {
    char buf[4096];
    int n = weavepy_format_into(buf, sizeof(buf), fmt, ap);
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
