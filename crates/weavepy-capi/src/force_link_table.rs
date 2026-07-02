//! Force-link table.
//!
//! `cargo test`-built binaries (and any embedder that doesn't
//! itself call into the C-API) would otherwise dead-strip every
//! `#[no_mangle] extern "C"` symbol defined in this crate, which
//! makes the resulting executable unable to satisfy the symbol
//! references in dlopen'd extension `.so` / `.dylib` / `.pyd`
//! files.
//!
//! We solve this with a `#[used] static` array of function pointers
//! pointing at every entry on the C-API surface. The `#[used]`
//! attribute prevents the optimiser from eliding the table; the
//! pointer references inside force the linker to keep the symbols
//! alive and visible in the dynamic symbol table of the final
//! binary.

use core::ffi::c_void;

use crate::abstract_ as ab;
use crate::argparse;
use crate::buffer;
use crate::capsule;
use crate::code_obj;
use crate::containers;
use crate::datetime_api as dt;
use crate::errors;
use crate::gc_bridge;
use crate::genericalloc;
use crate::lifecycle;
use crate::memory;
use crate::memoryview;
use crate::module;
use crate::monitoring;
use crate::numbers;
use crate::object;
use crate::pystate;
use crate::singletons;
use crate::slice;
use crate::strings;
use crate::types;
use crate::vectorcall;
use crate::wave4;
use crate::wave5;
use crate::wave5_pandas as w5p;

// The variadic helpers live in `varargs.c` and would otherwise be
// dead-stripped from the host binary, since nothing on the Rust
// side normally calls them. Reference them through `extern "C"`
// so the linker keeps both the symbols and the static archive
// they live in.
extern "C" {
    fn PyArg_ParseTuple(
        args: *mut crate::object::PyObject,
        fmt: *const core::ffi::c_char,
        ...
    ) -> core::ffi::c_int;
    fn PyArg_Parse(
        args: *mut crate::object::PyObject,
        fmt: *const core::ffi::c_char,
        ...
    ) -> core::ffi::c_int;
    fn PyArg_VaParse(
        args: *mut crate::object::PyObject,
        fmt: *const core::ffi::c_char,
        va: *mut core::ffi::c_void,
    ) -> core::ffi::c_int;
    fn PyArg_ParseTupleAndKeywords(
        args: *mut crate::object::PyObject,
        kwargs: *mut crate::object::PyObject,
        fmt: *const core::ffi::c_char,
        kwlist: *mut *mut core::ffi::c_char,
        ...
    ) -> core::ffi::c_int;
    fn PyArg_VaParseTupleAndKeywords(
        args: *mut crate::object::PyObject,
        kwargs: *mut crate::object::PyObject,
        fmt: *const core::ffi::c_char,
        kwlist: *mut *mut core::ffi::c_char,
        va: *mut core::ffi::c_void,
    ) -> core::ffi::c_int;
    fn PyArg_UnpackTuple(
        args: *mut crate::object::PyObject,
        name: *const core::ffi::c_char,
        min_count: isize,
        max_count: isize,
        ...
    ) -> core::ffi::c_int;
    fn Py_BuildValue(fmt: *const core::ffi::c_char, ...) -> *mut crate::object::PyObject;
    fn Py_VaBuildValue(
        fmt: *const core::ffi::c_char,
        va: *mut core::ffi::c_void,
    ) -> *mut crate::object::PyObject;
    fn PyTuple_Pack(n: isize, ...) -> *mut crate::object::PyObject;
    fn PyUnicode_FromFormat(fmt: *const core::ffi::c_char, ...) -> *mut crate::object::PyObject;
    fn PyUnicode_FromFormatV(
        fmt: *const core::ffi::c_char,
        va: *mut core::ffi::c_void,
    ) -> *mut crate::object::PyObject;
    fn PyErr_Format(
        ty: *mut crate::object::PyObject,
        fmt: *const core::ffi::c_char,
        ...
    ) -> *mut crate::object::PyObject;
    fn PyErr_FormatV(
        ty: *mut crate::object::PyObject,
        fmt: *const core::ffi::c_char,
        va: *mut core::ffi::c_void,
    ) -> *mut crate::object::PyObject;
    fn PyObject_CallFunction(
        callable: *mut crate::object::PyObject,
        fmt: *const core::ffi::c_char,
        ...
    ) -> *mut crate::object::PyObject;
    fn PyObject_CallMethod(
        target: *mut crate::object::PyObject,
        name: *const core::ffi::c_char,
        fmt: *const core::ffi::c_char,
        ...
    ) -> *mut crate::object::PyObject;
    fn PyObject_CallMethodObjArgs(
        target: *mut crate::object::PyObject,
        name: *mut crate::object::PyObject,
        ...
    ) -> *mut crate::object::PyObject;
    fn PyObject_CallFunctionObjArgs(
        callable: *mut crate::object::PyObject,
        ...
    ) -> *mut crate::object::PyObject;
    // RFC 0046 (wave 4): variadic tail numpy links, defined in varargs.c.
    fn PyOS_snprintf(
        str: *mut core::ffi::c_char,
        size: usize,
        fmt: *const core::ffi::c_char,
        ...
    ) -> core::ffi::c_int;
    fn PyErr_WarnFormat(
        category: *mut crate::object::PyObject,
        stack_level: isize,
        fmt: *const core::ffi::c_char,
        ...
    ) -> core::ffi::c_int;
}

macro_rules! addr {
    ($f:expr) => {
        FnPtr($f as *const c_void)
    };
}

/// Take the address of a `#[no_mangle] pub static` (or `static mut`)
/// global. Unlike functions, statics aren't naturally referenced by
/// any code path on the Rust side, so the linker is free to dead-strip
/// them on Linux. dlopen'd extension modules expect to resolve symbols
/// like `PyExc_RuntimeError` and `_Py_NoneStruct` against the host
/// binary, so we have to root every one of them through this table.
///
/// Note: the `mut` arm must come first because macro_rules picks the
/// first matching arm without backtracking; a leading `mut` token
/// would otherwise fail to parse against a bare `path` fragment.
macro_rules! addr_static {
    (mut $s:path) => {{
        // SAFETY: We only ever take the address of the cell; taking
        // the address of a `static mut` is sound and doesn't require
        // synchronisation.
        FnPtr(unsafe { core::ptr::addr_of_mut!($s) } as *const c_void)
    }};
    ($s:path) => {{
        // SAFETY: Same as above; we never dereference.
        FnPtr(unsafe { core::ptr::addr_of!($s) } as *const c_void)
    }};
}

