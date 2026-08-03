//! Raw FFI bindings to the vendored expat 2.6.4 (`expat-2.6.4/lib/expat.h`),
//! restricted to the surface CPython's `pyexpat` uses. The build is a
//! UTF-8 (`XML_Char == char`) build with `XML_NS`, `XML_DTD`, `XML_GE`
//! and `XML_CONTEXT_BYTES` enabled.

#![allow(non_camel_case_types, non_snake_case, clippy::missing_safety_doc)]

use std::os::raw::{c_char, c_int, c_long, c_ulong, c_void};

/// Opaque parser handle (`struct XML_ParserStruct *`).
pub type XML_Parser = *mut c_void;
/// UTF-8 build: `XML_Char` is `char`.
pub type XML_Char = c_char;
pub type XML_LChar = c_char;
/// `XML_Size` / `XML_Index` (no `XML_LARGE_SIZE`).
pub type XML_Size = c_ulong;
pub type XML_Index = c_long;
pub type XML_Bool = std::os::raw::c_uchar;

pub const XML_TRUE: XML_Bool = 1;
pub const XML_FALSE: XML_Bool = 0;

// enum XML_Status
pub const XML_STATUS_ERROR: c_int = 0;
pub const XML_STATUS_OK: c_int = 1;

// enum XML_ParamEntityParsing
pub const XML_PARAM_ENTITY_PARSING_NEVER: c_int = 0;
pub const XML_PARAM_ENTITY_PARSING_UNLESS_STANDALONE: c_int = 1;
pub const XML_PARAM_ENTITY_PARSING_ALWAYS: c_int = 2;

// enum XML_Content_Type
pub const XML_CTYPE_EMPTY: c_int = 1;
pub const XML_CTYPE_ANY: c_int = 2;
pub const XML_CTYPE_MIXED: c_int = 3;
pub const XML_CTYPE_NAME: c_int = 4;
pub const XML_CTYPE_CHOICE: c_int = 5;
pub const XML_CTYPE_SEQ: c_int = 6;

// enum XML_Content_Quant
pub const XML_CQUANT_NONE: c_int = 0;
pub const XML_CQUANT_OPT: c_int = 1;
pub const XML_CQUANT_REP: c_int = 2;
pub const XML_CQUANT_PLUS: c_int = 3;

/// Element content model (`XML_Content`), delivered to the
/// ElementDeclHandler and released with `XML_FreeContentModel`.
#[repr(C)]
pub struct XML_Content {
    pub type_: c_int,
    pub quant: c_int,
    pub name: *const XML_Char,
    pub numchildren: c_int,
    pub children: *mut XML_Content,
}

// Handler signatures (UTF-8 build).
pub type XML_StartElementHandler =
    unsafe extern "C" fn(*mut c_void, *const XML_Char, *mut *const XML_Char);
pub type XML_EndElementHandler = unsafe extern "C" fn(*mut c_void, *const XML_Char);
pub type XML_CharacterDataHandler = unsafe extern "C" fn(*mut c_void, *const XML_Char, c_int);
pub type XML_ProcessingInstructionHandler =
    unsafe extern "C" fn(*mut c_void, *const XML_Char, *const XML_Char);
pub type XML_CommentHandler = unsafe extern "C" fn(*mut c_void, *const XML_Char);
pub type XML_StartCdataSectionHandler = unsafe extern "C" fn(*mut c_void);
pub type XML_EndCdataSectionHandler = unsafe extern "C" fn(*mut c_void);
pub type XML_DefaultHandler = unsafe extern "C" fn(*mut c_void, *const XML_Char, c_int);
pub type XML_StartDoctypeDeclHandler =
    unsafe extern "C" fn(*mut c_void, *const XML_Char, *const XML_Char, *const XML_Char, c_int);
pub type XML_EndDoctypeDeclHandler = unsafe extern "C" fn(*mut c_void);
pub type XML_EntityDeclHandler = unsafe extern "C" fn(
    *mut c_void,
    *const XML_Char,
    c_int,
    *const XML_Char,
    c_int,
    *const XML_Char,
    *const XML_Char,
    *const XML_Char,
    *const XML_Char,
);
pub type XML_XmlDeclHandler =
    unsafe extern "C" fn(*mut c_void, *const XML_Char, *const XML_Char, c_int);
pub type XML_ElementDeclHandler =
    unsafe extern "C" fn(*mut c_void, *const XML_Char, *mut XML_Content);
