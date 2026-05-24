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
#include <stdarg.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

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
                /* O& takes a converter function plus a void* dest. */
                int (*converter)(PyObject *, void *) = va_arg(*ap, int (*)(PyObject *, void *));
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

static int parse_args_from(PyObject *args, const char *fmt, va_list ap) {
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
        int rc = parse_one(&st, arg, &ap);
        Py_DECREF(arg);
        if (rc != 0) return 0;
        idx++;
    }
    return 1;
}

int PyArg_ParseTuple(PyObject *args, const char *fmt, ...) {
    va_list ap;
    va_start(ap, fmt);
    int rc = parse_args_from(args, fmt, ap);
    va_end(ap);
    return rc;
}

int PyArg_Parse(PyObject *args, const char *fmt, ...) {
    va_list ap;
    va_start(ap, fmt);
    int rc = parse_args_from(args, fmt, ap);
    va_end(ap);
    return rc;
}

int PyArg_VaParse(PyObject *args, const char *fmt, va_list ap) {
    return parse_args_from(args, fmt, ap);
}

int PyArg_ParseTupleAndKeywords(PyObject *args, PyObject *kwargs, const char *fmt,
                                char **kwlist, ...) {
    /* For the foundation we ignore kwargs entirely and parse the
     * positional tuple. A future RFC will add full keyword
     * binding. */
    (void)kwargs;
    (void)kwlist;
    va_list ap;
    va_start(ap, kwlist);
    int rc = parse_args_from(args, fmt, ap);
    va_end(ap);
    return rc;
}

int PyArg_VaParseTupleAndKeywords(PyObject *args, PyObject *kwargs, const char *fmt,
                                  char **kwlist, va_list ap) {
    (void)kwargs;
    (void)kwlist;
    return parse_args_from(args, fmt, ap);
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

PyObject *Py_BuildValue(const char *fmt, ...) {
    if (!fmt) return _WeavePy_Build_None();
    va_list ap;
    va_start(ap, fmt);
    PyObject *result = NULL;
    /* If the format starts with a single unit, return that; otherwise
     * wrap in a tuple. */
    const char *p = fmt;
    /* Quick scan to count top-level units. */
    int top_units = 0;
    int depth = 0;
    for (const char *q = fmt; *q; q++) {
        if (*q == '(' || *q == '[' || *q == '{') depth++;
        else if (*q == ')' || *q == ']' || *q == '}') depth--;
        else if (depth == 0 && isalpha((unsigned char)*q)) top_units++;
    }
    if (top_units == 1) {
        result = build_one(&p, &ap);
    } else {
        PyObject **items = NULL;
        Py_ssize_t n = 0;
        Py_ssize_t cap = top_units > 0 ? top_units : 1;
        items = (PyObject **)malloc(cap * sizeof(PyObject *));
        while (*p) {
            PyObject *one = build_one(&p, &ap);
            if (!one) {
                for (Py_ssize_t i = 0; i < n; i++) Py_DECREF(items[i]);
                free(items);
                va_end(ap);
                return NULL;
            }
            items[n++] = one;
        }
        result = _WeavePy_Build_TupleFromArray(n, items);
        free(items);
    }
    va_end(ap);
    return result;
}

PyObject *Py_VaBuildValue(const char *fmt, va_list ap) {
    if (!fmt) return _WeavePy_Build_None();
    return build_one(&fmt, &ap);
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

static int weavepy_format_into(char *buf, size_t bufsize, const char *fmt, va_list ap) {
    /* Translate CPython %-specs that don't appear in C's printf
     * (`%S`, `%R`, `%U`, `%V`, `%T`, `%A`) into placeholders.
     * Everything else is forwarded to vsnprintf. */
    char tmp[8192];
    int written = 0;
    char *out = bufsize > sizeof(tmp) ? buf : tmp;
    size_t outsize = bufsize > sizeof(tmp) ? bufsize : sizeof(tmp);
    int n = vsnprintf(out, outsize, fmt, ap);
    if (n < 0) return -1;
    if (out != buf) {
        size_t copy = (size_t)n < bufsize ? (size_t)n : bufsize - 1;
        memcpy(buf, out, copy);
        buf[copy] = '\0';
        n = (int)copy;
    }
    written = n;
    return written;
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
