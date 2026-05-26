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
use crate::containers;
use crate::datetime_api as dt;
use crate::errors;
use crate::genericalloc;
use crate::lifecycle;
use crate::memory;
use crate::memoryview;
use crate::module;
use crate::numbers;
use crate::object;
use crate::slice;
use crate::strings;
use crate::types;
use crate::vectorcall;

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
}

macro_rules! addr {
    ($f:expr) => {
        FnPtr($f as *const c_void)
    };
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
];

/// Hand-out of the table to ensure the static is referenced from
/// non-`#[used]`-aware optimisers (e.g. release LTO).
pub fn touch() -> usize {
    FORCE_LINK.len()
}