pub type XML_AttlistDeclHandler = unsafe extern "C" fn(
    *mut c_void,
    *const XML_Char,
    *const XML_Char,
    *const XML_Char,
    *const XML_Char,
    c_int,
);
pub type XML_UnparsedEntityDeclHandler = unsafe extern "C" fn(
    *mut c_void,
    *const XML_Char,
    *const XML_Char,
    *const XML_Char,
    *const XML_Char,
    *const XML_Char,
);
pub type XML_NotationDeclHandler = unsafe extern "C" fn(
    *mut c_void,
    *const XML_Char,
    *const XML_Char,
    *const XML_Char,
    *const XML_Char,
);
pub type XML_StartNamespaceDeclHandler =
    unsafe extern "C" fn(*mut c_void, *const XML_Char, *const XML_Char);
pub type XML_EndNamespaceDeclHandler = unsafe extern "C" fn(*mut c_void, *const XML_Char);
pub type XML_NotStandaloneHandler = unsafe extern "C" fn(*mut c_void) -> c_int;
pub type XML_ExternalEntityRefHandler = unsafe extern "C" fn(
    XML_Parser,
    *const XML_Char,
    *const XML_Char,
    *const XML_Char,
    *const XML_Char,
) -> c_int;
pub type XML_SkippedEntityHandler = unsafe extern "C" fn(*mut c_void, *const XML_Char, c_int);

/// Single-byte encoding map filled by the UnknownEncodingHandler.
#[repr(C)]
pub struct XML_Encoding {
    pub map: [c_int; 256],
    pub data: *mut c_void,
    pub convert: Option<unsafe extern "C" fn(*mut c_void, *const c_char) -> c_int>,
    pub release: Option<unsafe extern "C" fn(*mut c_void)>,
}

pub type XML_UnknownEncodingHandler =
    unsafe extern "C" fn(*mut c_void, *const XML_Char, *mut XML_Encoding) -> c_int;

/// `XML_Expat_Version` (returned by value from `XML_ExpatVersionInfo`).
#[repr(C)]
pub struct XML_Expat_Version {
    pub major: c_int,
    pub minor: c_int,
    pub micro: c_int,
}

/// One entry of the `XML_GetFeatureList()` array (terminated by an entry
/// with `feature == XML_FEATURE_END == 0`).
#[repr(C)]
pub struct XML_Feature {
    pub feature: c_int,
    pub name: *const XML_LChar,
    pub value: c_long,
}

