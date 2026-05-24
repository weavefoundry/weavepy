/*
 * Python.h — WeavePy's C-API.
 *
 * Tracks CPython 3.13's Py_LIMITED_API surface. A C extension that
 * compiles against this header should be loadable into both CPython
 * 3.13 and WeavePy without changes; only the symbols it imports
 * via libpythonXY-style ABI need to be present at runtime.
 *
 * In WeavePy's case the symbols come from the host weavepy binary
 * directly (no separate libpython): the binary statically links
 * `weavepy-capi`, exports those symbols, and the dlopen()'d
 * extension resolves them at load time.
 *
 * RFC 0022 is the design document; the comments in this header
 * deliberately mirror the section structure of `Include/Python.h`
 * in CPython so the surface is easy to compare side by side.
 */

#ifndef WEAVEPY_PYTHON_H
#define WEAVEPY_PYTHON_H

#ifdef __cplusplus
extern "C" {
#endif

/* ------------------------------------------------------------------
 * pyport.h — platform / type configuration.
 * ------------------------------------------------------------------ */

#include <stddef.h>
#include <stdint.h>
#include <stdarg.h>
#include <stdio.h>

/* The CPython version we target. WeavePy reports 3.13.0. */
#define PY_MAJOR_VERSION 3
#define PY_MINOR_VERSION 13
#define PY_MICRO_VERSION 0
#define PY_VERSION_HEX ((PY_MAJOR_VERSION << 24) | (PY_MINOR_VERSION << 16) | (PY_MICRO_VERSION << 8))

/* Py_ssize_t: CPython uses ssize_t; we mirror that. */
typedef intptr_t Py_ssize_t;
typedef intptr_t Py_intptr_t;
typedef ptrdiff_t Py_hash_t;
typedef unsigned long Py_uhash_t;

#define PY_SSIZE_T_MAX  ((Py_ssize_t)((((size_t)-1) >> 1)))
#define PY_SSIZE_T_MIN  (-PY_SSIZE_T_MAX - 1)

/* No-op visibility macros — the WeavePy binary already exports the
 * symbols via its linker. CPython uses these to mark "stable ABI"
 * vs. "internal" symbols; we don't make that distinction yet. */
#define PyAPI_FUNC(rtype) extern rtype
#define PyAPI_DATA(rtype) extern rtype

/* Mark a function unused without warnings. */
#if defined(__GNUC__) || defined(__clang__)
#  define Py_UNUSED(arg) __attribute__((unused)) arg
#else
#  define Py_UNUSED(arg) arg
#endif

/* ------------------------------------------------------------------
 * object.h — core PyObject type and refcount macros.
 *
 * NOTE: the actual layout of `PyObject` is opaque — extensions only
 * need to see `ob_refcnt` and `ob_type` for the inline macros below.
 * The full struct lives in the host binary; extensions never alloc
 * one directly.
 * ------------------------------------------------------------------ */

typedef struct _object PyObject;
typedef struct _typeobject PyTypeObject;

struct _object {
    Py_ssize_t ob_refcnt;
    PyTypeObject *ob_type;
};

/* Var-sized object header. CPython makes the distinction so that
 * tuples / bytes / etc. can store their length right after the
 * header. WeavePy doesn't need it for storage (the payload lives
 * in a Rust Object) but we expose the same shape for ABI compat. */
typedef struct {
    PyObject ob_base;
    Py_ssize_t ob_size;
} PyVarObject;

#define Py_REFCNT(ob)   (((PyObject*)(ob))->ob_refcnt)
#define Py_TYPE(ob)     (((PyObject*)(ob))->ob_type)
#define Py_SIZE(ob)     (((PyVarObject*)(ob))->ob_size)
#define Py_SET_REFCNT(ob, n)   ((Py_REFCNT(ob)) = (n))
#define Py_SET_TYPE(ob, t)     ((Py_TYPE(ob)) = (t))
#define Py_SET_SIZE(ob, n)     ((Py_SIZE(ob)) = (n))

#define PyObject_HEAD       PyObject ob_base;
#define PyObject_VAR_HEAD   PyVarObject ob_base;

#define PyObject_HEAD_INIT(type) { 1, (type) },
#define PyVarObject_HEAD_INIT(type, size) { PyObject_HEAD_INIT(type) (size) },

PyAPI_FUNC(void) Py_IncRef(PyObject *o);
PyAPI_FUNC(void) Py_DecRef(PyObject *o);
PyAPI_FUNC(PyObject *) Py_NewRef(PyObject *o);
PyAPI_FUNC(PyObject *) Py_XNewRef(PyObject *o);

/* We expose Py_INCREF/Py_DECREF as plain function calls — the
 * inline-macro form CPython uses dereferences `ob_refcnt` directly,
 * but we want every refcount mutation to go through a single
 * choke-point so the host can keep the bridged Rust Object alive
 * until the C-side count reaches zero. */
#define Py_INCREF(op)  Py_IncRef((PyObject*)(op))
#define Py_DECREF(op)  Py_DecRef((PyObject*)(op))
#define Py_XINCREF(op) do { PyObject *_py_x = (PyObject*)(op); if (_py_x != NULL) Py_INCREF(_py_x); } while (0)
#define Py_XDECREF(op) do { PyObject *_py_x = (PyObject*)(op); if (_py_x != NULL) Py_DECREF(_py_x); } while (0)
#define Py_CLEAR(op)   do { PyObject **_py_t = (PyObject**)&(op); PyObject *_py_o = *_py_t; *_py_t = NULL; Py_XDECREF(_py_o); } while (0)

/* Universal singletons. Statically allocated in the host binary. */
PyAPI_DATA(PyObject) _Py_NoneStruct;
PyAPI_DATA(PyObject) _Py_TrueStruct;
PyAPI_DATA(PyObject) _Py_FalseStruct;
PyAPI_DATA(PyObject) _Py_NotImplementedStruct;
PyAPI_DATA(PyObject) _Py_EllipsisObject;

#define Py_None             (&_Py_NoneStruct)
#define Py_True             (&_Py_TrueStruct)
#define Py_False            (&_Py_FalseStruct)
#define Py_NotImplemented   (&_Py_NotImplementedStruct)
#define Py_Ellipsis         (&_Py_EllipsisObject)

#define Py_RETURN_NONE          do { Py_INCREF(Py_None); return Py_None; } while (0)
#define Py_RETURN_TRUE          do { Py_INCREF(Py_True); return Py_True; } while (0)
#define Py_RETURN_FALSE         do { Py_INCREF(Py_False); return Py_False; } while (0)
#define Py_RETURN_NOTIMPLEMENTED do { Py_INCREF(Py_NotImplemented); return Py_NotImplemented; } while (0)

#define Py_Is(x, y)           ((x) == (y))
#define Py_IsNone(x)          Py_Is((x), Py_None)
#define Py_IsTrue(x)          Py_Is((x), Py_True)
#define Py_IsFalse(x)         Py_Is((x), Py_False)

/* Comparison operators (`Py_LT`, ..., `Py_GE`) for PyObject_RichCompare. */
#define Py_LT 0
#define Py_LE 1
#define Py_EQ 2
#define Py_NE 3
#define Py_GT 4
#define Py_GE 5

/* Status value returned by some inquiry-style functions. */
#define Py_T_NONE   0

/* ------------------------------------------------------------------
 * Type system — PyType_Spec / PyType_Slot.
 *
 * Extensions describe a class via a static array of slots; the
 * runtime materialises the type on first call to PyType_FromSpec.
 * ------------------------------------------------------------------ */

typedef PyObject *(*unaryfunc)(PyObject *);
typedef PyObject *(*binaryfunc)(PyObject *, PyObject *);
typedef PyObject *(*ternaryfunc)(PyObject *, PyObject *, PyObject *);
typedef int (*inquiry)(PyObject *);
typedef Py_ssize_t (*lenfunc)(PyObject *);
typedef PyObject *(*ssizeargfunc)(PyObject *, Py_ssize_t);
typedef int (*ssizeobjargproc)(PyObject *, Py_ssize_t, PyObject *);
typedef int (*objobjproc)(PyObject *, PyObject *);
typedef int (*objobjargproc)(PyObject *, PyObject *, PyObject *);
typedef PyObject *(*getattrfunc)(PyObject *, char *);
typedef PyObject *(*getattrofunc)(PyObject *, PyObject *);
typedef int (*setattrofunc)(PyObject *, PyObject *, PyObject *);
typedef int (*setattrfunc)(PyObject *, char *, PyObject *);
typedef Py_hash_t (*hashfunc)(PyObject *);
typedef PyObject *(*reprfunc)(PyObject *);
typedef PyObject *(*richcmpfunc)(PyObject *, PyObject *, int);
typedef PyObject *(*getiterfunc)(PyObject *);
typedef PyObject *(*iternextfunc)(PyObject *);
typedef PyObject *(*descrgetfunc)(PyObject *, PyObject *, PyObject *);
typedef int (*descrsetfunc)(PyObject *, PyObject *, PyObject *);
typedef int (*initproc)(PyObject *, PyObject *, PyObject *);
typedef PyObject *(*newfunc)(PyTypeObject *, PyObject *, PyObject *);
typedef PyObject *(*allocfunc)(PyTypeObject *, Py_ssize_t);
typedef void (*destructor)(PyObject *);
typedef void (*freefunc)(void *);

typedef PyObject *(*PyCFunction)(PyObject *self, PyObject *args);
typedef PyObject *(*PyCFunctionWithKeywords)(PyObject *self, PyObject *args, PyObject *kwargs);
typedef PyObject *(*_PyCFunctionFastWithKeywords)(PyObject *self, PyObject *const *args, Py_ssize_t nargs, PyObject *kwnames);

/* Method calling conventions. */
#define METH_VARARGS    0x0001
#define METH_KEYWORDS   0x0002
#define METH_NOARGS     0x0004
#define METH_O          0x0008
#define METH_CLASS      0x0010
#define METH_STATIC     0x0020
#define METH_COEXIST    0x0040
#define METH_FASTCALL   0x0080
#define METH_METHOD     0x0200

typedef struct PyMethodDef {
    const char *ml_name;
    PyCFunction ml_meth;
    int ml_flags;
    const char *ml_doc;
} PyMethodDef;

typedef struct PyMemberDef PyMemberDef;
typedef struct PyGetSetDef PyGetSetDef;

typedef struct PyType_Slot {
    int slot;
    void *pfunc;
} PyType_Slot;

typedef struct PyType_Spec {
    const char *name;
    int basicsize;
    int itemsize;
    unsigned int flags;
    PyType_Slot *slots;
} PyType_Spec;

/* Type slot identifiers. We support a documented subset; the
 * unsupported slots are accepted by PyType_FromSpec but ignored. */
#define Py_tp_doc           56
#define Py_tp_base          39
#define Py_tp_bases         40
#define Py_tp_call          50
#define Py_tp_clear         51
#define Py_tp_dealloc       52
#define Py_tp_del           53
#define Py_tp_descr_get     54
#define Py_tp_descr_set     55
#define Py_tp_finalize      80
#define Py_tp_free          57
#define Py_tp_getattr       58
#define Py_tp_getattro      59
#define Py_tp_hash          60
#define Py_tp_init          61
#define Py_tp_is_gc         62
#define Py_tp_iter          63
#define Py_tp_iternext      64
#define Py_tp_methods       65
#define Py_tp_new           65
#define Py_tp_repr          66
#define Py_tp_richcompare   67
#define Py_tp_setattr       68
#define Py_tp_setattro      69
#define Py_tp_str           70
#define Py_tp_traverse      71
#define Py_tp_members       72
#define Py_tp_getset        73
#define Py_nb_add           5
#define Py_nb_subtract      11
#define Py_nb_multiply      9
#define Py_nb_true_divide   30
#define Py_nb_remainder     10
#define Py_nb_negative      8
#define Py_nb_positive      29
#define Py_nb_absolute      6
#define Py_nb_bool          7
#define Py_nb_int           8
#define Py_nb_float         12
#define Py_sq_length        45
#define Py_sq_concat        42
#define Py_sq_item          43
#define Py_sq_ass_item      44
#define Py_sq_contains      41
#define Py_mp_length        29
#define Py_mp_subscript     31
#define Py_mp_ass_subscript 32

/* Common type flags the typespec can carry. */
#define Py_TPFLAGS_DEFAULT          0x00000000UL
#define Py_TPFLAGS_BASETYPE         (1UL << 10)
#define Py_TPFLAGS_HEAPTYPE         (1UL << 9)
#define Py_TPFLAGS_HAVE_GC          (1UL << 14)
#define Py_TPFLAGS_LIST_SUBCLASS    (1UL << 25)
#define Py_TPFLAGS_TUPLE_SUBCLASS   (1UL << 26)
#define Py_TPFLAGS_DICT_SUBCLASS    (1UL << 29)
#define Py_TPFLAGS_LONG_SUBCLASS    (1UL << 24)
#define Py_TPFLAGS_BYTES_SUBCLASS   (1UL << 27)
#define Py_TPFLAGS_UNICODE_SUBCLASS (1UL << 28)
#define Py_TPFLAGS_TYPE_SUBCLASS    (1UL << 31)
#define Py_TPFLAGS_BASE_EXC_SUBCLASS (1UL << 30)

PyAPI_FUNC(PyObject *) PyType_FromSpec(PyType_Spec *spec);
PyAPI_FUNC(PyObject *) PyType_FromSpecWithBases(PyType_Spec *spec, PyObject *bases);
PyAPI_FUNC(PyObject *) PyType_FromModuleAndSpec(PyObject *module, PyType_Spec *spec, PyObject *bases);
PyAPI_FUNC(int) PyType_Ready(PyTypeObject *type);
PyAPI_FUNC(int) PyType_IsSubtype(PyTypeObject *a, PyTypeObject *b);
PyAPI_FUNC(int) PyObject_TypeCheck(PyObject *o, PyTypeObject *t);
PyAPI_FUNC(const char *) PyType_GetName(PyTypeObject *t);

/* ------------------------------------------------------------------
 * Object protocol (object.h / abstract.h subset).
 * ------------------------------------------------------------------ */

PyAPI_FUNC(PyObject *) PyObject_Repr(PyObject *o);
PyAPI_FUNC(PyObject *) PyObject_Str(PyObject *o);
PyAPI_FUNC(PyObject *) PyObject_ASCII(PyObject *o);
PyAPI_FUNC(PyObject *) PyObject_GetAttr(PyObject *o, PyObject *attr);
PyAPI_FUNC(PyObject *) PyObject_GetAttrString(PyObject *o, const char *attr);
PyAPI_FUNC(int) PyObject_SetAttr(PyObject *o, PyObject *attr, PyObject *value);
PyAPI_FUNC(int) PyObject_SetAttrString(PyObject *o, const char *attr, PyObject *value);
PyAPI_FUNC(int) PyObject_HasAttr(PyObject *o, PyObject *attr);
PyAPI_FUNC(int) PyObject_HasAttrString(PyObject *o, const char *attr);
PyAPI_FUNC(int) PyObject_DelAttrString(PyObject *o, const char *attr);
PyAPI_FUNC(PyObject *) PyObject_Call(PyObject *callable, PyObject *args, PyObject *kwargs);
PyAPI_FUNC(PyObject *) PyObject_CallObject(PyObject *callable, PyObject *args);
PyAPI_FUNC(PyObject *) PyObject_CallNoArgs(PyObject *callable);
PyAPI_FUNC(PyObject *) PyObject_CallOneArg(PyObject *callable, PyObject *arg);
PyAPI_FUNC(PyObject *) PyObject_CallFunction(PyObject *callable, const char *fmt, ...);
PyAPI_FUNC(PyObject *) PyObject_CallMethod(PyObject *o, const char *name, const char *fmt, ...);
PyAPI_FUNC(PyObject *) PyObject_CallMethodObjArgs(PyObject *o, PyObject *name, ...);
PyAPI_FUNC(PyObject *) PyObject_CallFunctionObjArgs(PyObject *callable, ...);
PyAPI_FUNC(int) PyObject_IsTrue(PyObject *o);
PyAPI_FUNC(int) PyObject_Not(PyObject *o);
PyAPI_FUNC(int) PyObject_RichCompareBool(PyObject *a, PyObject *b, int op);
PyAPI_FUNC(PyObject *) PyObject_RichCompare(PyObject *a, PyObject *b, int op);
PyAPI_FUNC(Py_hash_t) PyObject_Hash(PyObject *o);
PyAPI_FUNC(PyObject *) PyObject_Type(PyObject *o);
PyAPI_FUNC(int) PyObject_IsInstance(PyObject *o, PyObject *cls);
PyAPI_FUNC(int) PyObject_IsSubclass(PyObject *o, PyObject *cls);
PyAPI_FUNC(Py_ssize_t) PyObject_Length(PyObject *o);
PyAPI_FUNC(Py_ssize_t) PyObject_Size(PyObject *o);
PyAPI_FUNC(PyObject *) PyObject_GetItem(PyObject *o, PyObject *key);
PyAPI_FUNC(int) PyObject_SetItem(PyObject *o, PyObject *key, PyObject *v);
PyAPI_FUNC(int) PyObject_DelItem(PyObject *o, PyObject *key);
PyAPI_FUNC(PyObject *) PyObject_Dir(PyObject *o);
PyAPI_FUNC(PyObject *) PyObject_GetIter(PyObject *o);
PyAPI_FUNC(PyObject *) PyIter_Next(PyObject *o);

/* ------------------------------------------------------------------
 * Number protocol.
 * ------------------------------------------------------------------ */

PyAPI_FUNC(PyObject *) PyNumber_Add(PyObject *a, PyObject *b);
PyAPI_FUNC(PyObject *) PyNumber_Subtract(PyObject *a, PyObject *b);
PyAPI_FUNC(PyObject *) PyNumber_Multiply(PyObject *a, PyObject *b);
PyAPI_FUNC(PyObject *) PyNumber_TrueDivide(PyObject *a, PyObject *b);
PyAPI_FUNC(PyObject *) PyNumber_FloorDivide(PyObject *a, PyObject *b);
PyAPI_FUNC(PyObject *) PyNumber_Remainder(PyObject *a, PyObject *b);
PyAPI_FUNC(PyObject *) PyNumber_Negative(PyObject *o);
PyAPI_FUNC(PyObject *) PyNumber_Positive(PyObject *o);
PyAPI_FUNC(PyObject *) PyNumber_Absolute(PyObject *o);
PyAPI_FUNC(PyObject *) PyNumber_Long(PyObject *o);
PyAPI_FUNC(PyObject *) PyNumber_Float(PyObject *o);
PyAPI_FUNC(PyObject *) PyNumber_Power(PyObject *base, PyObject *exp, PyObject *mod);
PyAPI_FUNC(int) PyNumber_Check(PyObject *o);

/* ------------------------------------------------------------------
 * Sequence / mapping protocols.
 * ------------------------------------------------------------------ */

PyAPI_FUNC(int) PySequence_Check(PyObject *o);
PyAPI_FUNC(Py_ssize_t) PySequence_Length(PyObject *o);
PyAPI_FUNC(Py_ssize_t) PySequence_Size(PyObject *o);
PyAPI_FUNC(PyObject *) PySequence_GetItem(PyObject *o, Py_ssize_t i);
PyAPI_FUNC(int) PySequence_SetItem(PyObject *o, Py_ssize_t i, PyObject *v);
PyAPI_FUNC(int) PySequence_Contains(PyObject *o, PyObject *v);
PyAPI_FUNC(PyObject *) PySequence_List(PyObject *o);
PyAPI_FUNC(PyObject *) PySequence_Tuple(PyObject *o);
PyAPI_FUNC(int) PyMapping_Check(PyObject *o);
PyAPI_FUNC(Py_ssize_t) PyMapping_Length(PyObject *o);
PyAPI_FUNC(Py_ssize_t) PyMapping_Size(PyObject *o);
PyAPI_FUNC(PyObject *) PyMapping_GetItemString(PyObject *o, const char *key);
PyAPI_FUNC(int) PyMapping_HasKey(PyObject *o, PyObject *key);
PyAPI_FUNC(int) PyMapping_HasKeyString(PyObject *o, const char *key);
PyAPI_FUNC(int) PyMapping_SetItemString(PyObject *o, const char *key, PyObject *v);

/* ------------------------------------------------------------------
 * Long (int).
 * ------------------------------------------------------------------ */

PyAPI_FUNC(PyObject *) PyLong_FromLong(long v);
PyAPI_FUNC(PyObject *) PyLong_FromUnsignedLong(unsigned long v);
PyAPI_FUNC(PyObject *) PyLong_FromLongLong(long long v);
PyAPI_FUNC(PyObject *) PyLong_FromUnsignedLongLong(unsigned long long v);
PyAPI_FUNC(PyObject *) PyLong_FromSsize_t(Py_ssize_t v);
PyAPI_FUNC(PyObject *) PyLong_FromSize_t(size_t v);
PyAPI_FUNC(PyObject *) PyLong_FromDouble(double v);
PyAPI_FUNC(PyObject *) PyLong_FromString(const char *s, char **end, int base);
PyAPI_FUNC(long) PyLong_AsLong(PyObject *o);
PyAPI_FUNC(long long) PyLong_AsLongLong(PyObject *o);
PyAPI_FUNC(unsigned long) PyLong_AsUnsignedLong(PyObject *o);
PyAPI_FUNC(unsigned long long) PyLong_AsUnsignedLongLong(PyObject *o);
PyAPI_FUNC(Py_ssize_t) PyLong_AsSsize_t(PyObject *o);
PyAPI_FUNC(double) PyLong_AsDouble(PyObject *o);
PyAPI_FUNC(int) PyLong_Check(PyObject *o);

/* ------------------------------------------------------------------
 * Float, Bool, Complex.
 * ------------------------------------------------------------------ */

PyAPI_FUNC(PyObject *) PyFloat_FromDouble(double v);
PyAPI_FUNC(double) PyFloat_AsDouble(PyObject *o);
PyAPI_FUNC(int) PyFloat_Check(PyObject *o);

PyAPI_FUNC(PyObject *) PyBool_FromLong(long v);
PyAPI_FUNC(int) PyBool_Check(PyObject *o);

PyAPI_FUNC(PyObject *) PyComplex_FromDoubles(double real, double imag);
PyAPI_FUNC(double) PyComplex_RealAsDouble(PyObject *o);
PyAPI_FUNC(double) PyComplex_ImagAsDouble(PyObject *o);
PyAPI_FUNC(int) PyComplex_Check(PyObject *o);

/* ------------------------------------------------------------------
 * Unicode (str), Bytes, ByteArray.
 * ------------------------------------------------------------------ */

PyAPI_FUNC(PyObject *) PyUnicode_FromString(const char *s);
PyAPI_FUNC(PyObject *) PyUnicode_FromStringAndSize(const char *s, Py_ssize_t n);
PyAPI_FUNC(PyObject *) PyUnicode_FromFormat(const char *fmt, ...);
PyAPI_FUNC(PyObject *) PyUnicode_FromFormatV(const char *fmt, va_list args);
PyAPI_FUNC(const char *) PyUnicode_AsUTF8(PyObject *o);
PyAPI_FUNC(const char *) PyUnicode_AsUTF8AndSize(PyObject *o, Py_ssize_t *size);
PyAPI_FUNC(PyObject *) PyUnicode_AsEncodedString(PyObject *o, const char *enc, const char *errors);
PyAPI_FUNC(PyObject *) PyUnicode_AsUTF8String(PyObject *o);
PyAPI_FUNC(Py_ssize_t) PyUnicode_GetLength(PyObject *o);
PyAPI_FUNC(PyObject *) PyUnicode_Concat(PyObject *a, PyObject *b);
PyAPI_FUNC(int) PyUnicode_Check(PyObject *o);
PyAPI_FUNC(int) PyUnicode_CompareWithASCIIString(PyObject *o, const char *s);

PyAPI_FUNC(PyObject *) PyBytes_FromString(const char *s);
PyAPI_FUNC(PyObject *) PyBytes_FromStringAndSize(const char *s, Py_ssize_t n);
PyAPI_FUNC(char *) PyBytes_AsString(PyObject *o);
PyAPI_FUNC(int) PyBytes_AsStringAndSize(PyObject *o, char **buffer, Py_ssize_t *length);
PyAPI_FUNC(Py_ssize_t) PyBytes_Size(PyObject *o);
PyAPI_FUNC(int) PyBytes_Check(PyObject *o);

PyAPI_FUNC(PyObject *) PyByteArray_FromStringAndSize(const char *s, Py_ssize_t n);
PyAPI_FUNC(char *) PyByteArray_AsString(PyObject *o);
PyAPI_FUNC(Py_ssize_t) PyByteArray_Size(PyObject *o);
PyAPI_FUNC(int) PyByteArray_Check(PyObject *o);

/* ------------------------------------------------------------------
 * List, Tuple, Dict, Set.
 * ------------------------------------------------------------------ */

PyAPI_FUNC(PyObject *) PyList_New(Py_ssize_t size);
PyAPI_FUNC(int) PyList_Append(PyObject *list, PyObject *item);
PyAPI_FUNC(int) PyList_Insert(PyObject *list, Py_ssize_t pos, PyObject *item);
PyAPI_FUNC(int) PyList_SetItem(PyObject *list, Py_ssize_t pos, PyObject *item);
PyAPI_FUNC(PyObject *) PyList_GetItem(PyObject *list, Py_ssize_t pos);
PyAPI_FUNC(Py_ssize_t) PyList_Size(PyObject *list);
PyAPI_FUNC(PyObject *) PyList_AsTuple(PyObject *list);
PyAPI_FUNC(int) PyList_Reverse(PyObject *list);
PyAPI_FUNC(int) PyList_Sort(PyObject *list);
PyAPI_FUNC(int) PyList_Check(PyObject *o);

PyAPI_FUNC(PyObject *) PyTuple_New(Py_ssize_t size);
PyAPI_FUNC(int) PyTuple_SetItem(PyObject *tuple, Py_ssize_t pos, PyObject *item);
PyAPI_FUNC(PyObject *) PyTuple_GetItem(PyObject *tuple, Py_ssize_t pos);
PyAPI_FUNC(Py_ssize_t) PyTuple_Size(PyObject *tuple);
PyAPI_FUNC(PyObject *) PyTuple_Pack(Py_ssize_t n, ...);
PyAPI_FUNC(PyObject *) PyTuple_GetSlice(PyObject *tuple, Py_ssize_t lo, Py_ssize_t hi);
PyAPI_FUNC(int) PyTuple_Check(PyObject *o);

PyAPI_FUNC(PyObject *) PyDict_New(void);
PyAPI_FUNC(int) PyDict_SetItem(PyObject *d, PyObject *k, PyObject *v);
PyAPI_FUNC(int) PyDict_SetItemString(PyObject *d, const char *k, PyObject *v);
PyAPI_FUNC(PyObject *) PyDict_GetItem(PyObject *d, PyObject *k);
PyAPI_FUNC(PyObject *) PyDict_GetItemString(PyObject *d, const char *k);
PyAPI_FUNC(int) PyDict_DelItem(PyObject *d, PyObject *k);
PyAPI_FUNC(int) PyDict_DelItemString(PyObject *d, const char *k);
PyAPI_FUNC(int) PyDict_Contains(PyObject *d, PyObject *k);
PyAPI_FUNC(Py_ssize_t) PyDict_Size(PyObject *d);
PyAPI_FUNC(int) PyDict_Next(PyObject *d, Py_ssize_t *ppos, PyObject **pkey, PyObject **pvalue);
PyAPI_FUNC(PyObject *) PyDict_Keys(PyObject *d);
PyAPI_FUNC(PyObject *) PyDict_Values(PyObject *d);
PyAPI_FUNC(PyObject *) PyDict_Items(PyObject *d);
PyAPI_FUNC(PyObject *) PyDict_Copy(PyObject *d);
PyAPI_FUNC(int) PyDict_Update(PyObject *d, PyObject *other);
PyAPI_FUNC(int) PyDict_Merge(PyObject *a, PyObject *b, int override_);
PyAPI_FUNC(int) PyDict_Clear(PyObject *d);
PyAPI_FUNC(int) PyDict_Check(PyObject *o);

PyAPI_FUNC(PyObject *) PySet_New(PyObject *iterable);
PyAPI_FUNC(PyObject *) PyFrozenSet_New(PyObject *iterable);
PyAPI_FUNC(int) PySet_Add(PyObject *s, PyObject *item);
PyAPI_FUNC(int) PySet_Contains(PyObject *s, PyObject *item);
PyAPI_FUNC(int) PySet_Discard(PyObject *s, PyObject *item);
PyAPI_FUNC(Py_ssize_t) PySet_Size(PyObject *s);
PyAPI_FUNC(int) PySet_Check(PyObject *o);
PyAPI_FUNC(int) PyFrozenSet_Check(PyObject *o);

/* ------------------------------------------------------------------
 * Module + import.
 * ------------------------------------------------------------------ */

#define PYTHON_API_VERSION 1013
#define PYTHON_ABI_VERSION 3

#define PyModuleDef_HEAD_INIT { { 1, NULL }, NULL, 0, NULL },

typedef struct PyModuleDef_Slot {
    int slot;
    void *value;
} PyModuleDef_Slot;

#define Py_mod_create  1
#define Py_mod_exec    2

typedef struct PyModuleDef_Base {
    PyObject_HEAD
    PyObject *(*m_init)(void);
    Py_ssize_t m_index;
    PyObject *m_copy;
} PyModuleDef_Base;

typedef struct PyModuleDef {
    PyModuleDef_Base m_base;
    const char *m_name;
    const char *m_doc;
    Py_ssize_t m_size;
    PyMethodDef *m_methods;
    PyModuleDef_Slot *m_slots;
    void *m_traverse;
    void *m_clear;
    void *m_free;
} PyModuleDef;

PyAPI_FUNC(PyObject *) PyModule_Create2(PyModuleDef *def, int api_version);
PyAPI_FUNC(PyObject *) PyModuleDef_Init(PyModuleDef *def);
PyAPI_FUNC(int) PyModule_AddObject(PyObject *m, const char *name, PyObject *value);
PyAPI_FUNC(int) PyModule_AddObjectRef(PyObject *m, const char *name, PyObject *value);
PyAPI_FUNC(int) PyModule_AddStringConstant(PyObject *m, const char *name, const char *value);
PyAPI_FUNC(int) PyModule_AddIntConstant(PyObject *m, const char *name, long value);
PyAPI_FUNC(int) PyModule_AddType(PyObject *m, PyTypeObject *type);
PyAPI_FUNC(int) PyModule_AddFunctions(PyObject *m, PyMethodDef *defs);
PyAPI_FUNC(PyObject *) PyModule_GetDict(PyObject *m);
PyAPI_FUNC(const char *) PyModule_GetName(PyObject *m);
PyAPI_FUNC(int) PyModule_Check(PyObject *o);

#define PyModule_Create(def) PyModule_Create2((def), PYTHON_API_VERSION)

PyAPI_FUNC(PyObject *) PyImport_ImportModule(const char *name);
PyAPI_FUNC(PyObject *) PyImport_AddModule(const char *name);
PyAPI_FUNC(PyObject *) PyImport_GetModule(PyObject *name);

/* ------------------------------------------------------------------
 * Argument parsing & value building.
 * ------------------------------------------------------------------ */

PyAPI_FUNC(int) PyArg_ParseTuple(PyObject *args, const char *fmt, ...);
PyAPI_FUNC(int) PyArg_ParseTupleAndKeywords(PyObject *args, PyObject *kwargs, const char *fmt, char **kwlist, ...);
PyAPI_FUNC(int) PyArg_VaParse(PyObject *args, const char *fmt, va_list va);
PyAPI_FUNC(int) PyArg_VaParseTupleAndKeywords(PyObject *args, PyObject *kwargs, const char *fmt, char **kwlist, va_list va);
PyAPI_FUNC(int) PyArg_Parse(PyObject *args, const char *fmt, ...);
PyAPI_FUNC(int) PyArg_UnpackTuple(PyObject *args, const char *name, Py_ssize_t min, Py_ssize_t max, ...);
PyAPI_FUNC(PyObject *) Py_BuildValue(const char *fmt, ...);
PyAPI_FUNC(PyObject *) Py_VaBuildValue(const char *fmt, va_list args);

/* ------------------------------------------------------------------
 * Errors + exception statics.
 * ------------------------------------------------------------------ */

PyAPI_FUNC(void) PyErr_SetString(PyObject *type, const char *msg);
PyAPI_FUNC(PyObject *) PyErr_Format(PyObject *type, const char *fmt, ...);
PyAPI_FUNC(PyObject *) PyErr_FormatV(PyObject *type, const char *fmt, va_list args);
PyAPI_FUNC(void) PyErr_SetObject(PyObject *type, PyObject *value);
PyAPI_FUNC(void) PyErr_SetNone(PyObject *type);
PyAPI_FUNC(PyObject *) PyErr_Occurred(void);
PyAPI_FUNC(void) PyErr_Clear(void);
PyAPI_FUNC(void) PyErr_Print(void);
PyAPI_FUNC(void) PyErr_PrintEx(int set_sys_last_vars);
PyAPI_FUNC(void) PyErr_Fetch(PyObject **ptype, PyObject **pvalue, PyObject **ptraceback);
PyAPI_FUNC(void) PyErr_Restore(PyObject *type, PyObject *value, PyObject *traceback);
PyAPI_FUNC(int) PyErr_GivenExceptionMatches(PyObject *given, PyObject *exc);
PyAPI_FUNC(int) PyErr_ExceptionMatches(PyObject *exc);
PyAPI_FUNC(void) PyErr_NormalizeException(PyObject **exc, PyObject **val, PyObject **tb);
PyAPI_FUNC(PyObject *) PyErr_NoMemory(void);
PyAPI_FUNC(int) PyErr_BadArgument(void);
PyAPI_FUNC(void) PyErr_BadInternalCall(void);
PyAPI_FUNC(int) PyErr_WarnEx(PyObject *category, const char *msg, Py_ssize_t stacklevel);

PyAPI_FUNC(PyObject *) PyErr_NewException(const char *name, PyObject *base, PyObject *dict);
PyAPI_FUNC(PyObject *) PyErr_NewExceptionWithDoc(const char *name, const char *doc, PyObject *base, PyObject *dict);

PyAPI_DATA(PyObject *) PyExc_BaseException;
PyAPI_DATA(PyObject *) PyExc_Exception;
PyAPI_DATA(PyObject *) PyExc_ArithmeticError;
PyAPI_DATA(PyObject *) PyExc_AssertionError;
PyAPI_DATA(PyObject *) PyExc_AttributeError;
PyAPI_DATA(PyObject *) PyExc_BufferError;
PyAPI_DATA(PyObject *) PyExc_EOFError;
PyAPI_DATA(PyObject *) PyExc_FloatingPointError;
PyAPI_DATA(PyObject *) PyExc_GeneratorExit;
PyAPI_DATA(PyObject *) PyExc_ImportError;
PyAPI_DATA(PyObject *) PyExc_IndentationError;
PyAPI_DATA(PyObject *) PyExc_IndexError;
PyAPI_DATA(PyObject *) PyExc_KeyError;
PyAPI_DATA(PyObject *) PyExc_KeyboardInterrupt;
PyAPI_DATA(PyObject *) PyExc_LookupError;
PyAPI_DATA(PyObject *) PyExc_MemoryError;
PyAPI_DATA(PyObject *) PyExc_ModuleNotFoundError;
PyAPI_DATA(PyObject *) PyExc_NameError;
PyAPI_DATA(PyObject *) PyExc_NotImplementedError;
PyAPI_DATA(PyObject *) PyExc_OSError;
PyAPI_DATA(PyObject *) PyExc_OverflowError;
PyAPI_DATA(PyObject *) PyExc_RecursionError;
PyAPI_DATA(PyObject *) PyExc_ReferenceError;
PyAPI_DATA(PyObject *) PyExc_RuntimeError;
PyAPI_DATA(PyObject *) PyExc_StopAsyncIteration;
PyAPI_DATA(PyObject *) PyExc_StopIteration;
PyAPI_DATA(PyObject *) PyExc_SyntaxError;
PyAPI_DATA(PyObject *) PyExc_SystemError;
PyAPI_DATA(PyObject *) PyExc_SystemExit;
PyAPI_DATA(PyObject *) PyExc_TabError;
PyAPI_DATA(PyObject *) PyExc_TimeoutError;
PyAPI_DATA(PyObject *) PyExc_TypeError;
PyAPI_DATA(PyObject *) PyExc_UnboundLocalError;
PyAPI_DATA(PyObject *) PyExc_UnicodeDecodeError;
PyAPI_DATA(PyObject *) PyExc_UnicodeEncodeError;
PyAPI_DATA(PyObject *) PyExc_UnicodeError;
PyAPI_DATA(PyObject *) PyExc_UnicodeTranslateError;
PyAPI_DATA(PyObject *) PyExc_ValueError;
PyAPI_DATA(PyObject *) PyExc_ZeroDivisionError;
PyAPI_DATA(PyObject *) PyExc_BlockingIOError;
PyAPI_DATA(PyObject *) PyExc_BrokenPipeError;
PyAPI_DATA(PyObject *) PyExc_ChildProcessError;
PyAPI_DATA(PyObject *) PyExc_ConnectionAbortedError;
PyAPI_DATA(PyObject *) PyExc_ConnectionError;
PyAPI_DATA(PyObject *) PyExc_ConnectionRefusedError;
PyAPI_DATA(PyObject *) PyExc_ConnectionResetError;
PyAPI_DATA(PyObject *) PyExc_FileExistsError;
PyAPI_DATA(PyObject *) PyExc_FileNotFoundError;
PyAPI_DATA(PyObject *) PyExc_InterruptedError;
PyAPI_DATA(PyObject *) PyExc_IsADirectoryError;
PyAPI_DATA(PyObject *) PyExc_NotADirectoryError;
PyAPI_DATA(PyObject *) PyExc_PermissionError;
PyAPI_DATA(PyObject *) PyExc_ProcessLookupError;
PyAPI_DATA(PyObject *) PyExc_Warning;
PyAPI_DATA(PyObject *) PyExc_UserWarning;
PyAPI_DATA(PyObject *) PyExc_DeprecationWarning;
PyAPI_DATA(PyObject *) PyExc_PendingDeprecationWarning;
PyAPI_DATA(PyObject *) PyExc_SyntaxWarning;
PyAPI_DATA(PyObject *) PyExc_RuntimeWarning;
PyAPI_DATA(PyObject *) PyExc_FutureWarning;
PyAPI_DATA(PyObject *) PyExc_ImportWarning;
PyAPI_DATA(PyObject *) PyExc_UnicodeWarning;
PyAPI_DATA(PyObject *) PyExc_BytesWarning;
PyAPI_DATA(PyObject *) PyExc_ResourceWarning;

/* ------------------------------------------------------------------
 * Memory.
 * ------------------------------------------------------------------ */

PyAPI_FUNC(void *) PyMem_Malloc(size_t n);
PyAPI_FUNC(void *) PyMem_Calloc(size_t nelem, size_t elsize);
PyAPI_FUNC(void *) PyMem_Realloc(void *p, size_t n);
PyAPI_FUNC(void) PyMem_Free(void *p);
PyAPI_FUNC(void *) PyMem_RawMalloc(size_t n);
PyAPI_FUNC(void *) PyMem_RawCalloc(size_t nelem, size_t elsize);
PyAPI_FUNC(void *) PyMem_RawRealloc(void *p, size_t n);
PyAPI_FUNC(void) PyMem_RawFree(void *p);
PyAPI_FUNC(void *) PyObject_Malloc(size_t n);
PyAPI_FUNC(void *) PyObject_Calloc(size_t nelem, size_t elsize);
PyAPI_FUNC(void *) PyObject_Realloc(void *p, size_t n);
PyAPI_FUNC(void) PyObject_Free(void *p);

/* ------------------------------------------------------------------
 * GIL / lifecycle (mostly stubs — WeavePy is single-threaded).
 * ------------------------------------------------------------------ */

typedef int PyGILState_STATE;

PyAPI_FUNC(PyGILState_STATE) PyGILState_Ensure(void);
PyAPI_FUNC(void) PyGILState_Release(PyGILState_STATE state);
PyAPI_FUNC(int) PyGILState_Check(void);

typedef struct _ts PyThreadState;

PyAPI_FUNC(PyThreadState *) PyThreadState_Get(void);
PyAPI_FUNC(PyThreadState *) PyEval_SaveThread(void);
PyAPI_FUNC(void) PyEval_RestoreThread(PyThreadState *tstate);

PyAPI_FUNC(void) Py_Initialize(void);
PyAPI_FUNC(void) Py_InitializeEx(int initsigs);
PyAPI_FUNC(int) Py_FinalizeEx(void);
PyAPI_FUNC(void) Py_Finalize(void);
PyAPI_FUNC(int) Py_IsInitialized(void);
PyAPI_FUNC(const char *) Py_GetVersion(void);
PyAPI_FUNC(const char *) Py_GetCompiler(void);
PyAPI_FUNC(const char *) Py_GetCopyright(void);
PyAPI_FUNC(const char *) Py_GetPlatform(void);
PyAPI_FUNC(const char *) Py_GetBuildInfo(void);
PyAPI_FUNC(int) Py_AtExit(void (*func)(void));

/* ------------------------------------------------------------------
 * Buffer protocol (minimum surface needed for numpy / array-style
 * extensions to publish data).
 * ------------------------------------------------------------------ */

#define PyBUF_SIMPLE        0x0000
#define PyBUF_WRITABLE      0x0001
#define PyBUF_FORMAT        0x0004
#define PyBUF_ND            0x0008
#define PyBUF_STRIDES       (0x0010 | PyBUF_ND)
#define PyBUF_C_CONTIGUOUS  (0x0020 | PyBUF_STRIDES)
#define PyBUF_F_CONTIGUOUS  (0x0040 | PyBUF_STRIDES)
#define PyBUF_ANY_CONTIGUOUS (0x0080 | PyBUF_STRIDES)
#define PyBUF_INDIRECT      (0x0100 | PyBUF_STRIDES)
#define PyBUF_CONTIG        (PyBUF_ND | PyBUF_WRITABLE)
#define PyBUF_CONTIG_RO     PyBUF_ND
#define PyBUF_FULL          (PyBUF_INDIRECT | PyBUF_FORMAT | PyBUF_WRITABLE)
#define PyBUF_FULL_RO       (PyBUF_INDIRECT | PyBUF_FORMAT)

typedef struct bufferinfo {
    void *buf;
    PyObject *obj;
    Py_ssize_t len;
    Py_ssize_t itemsize;
    int readonly;
    int ndim;
    char *format;
    Py_ssize_t *shape;
    Py_ssize_t *strides;
    Py_ssize_t *suboffsets;
    void *internal;
} Py_buffer;

PyAPI_FUNC(int) PyObject_GetBuffer(PyObject *exporter, Py_buffer *view, int flags);
PyAPI_FUNC(void) PyBuffer_Release(Py_buffer *view);
PyAPI_FUNC(int) PyObject_CheckBuffer(PyObject *o);

/* ------------------------------------------------------------------
 * Iteration helpers.
 * ------------------------------------------------------------------ */

PyAPI_FUNC(PyObject *) PyIter_NextItem(PyObject *iter, int *finished);

/* ------------------------------------------------------------------
 * Capsule (opaque void* wrapper for extension-level helpers).
 * ------------------------------------------------------------------ */

typedef void (*PyCapsule_Destructor)(PyObject *);

PyAPI_FUNC(PyObject *) PyCapsule_New(void *pointer, const char *name, PyCapsule_Destructor destructor);
PyAPI_FUNC(void *) PyCapsule_GetPointer(PyObject *capsule, const char *name);
PyAPI_FUNC(const char *) PyCapsule_GetName(PyObject *capsule);
PyAPI_FUNC(int) PyCapsule_IsValid(PyObject *capsule, const char *name);
PyAPI_FUNC(int) PyCapsule_SetPointer(PyObject *capsule, void *pointer);

/* ------------------------------------------------------------------
 * Slice helpers.
 * ------------------------------------------------------------------ */

PyAPI_FUNC(PyObject *) PySlice_New(PyObject *start, PyObject *stop, PyObject *step);
PyAPI_FUNC(int) PySlice_Check(PyObject *o);

/* ------------------------------------------------------------------
 * Convenience macros so extension authors can `#include <Python.h>`
 * exactly as they would on CPython.
 * ------------------------------------------------------------------ */

/* Many CPython headers wrap their declarations in this macro for
 * conditional compilation; expose a no-op definition so legacy
 * source builds. */
#define _Py_DEPRECATED_EXTERNALLY(...)

#ifdef __cplusplus
}
#endif

#endif /* WEAVEPY_PYTHON_H */