/// Sync wrapper so we can stash function pointers in a `static`.
/// Function pointers are pure addresses, no interior state, so this
/// is sound.
#[derive(Copy, Clone)]
#[repr(transparent)]
struct FnPtr(*const c_void);

unsafe impl Sync for FnPtr {}

#[used]
#[allow(clippy::ptr_as_ptr)]
static FORCE_LINK: &[FnPtr] = &[
    // object.rs
    addr!(object::Py_IncRef),
    addr!(object::Py_DecRef),
    addr!(object::Py_NewRef),
    addr!(object::Py_XNewRef),
    addr!(object::_Py_Dealloc),
    addr!(object::_PyWeavePy_Dealloc),
    // numbers.rs
    addr!(numbers::PyLong_FromLong),
    addr!(numbers::PyLong_FromLongLong),
    addr!(numbers::PyLong_FromUnsignedLong),
    addr!(numbers::PyLong_FromUnsignedLongLong),
    addr!(numbers::PyLong_FromSize_t),
    addr!(numbers::PyLong_FromSsize_t),
    addr!(numbers::PyLong_FromDouble),
    addr!(numbers::PyLong_FromString),
    addr!(numbers::PyLong_AsLong),
    addr!(numbers::PyLong_AsLongLong),
    addr!(numbers::PyLong_AsUnsignedLong),
    addr!(numbers::PyLong_AsUnsignedLongLong),
    addr!(numbers::PyLong_AsSsize_t),
    addr!(numbers::PyLong_AsDouble),
    addr!(numbers::PyLong_Check),
    addr!(numbers::PyFloat_FromDouble),
    addr!(numbers::PyFloat_AsDouble),
    addr!(numbers::PyFloat_Check),
    addr!(numbers::PyBool_FromLong),
    addr!(numbers::PyBool_Check),
    addr!(numbers::PyComplex_FromDoubles),
    addr!(numbers::PyComplex_RealAsDouble),
    addr!(numbers::PyComplex_ImagAsDouble),
    addr!(numbers::PyComplex_Check),
    // strings.rs
    addr!(strings::PyUnicode_FromString),
    addr!(strings::PyUnicode_FromStringAndSize),
    addr!(strings::PyUnicode_AsUTF8),
    addr!(strings::PyUnicode_AsUTF8AndSize),
    addr!(strings::PyUnicode_AsUTF8String),
    addr!(strings::PyUnicode_AsEncodedString),
    addr!(strings::PyUnicode_GetLength),
    addr!(strings::PyUnicode_Concat),
    addr!(strings::PyUnicode_Check),
    addr!(strings::PyUnicode_CompareWithASCIIString),
    addr!(strings::PyBytes_FromString),
    addr!(strings::PyBytes_FromStringAndSize),
    addr!(strings::PyBytes_AsString),
    addr!(strings::PyBytes_AsStringAndSize),
    addr!(strings::PyBytes_Size),
    addr!(strings::PyBytes_Check),
    addr!(strings::PyByteArray_FromStringAndSize),
    addr!(strings::PyByteArray_AsString),
    addr!(strings::PyByteArray_Size),
    addr!(strings::PyByteArray_Check),
    // containers.rs
    addr!(containers::PyList_New),
    addr!(containers::PyList_Size),
    addr!(containers::PyList_GetItem),
    addr!(containers::PyList_SetItem),
    addr!(containers::PyList_Append),
    addr!(containers::PyList_Insert),
    addr!(containers::PyList_Reverse),
    addr!(containers::PyList_Sort),
    addr!(containers::PyList_AsTuple),
    addr!(containers::PyList_Check),
    addr!(containers::PyTuple_New),
    addr!(containers::PyTuple_Size),
    addr!(containers::PyTuple_GetItem),
    addr!(containers::PyTuple_SetItem),
    addr!(containers::PyTuple_GetSlice),
    addr!(containers::PyTuple_Check),
    addr!(containers::PyDict_New),
    addr!(containers::PyDict_Size),
    addr!(containers::PyDict_SetItem),
    addr!(containers::PyDict_SetItemString),
    addr!(containers::PyDict_GetItem),
    addr!(containers::_PyDict_GetItem_KnownHash),
    addr!(containers::PyDict_GetItemString),
    addr!(containers::PyDict_DelItem),
    addr!(containers::PyDict_DelItemString),
    addr!(containers::PyDict_Contains),
    addr!(containers::PyDict_Clear),
    addr!(containers::PyDict_Copy),
    addr!(containers::PyDict_Keys),
    addr!(containers::PyDict_Values),
    addr!(containers::PyDict_Items),
    addr!(containers::PyDict_Merge),
    addr!(containers::PyDict_Update),
    addr!(containers::PyDict_Next),
    addr!(containers::PyDict_Check),
    addr!(containers::PySet_New),
    addr!(containers::PySet_Add),
    addr!(containers::PySet_Discard),
    addr!(containers::PySet_Contains),
    addr!(containers::PySet_Size),
    addr!(containers::PySet_Check),
    addr!(containers::PyFrozenSet_New),
    addr!(containers::PyFrozenSet_Check),
    // abstract_.rs
    addr!(ab::PyObject_GetAttr),
    addr!(ab::PyObject_GetAttrString),
    addr!(ab::PyObject_SetAttr),
    addr!(ab::PyObject_SetAttrString),
    addr!(ab::PyObject_DelAttrString),
    addr!(ab::PyObject_HasAttr),
    addr!(ab::PyObject_HasAttrWithError),
    addr!(ab::PyObject_HasAttrString),
    addr!(ab::PyObject_GetItem),
    addr!(ab::PyObject_SetItem),
    addr!(ab::PyObject_DelItem),
    addr!(ab::PyObject_Call),
    addr!(ab::PyObject_CallObject),
    addr!(ab::PyObject_CallNoArgs),
    addr!(ab::PyObject_CallOneArg),
    addr!(ab::PyObject_GetIter),
    addr!(ab::PyObject_Type),
    addr!(types::PyObject_TypeCheck),
    addr!(ab::PyObject_IsInstance),
    addr!(ab::PyObject_IsSubclass),
    addr!(ab::PyObject_IsTrue),
    addr!(ab::PyObject_Not),
    addr!(ab::PyObject_Hash),
    addr!(ab::PyObject_Length),
    addr!(ab::PyObject_Size),
    addr!(ab::PyObject_Str),
    addr!(ab::PyObject_Repr),
    addr!(ab::PyObject_ASCII),
    addr!(ab::PyObject_Dir),
    addr!(ab::PyObject_RichCompare),
    addr!(ab::PyObject_RichCompareBool),
    addr!(buffer::PyObject_CheckBuffer),
    addr!(buffer::PyObject_GetBuffer),
    addr!(ab::PyIter_Next),
    addr!(ab::PyIter_NextItem),
    addr!(ab::PyNumber_Check),
    addr!(ab::PyNumber_Add),
    addr!(ab::PyNumber_Subtract),
    addr!(ab::PyNumber_Multiply),
    addr!(ab::PyNumber_TrueDivide),
    addr!(ab::PyNumber_FloorDivide),
    addr!(ab::PyNumber_Remainder),
    addr!(ab::PyNumber_Power),
    addr!(ab::PyNumber_Negative),
    addr!(ab::PyNumber_Positive),
    addr!(ab::PyNumber_Absolute),
    addr!(ab::PyNumber_Long),
    addr!(ab::PyNumber_Float),
    addr!(ab::PySequence_Check),
    addr!(ab::PySequence_Length),
    addr!(ab::PySequence_Size),
    addr!(ab::PySequence_GetItem),
    addr!(ab::PySequence_SetItem),
    addr!(ab::PySequence_Contains),
    addr!(ab::PySequence_List),
    addr!(ab::PySequence_Tuple),
    addr!(ab::PyMapping_Check),
    addr!(ab::PyMapping_Length),
    addr!(ab::PyMapping_Size),
    addr!(ab::PyMapping_HasKey),
    addr!(ab::PyMapping_HasKeyString),
    addr!(ab::PyMapping_GetItemString),
    addr!(ab::PyMapping_SetItemString),
    // errors.rs
    addr!(errors::PyErr_Occurred),
    addr!(errors::PyErr_Clear),
    addr!(errors::PyErr_Print),
    addr!(errors::PyErr_PrintEx),
    addr!(errors::PyErr_Fetch),
    addr!(errors::PyErr_Restore),
    addr!(errors::PyErr_NormalizeException),
    addr!(errors::PyErr_SetNone),
    addr!(errors::PyErr_SetString),
    addr!(errors::PyErr_SetObject),
    addr!(errors::PyErr_NewException),
    addr!(errors::PyErr_NewExceptionWithDoc),
    addr!(errors::PyErr_ExceptionMatches),
    addr!(errors::PyErr_GivenExceptionMatches),
    addr!(errors::PyErr_BadArgument),
    addr!(errors::PyErr_BadInternalCall),
    addr!(errors::PyErr_NoMemory),
    addr!(errors::PyErr_WarnEx),
    // module.rs
    addr!(module::PyModule_Create2),
    addr!(module::PyModuleDef_Init),
    addr!(module::PyModule_GetDict),
    addr!(module::PyModule_GetName),
    addr!(module::PyModule_AddObject),
    addr!(module::PyModule_AddObjectRef),
    addr!(module::PyModule_AddIntConstant),
    addr!(module::PyModule_AddStringConstant),
    addr!(module::PyModule_AddType),
    addr!(module::PyModule_AddFunctions),
    addr!(module::PyModule_Check),
    addr!(module::PyImport_AddModule),
    addr!(module::PyImport_GetModule),
    addr!(module::PyImport_ImportModule),
    // memory.rs
    addr!(memory::PyMem_Malloc),
    addr!(memory::PyMem_Free),
    addr!(memory::PyMem_Calloc),
    addr!(memory::PyMem_Realloc),
    addr!(memory::PyMem_RawMalloc),
    addr!(memory::PyMem_RawFree),
    addr!(memory::PyMem_RawCalloc),
    addr!(memory::PyMem_RawRealloc),
    addr!(memory::PyObject_Malloc),
    addr!(memory::PyObject_Free),
    addr!(memory::PyObject_Calloc),
    addr!(memory::PyObject_Realloc),
    // types.rs
    addr!(types::PyType_FromSpec),
    addr!(types::PyType_FromSpecWithBases),
    addr!(types::PyType_FromModuleAndSpec),
    addr!(types::PyType_FromMetaclass),
    addr!(types::PyType_Ready),
    addr!(types::PyType_GetName),
    addr!(types::PyType_GetQualName),
    addr!(types::PyType_GetFlags),
    addr!(types::PyType_GetSlot),
    addr!(types::PyType_HasFeature),
    addr!(types::PyType_IsSubtype),
    // capsule.rs
    addr!(capsule::PyCapsule_New),
    addr!(capsule::PyCapsule_GetPointer),
    addr!(capsule::PyCapsule_GetName),
    addr!(capsule::PyCapsule_SetPointer),
    addr!(capsule::PyCapsule_SetName),
    addr!(capsule::PyCapsule_GetDestructor),
    addr!(capsule::PyCapsule_SetDestructor),
    addr!(capsule::PyCapsule_GetContext),
    addr!(capsule::PyCapsule_SetContext),
    addr!(capsule::PyCapsule_Import),
    addr!(capsule::PyCapsule_IsValid),
    // datetime_api.rs
    addr!(dt::PyDate_FromDate),
    addr!(dt::PyDateTime_FromDateAndTime),
    addr!(dt::PyTime_FromTime),
    addr!(dt::PyDelta_FromDSU),
    addr!(dt::PyTimeZone_FromOffset),
    addr!(dt::PyTimeZone_FromOffsetAndName),
    addr!(dt::PyDateTime_GET_YEAR),
    addr!(dt::PyDateTime_GET_MONTH),
    addr!(dt::PyDateTime_GET_DAY),
    addr!(dt::PyDateTime_DATE_GET_HOUR),
    addr!(dt::PyDateTime_DATE_GET_MINUTE),
    addr!(dt::PyDateTime_DATE_GET_SECOND),
    addr!(dt::PyDateTime_DATE_GET_MICROSECOND),
    addr!(dt::PyDateTime_TIME_GET_HOUR),
    addr!(dt::PyDateTime_TIME_GET_MINUTE),
    addr!(dt::PyDateTime_TIME_GET_SECOND),
    addr!(dt::PyDateTime_TIME_GET_MICROSECOND),
    addr!(dt::PyDateTime_DELTA_GET_DAYS),
    addr!(dt::PyDateTime_DELTA_GET_SECONDS),
    addr!(dt::PyDateTime_DELTA_GET_MICROSECONDS),
    addr!(dt::PyDate_Check),
    addr!(dt::PyDate_CheckExact),
    addr!(dt::PyDateTime_Check),
    addr!(dt::PyDateTime_CheckExact),
    addr!(dt::PyTime_Check),
    addr!(dt::PyTime_CheckExact),
    addr!(dt::PyDelta_Check),
    addr!(dt::PyDelta_CheckExact),
    addr!(dt::PyTZInfo_Check),
    addr!(dt::PyTZInfo_CheckExact),
    // buffer.rs
    addr!(buffer::PyBuffer_Release),
    addr!(buffer::PyBuffer_FillInfo),
    addr!(buffer::PyBuffer_IsContiguous),
    addr!(buffer::PyBuffer_ToContiguous),
    addr!(buffer::PyBuffer_FromContiguous),
    addr!(buffer::PyBuffer_GetPointer),
    addr!(buffer::PyBuffer_FillContiguousStrides),
    addr!(buffer::PyBuffer_SizeFromFormat),
    addr!(buffer::PyBuffer_HasFlag),
    // memoryview.rs
    addr!(memoryview::PyMemoryView_Check),
    addr!(memoryview::PyMemoryView_FromObject),
    addr!(memoryview::PyMemoryView_FromMemory),
    addr!(memoryview::PyMemoryView_FromBuffer),
    addr!(memoryview::PyMemoryView_GetContiguous),
    addr!(memoryview::PyMemoryView_GET_BUFFER),
    addr!(memoryview::PyMemoryView_GET_BASE),
    // vectorcall.rs
    addr!(vectorcall::PyVectorcall_NARGS),
    addr!(vectorcall::PyVectorcall_Function),
    addr!(vectorcall::PyVectorcall_Call),
    addr!(vectorcall::PyObject_Vectorcall),
    addr!(vectorcall::PyObject_VectorcallDict),
    addr!(vectorcall::PyObject_VectorcallMethod),
    addr!(vectorcall::PyObject_CallOneArg2),
    // genericalloc.rs
    addr!(genericalloc::PyType_GenericAlloc),
    addr!(genericalloc::PyType_GenericNew),
    addr!(genericalloc::_PyObject_New),
    addr!(genericalloc::_PyObject_NewVar),
    addr!(genericalloc::PyObject_Init),
    addr!(genericalloc::PyObject_InitVar),
    addr!(genericalloc::PyObject_GenericGetAttr),
    addr!(genericalloc::PyObject_GenericSetAttr),
    addr!(genericalloc::PyObject_GenericGetDict),
    addr!(genericalloc::PyObject_GenericSetDict),
    addr!(genericalloc::PyObject_HashNotImplemented),
    addr!(genericalloc::_Py_HashPointer),
    addr!(genericalloc::Py_HashPointer),
    addr!(genericalloc::_Py_HashBytes),
    addr!(genericalloc::Py_GenericAlias),
    // gc_bridge.rs — GC allocation + tracking C-API (RFC 0044).
    addr!(gc_bridge::_PyObject_GC_New),
    addr!(gc_bridge::_PyObject_GC_NewVar),
    addr!(gc_bridge::PyObject_GC_Track),
    addr!(gc_bridge::PyObject_GC_UnTrack),
    addr!(gc_bridge::PyObject_GC_IsTracked),
    addr!(gc_bridge::PyObject_GC_Del),
    // slice.rs
    addr!(slice::PySlice_New),
    addr!(slice::PySlice_Check),
    addr!(slice::PySlice_Unpack),
    addr!(slice::PySlice_AdjustIndices),
    addr!(slice::PySlice_GetIndicesEx),
    addr!(slice::PySlice_GetIndices),
    // lifecycle.rs
    addr!(lifecycle::Py_Initialize),
    addr!(lifecycle::Py_InitializeEx),
    addr!(lifecycle::Py_Finalize),
    addr!(lifecycle::Py_FinalizeEx),
    addr!(lifecycle::Py_IsInitialized),
    addr!(memory::Py_AtExit),
    addr!(lifecycle::Py_GetVersion),
    addr!(lifecycle::Py_GetPlatform),
    addr!(lifecycle::Py_GetCompiler),
    addr!(lifecycle::Py_GetBuildInfo),
    addr!(lifecycle::Py_GetCopyright),
    addr!(lifecycle::PyEval_SaveThread),
    addr!(lifecycle::PyEval_RestoreThread),
    addr!(lifecycle::PyEval_AcquireThread),
    addr!(lifecycle::PyEval_ReleaseThread),
    addr!(lifecycle::PyGILState_Ensure),
    addr!(lifecycle::PyGILState_Release),
    addr!(lifecycle::PyGILState_Check),
    addr!(lifecycle::PyThreadState_Get),
    addr!(lifecycle::PyThreadState_Swap),
    // argparse.rs (the C shim calls these)
    addr!(argparse::_WeavePy_Arg_Length),
    addr!(argparse::_WeavePy_Arg_Item),
    addr!(argparse::_WeavePy_Arg_Long),
    addr!(argparse::_WeavePy_Arg_Int),
    addr!(argparse::_WeavePy_Arg_Bool),
    addr!(argparse::_WeavePy_Arg_Double),
    addr!(argparse::_WeavePy_Arg_String),
    addr!(argparse::_WeavePy_Arg_StringAndSize),
    addr!(argparse::_WeavePy_Arg_Buffer),
    addr!(argparse::_WeavePy_Arg_Object),
    addr!(argparse::_WeavePy_Build_None),
    addr!(argparse::_WeavePy_Build_FromI64),
    addr!(argparse::_WeavePy_Build_FromU64),
    addr!(argparse::_WeavePy_Build_FromDouble),
    addr!(argparse::_WeavePy_Build_FromString),
    addr!(argparse::_WeavePy_Build_FromStringAndSize),
    addr!(argparse::_WeavePy_Build_FromBytesAndSize),
    addr!(argparse::_WeavePy_Build_TupleFromArray),
    addr!(argparse::_WeavePy_Build_ListFromArray),
    addr!(argparse::_WeavePy_Build_DictFromArrays),
    addr!(argparse::_WeavePy_Format_Set),
    addr!(slice::_WeavePy_LastResort),
    addr!(containers::_WeavePy_TuplePackFromArray),
    // C-side variadic helpers from varargs.c. These live in a
    // static archive that would otherwise get stripped, so we
    // reference each entry through its `extern "C"` declaration.
    addr!(PyArg_ParseTuple),
    addr!(PyArg_Parse),
    addr!(PyArg_VaParse),
    addr!(PyArg_ParseTupleAndKeywords),
    addr!(PyArg_VaParseTupleAndKeywords),
    addr!(PyArg_UnpackTuple),
    addr!(Py_BuildValue),
    addr!(Py_VaBuildValue),
    addr!(PyTuple_Pack),
    addr!(PyUnicode_FromFormat),
    addr!(PyUnicode_FromFormatV),
    addr!(PyErr_Format),
    addr!(PyErr_FormatV),
    addr!(PyObject_CallFunction),
    addr!(PyObject_CallMethod),
    addr!(PyObject_CallMethodObjArgs),
    addr!(PyObject_CallFunctionObjArgs),
    // Static globals. Functions get picked up implicitly by the
    // module-level references above, but `#[no_mangle] pub static`
    // items have no automatic referrer in the host binary and would
    // otherwise be stripped (and therefore unresolvable from dlopen'd
    // extensions). Reference the address of each one so the linker
    // emits them into the dynamic symbol table.
    //
    // singletons.rs
    addr_static!(singletons::_Py_NoneStruct),
    addr_static!(singletons::_Py_TrueStruct),
    addr_static!(singletons::_Py_FalseStruct),
    addr_static!(singletons::_Py_NotImplementedStruct),
    addr_static!(singletons::_Py_EllipsisObject),
    // types.rs — the static built-in type objects. A stock extension
    // compares `Py_TYPE(o) == &PyFloat_Type` etc., so these data
    // symbols must be in the host's dynamic symbol table (RFC 0043).
    addr_static!(types::PyType_Type),
    addr_static!(types::PyBaseObject_Type),
    addr_static!(types::PyLong_Type),
    addr_static!(types::PyFloat_Type),
    addr_static!(types::PyBool_Type),
    addr_static!(types::PyComplex_Type),
    addr_static!(types::PyUnicode_Type),
    addr_static!(types::PyBytes_Type),
    addr_static!(types::PyByteArray_Type),
    addr_static!(types::PyTuple_Type),
    addr_static!(types::PyList_Type),
    addr_static!(types::PyDict_Type),
    addr_static!(types::PySet_Type),
    addr_static!(types::PyFrozenSet_Type),
    addr_static!(types::PyRange_Type),
    addr_static!(types::PyModule_Type),
    addr_static!(types::PySlice_Type),
    addr_static!(types::PyCapsule_Type),
    addr_static!(types::PySeqIter_Type),
    // datetime_api.rs
    addr_static!(mut dt::PyDateTimeAPI),
    addr_static!(dt::PyDateTimeAPI_Instance),
    // errors.rs (PyExc_* exception type slots).
    addr_static!(mut errors::PyExc_BaseException),
    addr_static!(mut errors::PyExc_Exception),
    addr_static!(mut errors::PyExc_ArithmeticError),
    addr_static!(mut errors::PyExc_AssertionError),
    addr_static!(mut errors::PyExc_AttributeError),
    addr_static!(mut errors::PyExc_BufferError),
    addr_static!(mut errors::PyExc_EOFError),
    addr_static!(mut errors::PyExc_FloatingPointError),
    addr_static!(mut errors::PyExc_GeneratorExit),
    addr_static!(mut errors::PyExc_ImportError),
    addr_static!(mut errors::PyExc_IndentationError),
    addr_static!(mut errors::PyExc_IndexError),
    addr_static!(mut errors::PyExc_KeyError),
    addr_static!(mut errors::PyExc_KeyboardInterrupt),
    addr_static!(mut errors::PyExc_LookupError),
    addr_static!(mut errors::PyExc_MemoryError),
    addr_static!(mut errors::PyExc_ModuleNotFoundError),
    addr_static!(mut errors::PyExc_NameError),
    addr_static!(mut errors::PyExc_NotImplementedError),
    addr_static!(mut errors::PyExc_OSError),
    addr_static!(mut errors::PyExc_OverflowError),
    addr_static!(mut errors::PyExc_RecursionError),
    addr_static!(mut errors::PyExc_ReferenceError),
    addr_static!(mut errors::PyExc_RuntimeError),
    addr_static!(mut errors::PyExc_StopAsyncIteration),
    addr_static!(mut errors::PyExc_StopIteration),
    addr_static!(mut errors::PyExc_SyntaxError),
    addr_static!(mut errors::PyExc_SystemError),
    addr_static!(mut errors::PyExc_SystemExit),
    addr_static!(mut errors::PyExc_TabError),
    addr_static!(mut errors::PyExc_TimeoutError),
    addr_static!(mut errors::PyExc_TypeError),
    addr_static!(mut errors::PyExc_UnboundLocalError),
    addr_static!(mut errors::PyExc_UnicodeDecodeError),
    addr_static!(mut errors::PyExc_UnicodeEncodeError),
    addr_static!(mut errors::PyExc_UnicodeError),
    addr_static!(mut errors::PyExc_UnicodeTranslateError),
    addr_static!(mut errors::PyExc_ValueError),
    addr_static!(mut errors::PyExc_ZeroDivisionError),
    addr_static!(mut errors::PyExc_BlockingIOError),
    addr_static!(mut errors::PyExc_BrokenPipeError),
    addr_static!(mut errors::PyExc_ChildProcessError),
    addr_static!(mut errors::PyExc_ConnectionAbortedError),
    addr_static!(mut errors::PyExc_ConnectionError),
    addr_static!(mut errors::PyExc_ConnectionRefusedError),
    addr_static!(mut errors::PyExc_ConnectionResetError),
    addr_static!(mut errors::PyExc_FileExistsError),
    addr_static!(mut errors::PyExc_FileNotFoundError),
    addr_static!(mut errors::PyExc_InterruptedError),
    addr_static!(mut errors::PyExc_IsADirectoryError),
    addr_static!(mut errors::PyExc_NotADirectoryError),
    addr_static!(mut errors::PyExc_PermissionError),
    addr_static!(mut errors::PyExc_ProcessLookupError),
    addr_static!(mut errors::PyExc_Warning),
    addr_static!(mut errors::PyExc_UserWarning),
    addr_static!(mut errors::PyExc_DeprecationWarning),
    addr_static!(mut errors::PyExc_PendingDeprecationWarning),
    addr_static!(mut errors::PyExc_SyntaxWarning),
    addr_static!(mut errors::PyExc_RuntimeWarning),
    addr_static!(mut errors::PyExc_FutureWarning),
    addr_static!(mut errors::PyExc_ImportWarning),
    addr_static!(mut errors::PyExc_UnicodeWarning),
    addr_static!(mut errors::PyExc_BytesWarning),
    addr_static!(mut errors::PyExc_ResourceWarning),
    // ----------------------------------------------------------------
    // RFC 0046 (wave 4): the CPython 3.13 C-API tail stock numpy's
    // `_multiarray_umath` links. New leaf implementations live in
    // `wave4.rs`; the rest were already implemented in waves 1-3 but had
    // never been pinned into the dynamic symbol table.
    // ----------------------------------------------------------------
    // wave4.rs — predicates / iteration
    addr!(wave4::PyCallable_Check),
    addr!(wave4::PyIndex_Check),
    addr!(wave4::PyIter_Check),
    addr!(wave4::PyObject_SelfIter),
    addr!(wave4::PySeqIter_New),
    // wave4.rs — sound no-ops
    addr!(wave4::PyErr_CheckSignals),
    addr!(wave4::PyTraceMalloc_Track),
    addr!(wave4::PyTraceMalloc_Untrack),
    addr!(wave4::PyType_Modified),
    addr!(wave4::PyMutex_Lock),
    addr!(wave4::PyMutex_Unlock),
    addr!(wave4::PyObject_ClearWeakRefs),
    addr!(wave4::PyErr_WriteUnraisable),
    // wave4.rs — exception chaining
    addr!(wave4::PyException_SetCause),
    addr!(wave4::PyException_SetContext),
    addr!(wave4::PyException_SetTraceback),
    // wave4.rs — dict tail
    addr!(wave4::PyDict_GetItemWithError),
    addr!(wave4::PyDict_GetItemRef),
    addr!(wave4::PyDict_GetItemStringRef),
    addr!(wave4::PyDict_ContainsString),
    addr!(wave4::PyDict_SetDefaultRef),
    addr!(wave4::PyDictProxy_New),
    // wave4.rs — numbers
    addr!(wave4::PyComplex_AsCComplex),
    addr!(wave4::PyComplex_FromCComplex),
    addr!(wave4::_PyLong_Sign),
    addr!(wave4::_Py_HashDouble),
    addr!(wave4::PyLong_FromUnicodeObject),
    addr!(wave4::PyFloat_FromString),
    // wave4.rs — unicode tail
    addr!(wave4::PyUnicode_AsUCS4),
    addr!(wave4::PyUnicode_AsUCS4Copy),
    addr!(wave4::PyUnicode_Format),
    addr!(wave4::_PyUnicode_IsAlpha),
    addr!(wave4::_PyUnicode_IsDecimalDigit),
    addr!(wave4::_PyUnicode_IsDigit),
    addr!(wave4::_PyUnicode_IsNumeric),
    addr!(wave4::_PyUnicode_IsLowercase),
    addr!(wave4::_PyUnicode_IsUppercase),
    addr!(wave4::_PyUnicode_IsTitlecase),
    addr!(wave4::_PyUnicode_IsWhitespace),
    addr_static!(wave4::_Py_ascii_whitespace),
    // wave4.rs — OS string parsing
    addr!(wave4::PyOS_string_to_double),
    addr!(wave4::PyOS_strtol),
    addr!(wave4::PyOS_strtoul),
    // wave4.rs — object tail
    addr!(wave4::PyObject_AsFileDescriptor),
    addr!(wave4::PyObject_GetOptionalAttr),
    addr!(wave4::PyObject_Print),
    addr!(wave4::PyMethod_New),
    // wave4.rs — import / sys / eval
    addr!(wave4::PyImport_Import),
    addr!(wave4::PySys_GetObject),
    addr!(wave4::PyEval_GetBuiltins),
    addr!(wave4::PyInterpreterState_Main),
    // wave4.rs — errors
    addr!(wave4::_PyErr_BadInternalCall),
    addr!(wave4::PyErr_SetFromErrno),
    // wave4.rs — contextvars
    addr!(wave4::PyContextVar_New),
    addr!(wave4::PyContextVar_Get),
    addr!(wave4::PyContextVar_Set),
    // varargs.c — wave-4 variadic shims
    addr!(PyOS_snprintf),
    addr!(PyErr_WarnFormat),
    // Already implemented in waves 1-3, now pinned for numpy.
    addr!(numbers::PyLong_AsLongAndOverflow),
    addr!(numbers::PyLong_AsLongLongAndOverflow),
    addr!(numbers::PyLong_AsVoidPtr),
    addr!(numbers::PyLong_FromVoidPtr),
    addr!(ab::PyNumber_And),
    addr!(ab::PyNumber_AsSsize_t),
    addr!(ab::PyNumber_Divmod),
    addr!(ab::PyNumber_Index),
    addr!(ab::PyNumber_Invert),
    addr!(ab::PyNumber_Lshift),
    addr!(ab::PyNumber_Or),
    addr!(ab::PyNumber_Rshift),
    addr!(ab::PyNumber_Xor),
    addr!(ab::PyObject_Bytes),
    addr!(ab::PyObject_Format),
    addr!(ab::PyObject_LengthHint),
    addr!(ab::PySequence_Concat),
    addr!(ab::PySequence_InPlaceConcat),
    addr!(ab::PySequence_InPlaceRepeat),
    addr!(ab::PySequence_Repeat),
    addr!(containers::PySequence_Fast),
    addr!(strings::PyUnicode_AsASCIIString),
    addr!(strings::PyUnicode_AsLatin1String),
    addr!(strings::PyUnicode_Compare),
    addr!(strings::PyUnicode_Contains),
    addr!(strings::PyUnicode_FromEncodedObject),
    addr!(strings::PyUnicode_FromKindAndData),
    addr!(strings::PyUnicode_Replace),
    addr!(strings::PyUnicode_Substring),
    addr!(strings::PyUnicode_FindChar),
    addr!(strings::PyUnicode_Tailmatch),
    addr!(ab::Py_EnterRecursiveCall),
    addr!(ab::Py_LeaveRecursiveCall),
    addr!(strings::PyUnicode_InternFromString),
    // wave-4 type-object statics (numpy compares `Py_TYPE(x) == &…`).
    addr_static!(types::PyMemoryView_Type),
    addr_static!(types::PyDictProxy_Type),
    addr_static!(types::PyGetSetDescr_Type),
    addr_static!(types::PyMemberDescr_Type),
    addr_static!(types::PyMethodDescr_Type),
    addr_static!(types::PyWrapperDescr_Type),
    addr_static!(types::PyModuleDef_Type),
    // IOError alias of OSError.
    addr_static!(mut errors::PyExc_IOError),
    // ----------------------------------------------------------------
    // RFC 0047 (wave 5): the CPython 3.13 C-API leaf tail that
    // Cython-generated extensions (and pandas) link. New leaf
    // implementations live in `wave5.rs`; each delegates onto the
    // wave-1/2/3 surface.
    // ----------------------------------------------------------------
    addr!(wave5::_PyObject_GetDictPtr),
    addr!(wave5::PyObject_GetOptionalAttrString),
    addr!(wave5::PyMapping_GetOptionalItem),
    addr!(wave5::PyMapping_GetOptionalItemString),
    addr!(wave5::_PyObject_GetMethod),
    addr!(wave5::PyObject_CallMethodOneArg),
    addr!(wave5::_PyDict_NewPresized),
    addr!(wave5::PyLong_AsInt),
    addr!(wave5::PyImport_ImportModuleLevelObject),
    // ----------------------------------------------------------------
    // RFC 0047 (wave 5): the *real* Cython-output tail. A genuine
    // `cythonize`d `.so` (and pandas, ~70% Cython) links a faithful
    // code/frame/traceback surface, a real `PyThreadState` whose
    // `current_exception` slot it drives directly
    // (`CYTHON_FAST_THREAD_STATE`), the MRO lookup, the module/import
    // ref helpers, and a cluster of GC/managed-dict no-ops. These were
    // invisible to the hermetic `_stockcython.c` fixture (which created
    // no code objects and hand-rolled its own types).
    // ----------------------------------------------------------------
    // code_obj.rs — code / frame / traceback facade
    addr!(code_obj::PyUnstable_Code_NewWithPosOnlyArgs),
    addr!(code_obj::PyUnstable_Code_New),
    addr!(code_obj::PyCode_NewEmpty),
    addr!(code_obj::PyFrame_New),
    addr!(code_obj::PyTraceBack_Here),
    addr_static!(code_obj::PyCode_Type),
    addr_static!(code_obj::PyFrame_Type),
    addr_static!(code_obj::PyTraceBack_Type),
    // pystate.rs — faithful thread/interpreter state
    addr!(pystate::PyThreadState_GetUnchecked),
    addr!(pystate::PyInterpreterState_GetID),
    addr!(pystate::PyGC_Enable),
    addr!(pystate::PyGC_Disable),
    // errors.rs — 3.12+ single-object exception API
    addr!(errors::PyErr_GetRaisedException),
    addr!(errors::PyErr_SetRaisedException),
    // module.rs — import / module ref helpers
    addr!(module::PyImport_AddModuleRef),
    addr!(module::PyImport_GetModuleDict),
    addr!(module::PyModule_NewObject),
    addr!(module::PyClassMethod_New),
    addr!(module::PyDescr_NewClassMethod),
    // wave5.rs — MRO lookup, kwarg validation, GC/managed-dict no-ops
    addr!(wave5::_PyType_Lookup),
    addr!(wave5::PyArg_ValidateKeywordArguments),
    addr!(wave5::PyObject_VisitManagedDict),
    addr!(wave5::PyObject_ClearManagedDict),
    addr!(wave5::PyObject_GC_IsFinalized),
    addr!(wave5::PyObject_CallFinalizerFromDealloc),
    addr_static!(wave5::Py_Version),
    // ----------------------------------------------------------------
    // RFC 0047 (wave 5): the real numpy.random + pandas leaf tail
    // (`crate::wave5_pandas`), plus existing entry points that only an
    // extension references and that the linker therefore dead-stripped.
    // ----------------------------------------------------------------
    addr!(w5p::PyThread_allocate_lock),
    addr!(w5p::PyThread_free_lock),
    addr!(w5p::PyThread_acquire_lock),
    addr!(w5p::PyThread_acquire_lock_timed),
    addr!(w5p::PyThread_release_lock),
    addr!(w5p::PyThreadState_GetFrame),
    addr!(w5p::PyModule_GetState),
    addr!(w5p::PyState_FindModule),
    addr!(w5p::PyList_SetSlice),
    addr!(w5p::PyException_GetTraceback),
    addr!(w5p::PyStaticMethod_New),
    addr!(w5p::_PyLong_Copy),
    addr!(w5p::PyUnicode_FromWideChar),
    addr!(w5p::PyUnicode_DecodeLocale),
    addr!(w5p::PyUnicode_EncodeLocale),
    addr!(w5p::PyUnicode_Resize),
    addr!(w5p::_Py_FatalErrorFunc),
    addr!(w5p::PyCMethod_New),
    addr_static!(w5p::_PyByteArray_empty_string),
    addr!(module::PyCFunction_NewEx),
    // Existing definitions referenced only by a dlopen'd extension.
    addr!(strings::PyUnicode_FromOrdinal),
    addr!(strings::PyUnicode_Decode),
    addr!(strings::PyUnicode_New),
    addr!(strings::PyUnicode_Split),
    addr!(strings::PyUnicode_Join),
    addr!(strings::PyUnicode_CopyCharacters),
    addr!(strings::PyUnicode_WriteChar),
    addr!(strings::PyUnicode_ReadChar),
    addr!(ab::PyNumber_InPlaceRshift),
    addr!(ab::PyNumber_InPlaceAnd),
    addr!(containers::PySet_Pop),
    // monitoring.rs — PEP 669 sys.monitoring C-API (no-op surface)
    addr!(monitoring::PyMonitoring_EnterScope),
    addr!(monitoring::PyMonitoring_ExitScope),
    addr!(monitoring::_PyMonitoring_FirePyStartEvent),
    addr!(monitoring::_PyMonitoring_FirePyResumeEvent),
    addr!(monitoring::_PyMonitoring_FirePyReturnEvent),
    addr!(monitoring::_PyMonitoring_FirePyYieldEvent),
    addr!(monitoring::_PyMonitoring_FireCallEvent),
    addr!(monitoring::_PyMonitoring_FireLineEvent),
    addr!(monitoring::_PyMonitoring_FireJumpEvent),
    addr!(monitoring::_PyMonitoring_FireBranchEvent),
    addr!(monitoring::_PyMonitoring_FireCReturnEvent),
    addr!(monitoring::_PyMonitoring_FirePyThrowEvent),
    addr!(monitoring::_PyMonitoring_FireRaiseEvent),
    addr!(monitoring::_PyMonitoring_FireReraiseEvent),
    addr!(monitoring::_PyMonitoring_FireExceptionHandledEvent),
    addr!(monitoring::_PyMonitoring_FireCRaiseEvent),
    addr!(monitoring::_PyMonitoring_FirePyUnwindEvent),
    addr!(monitoring::_PyMonitoring_FireStopIterationEvent),
    // Already implemented in earlier waves, now pinned for real Cython.
    addr!(containers::PyDict_Pop),
    addr!(containers::PyDict_SetDefault),
    addr!(strings::PyUnicode_DecodeUTF8),
    addr!(strings::PyUnicode_InternInPlace),
    addr_static!(types::PyCFunction_Type),
    addr_static!(types::PyMethod_Type),
    // ---------------------------------------------------------------
    // Completeness sweep (RFC 0047, wave 5): every `#[no_mangle]`
    // `extern "C"` entry point this crate *defines* must be rooted
    // here, or the linker dead-strips it and a dlopen'd extension that
    // calls it jumps through an unbound stub to a NULL address and
    // segfaults — exactly how numpy's `SeedSequence` (`n //= 2**32`,
    // i.e. `PyNumber_InPlaceFloorDivide`) crashed. The
    // `force_link_completeness` test (tests/force_link_completeness.rs)
    // fails the build if any defined symbol is ever missing again, so
    // this list is no longer a best-effort guess.
    //
    // abstract_ — number/sequence/mapping/object protocol.
    addr!(ab::PyNumber_InPlaceAdd),
    addr!(ab::PyNumber_InPlaceSubtract),
    addr!(ab::PyNumber_InPlaceMultiply),
    addr!(ab::PyNumber_InPlaceTrueDivide),
    addr!(ab::PyNumber_InPlaceFloorDivide),
    addr!(ab::PyNumber_InPlaceRemainder),
    addr!(ab::PyNumber_InPlacePower),
    addr!(ab::PyNumber_InPlaceMatrixMultiply),
    addr!(ab::PyNumber_InPlaceLshift),
    addr!(ab::PyNumber_InPlaceOr),
    addr!(ab::PyNumber_InPlaceXor),
    addr!(ab::PyNumber_ToBase),
    addr!(ab::PyObject_DelAttr),
    addr!(ab::PyObject_GetAttrId),
    addr!(ab::PySequence_Count),
    addr!(ab::PySequence_Index),
    addr!(ab::PySequence_GetSlice),
    addr!(ab::PySequence_SetSlice),
    addr!(ab::PySequence_DelSlice),
    addr!(ab::PyMapping_DelItem),
    addr!(ab::PyMapping_DelItemString),
    addr!(ab::PyMapping_Keys),
    addr!(ab::PyMapping_Values),
    addr!(ab::PyMapping_Items),
    addr!(ab::_PyObject_GenericGetAttrWithDict),
    addr!(ab::_PyObject_GenericSetAttrWithDict),
    addr!(ab::_PyObject_LookupAttr),
    addr!(ab::_PyObject_LookupAttrId),
    addr!(ab::_Py_CheckRecursionLimit),
    // containers — list/tuple/set fast accessors.
    addr!(containers::PyList_Extend),
    addr!(containers::_PyList_Extend),
    addr!(containers::_PyList_GET_ITEM),
    addr!(containers::_PyList_SET_ITEM),
    addr!(containers::_PyTuple_GET_ITEM),
    addr!(containers::_PyTuple_SET_ITEM),
    addr!(containers::_PyTuple_Resize),
    addr!(containers::PySequence_Fast_GET_ITEM),
    addr!(containers::PySequence_Fast_GET_SIZE),
    addr!(containers::PySequence_Fast_ITEMS),
    addr!(containers::PySet_Clear),
    // numbers — float/long introspection + IEEE pack/unpack.
    addr!(numbers::PyFloat_GetInfo),
    addr!(numbers::PyFloat_GetMax),
    addr!(numbers::PyFloat_GetMin),
    addr!(numbers::PyLong_GetInfo),
    addr!(numbers::_PyFloat_Pack4),
    addr!(numbers::_PyFloat_Pack8),
    addr!(numbers::_PyFloat_Unpack4),
    addr!(numbers::_PyFloat_Unpack8),
    addr!(numbers::_PyLong_AsByteArray),
    addr!(numbers::_PyLong_FromByteArray),
    // strings — bytes/bytearray/unicode codecs + helpers.
    addr!(strings::PyBytes_Concat),
    addr!(strings::PyBytes_ConcatAndDel),
    addr!(strings::PyBytes_FromFormat),
    addr!(strings::PyByteArray_Concat),
    addr!(strings::PyByteArray_Resize),
    addr!(strings::PyUnicode_DecodeASCII),
    addr!(strings::PyUnicode_DecodeLatin1),
    addr!(strings::PyUnicode_DecodeFSDefault),
    addr!(strings::PyUnicode_DecodeFSDefaultAndSize),
    addr!(strings::PyUnicode_EncodeFSDefault),
    addr!(strings::PyUnicode_EqualToUTF8),
    addr!(strings::PyUnicode_EqualToUTF8AndSize),
    addr!(strings::PyUnicode_Fill),
    addr!(strings::PyUnicode_IsIdentifier),
    addr!(strings::PyUnicode_RichCompare),
    addr!(strings::PyUnicode_Splitlines),
];

/// Hand-out of the table to ensure the static is referenced from
/// non-`#[used]`-aware optimisers (e.g. release LTO).
pub fn touch() -> usize {
    FORCE_LINK.len()
}