extern "C" {
    pub fn XML_ParserCreate(encoding: *const XML_Char) -> XML_Parser;
    pub fn XML_SetEncoding(parser: XML_Parser, encoding: *const XML_Char) -> c_int;
    pub fn XML_ExpatVersionInfo() -> XML_Expat_Version;
    pub fn XML_GetFeatureList() -> *const XML_Feature;
    pub fn XML_ParserCreateNS(encoding: *const XML_Char, sep: XML_Char) -> XML_Parser;
    pub fn XML_ExternalEntityParserCreate(
        parser: XML_Parser,
        context: *const XML_Char,
        encoding: *const XML_Char,
    ) -> XML_Parser;
    pub fn XML_ParserFree(parser: XML_Parser);
    pub fn XML_Parse(parser: XML_Parser, s: *const c_char, len: c_int, isFinal: c_int) -> c_int;
    pub fn XML_StopParser(parser: XML_Parser, resumable: XML_Bool) -> c_int;

    pub fn XML_SetUserData(parser: XML_Parser, userData: *mut c_void);
    pub fn XML_SetReturnNSTriplet(parser: XML_Parser, do_nst: c_int);
    pub fn XML_SetParamEntityParsing(parser: XML_Parser, parsing: c_int) -> c_int;
    pub fn XML_SetHashSalt(parser: XML_Parser, hash_salt: c_ulong) -> c_int;
    pub fn XML_SetBase(parser: XML_Parser, base: *const XML_Char) -> c_int;
    pub fn XML_GetBase(parser: XML_Parser) -> *const XML_Char;
    pub fn XML_GetSpecifiedAttributeCount(parser: XML_Parser) -> c_int;
    pub fn XML_UseForeignDTD(parser: XML_Parser, useDTD: XML_Bool) -> c_int;
    pub fn XML_SetReparseDeferralEnabled(parser: XML_Parser, enabled: XML_Bool) -> XML_Bool;

    pub fn XML_GetErrorCode(parser: XML_Parser) -> c_int;
    pub fn XML_ErrorString(code: c_int) -> *const XML_LChar;
    pub fn XML_GetCurrentLineNumber(parser: XML_Parser) -> XML_Size;
    pub fn XML_GetCurrentColumnNumber(parser: XML_Parser) -> XML_Size;
    pub fn XML_GetCurrentByteIndex(parser: XML_Parser) -> XML_Index;
    pub fn XML_GetInputContext(
        parser: XML_Parser,
        offset: *mut c_int,
        size: *mut c_int,
    ) -> *const c_char;
    pub fn XML_ExpatVersion() -> *const XML_LChar;
    pub fn XML_FreeContentModel(parser: XML_Parser, model: *mut XML_Content);
    pub fn XML_MemFree(parser: XML_Parser, ptr: *mut c_void);

    pub fn XML_SetStartElementHandler(parser: XML_Parser, h: Option<XML_StartElementHandler>);
    pub fn XML_SetEndElementHandler(parser: XML_Parser, h: Option<XML_EndElementHandler>);
    pub fn XML_SetCharacterDataHandler(parser: XML_Parser, h: Option<XML_CharacterDataHandler>);
    pub fn XML_SetProcessingInstructionHandler(
        parser: XML_Parser,
        h: Option<XML_ProcessingInstructionHandler>,
    );
    pub fn XML_SetCommentHandler(parser: XML_Parser, h: Option<XML_CommentHandler>);
    pub fn XML_SetStartCdataSectionHandler(
        parser: XML_Parser,
        h: Option<XML_StartCdataSectionHandler>,
    );
    pub fn XML_SetEndCdataSectionHandler(parser: XML_Parser, h: Option<XML_EndCdataSectionHandler>);
    pub fn XML_SetDefaultHandler(parser: XML_Parser, h: Option<XML_DefaultHandler>);
    pub fn XML_SetDefaultHandlerExpand(parser: XML_Parser, h: Option<XML_DefaultHandler>);
    pub fn XML_SetStartDoctypeDeclHandler(
        parser: XML_Parser,
        h: Option<XML_StartDoctypeDeclHandler>,
    );
    pub fn XML_SetEndDoctypeDeclHandler(parser: XML_Parser, h: Option<XML_EndDoctypeDeclHandler>);
    pub fn XML_SetEntityDeclHandler(parser: XML_Parser, h: Option<XML_EntityDeclHandler>);
    pub fn XML_SetXmlDeclHandler(parser: XML_Parser, h: Option<XML_XmlDeclHandler>);
    pub fn XML_SetElementDeclHandler(parser: XML_Parser, h: Option<XML_ElementDeclHandler>);
    pub fn XML_SetAttlistDeclHandler(parser: XML_Parser, h: Option<XML_AttlistDeclHandler>);
    pub fn XML_SetUnparsedEntityDeclHandler(
        parser: XML_Parser,
        h: Option<XML_UnparsedEntityDeclHandler>,
    );
    pub fn XML_SetNotationDeclHandler(parser: XML_Parser, h: Option<XML_NotationDeclHandler>);
    pub fn XML_SetStartNamespaceDeclHandler(
        parser: XML_Parser,
        h: Option<XML_StartNamespaceDeclHandler>,
    );
    pub fn XML_SetEndNamespaceDeclHandler(
        parser: XML_Parser,
        h: Option<XML_EndNamespaceDeclHandler>,
    );
    pub fn XML_SetNotStandaloneHandler(parser: XML_Parser, h: Option<XML_NotStandaloneHandler>);
    pub fn XML_SetExternalEntityRefHandler(
        parser: XML_Parser,
        h: Option<XML_ExternalEntityRefHandler>,
    );
    pub fn XML_SetSkippedEntityHandler(parser: XML_Parser, h: Option<XML_SkippedEntityHandler>);
    pub fn XML_SetUnknownEncodingHandler(
        parser: XML_Parser,
        h: Option<XML_UnknownEncodingHandler>,
        data: *mut c_void,
    );
}

/// `XML_GetUserData` is a macro in `expat.h` (not an exported symbol): the
/// userdata pointer is stored in the first word of the parser struct.
///
/// # Safety
/// `parser` must be a live parser handle.
pub unsafe fn XML_GetUserData(parser: XML_Parser) -> *mut c_void {
    unsafe { *(parser as *mut *mut c_void) }
}
