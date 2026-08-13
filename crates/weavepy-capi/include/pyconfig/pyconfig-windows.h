/* pyconfig.h for WeavePy on Windows (RFC 0064 WS3).
 *
 * CPython ships a hand-maintained `PC/pyconfig.h` on Windows (there
 * is no autoconf step); this is WeavePy's equivalent, shaped after
 * CPython 3.13's file: the same platform macros, LLP64 type sizes,
 * shared-core markers, and — critically — the MSVC autolink pragma
 * that makes `cl /LD ext.c /I{Include}` pull `python313.lib` off the
 * `/LIBPATH` without the build script naming it. setuptools points
 * `/LIBPATH:` at `{sys.base_exec_prefix}\libs`, where the WeavePy
 * artifact ships the import library for `python313.dll`.
 */

#ifndef Py_CONFIG_H
#define Py_CONFIG_H

/* --- platform identification -------------------------------------- */

#define MS_WIN32 /* only support win32 and greater. */
#define MS_WINDOWS
#ifdef _WIN64
#define MS_WIN64
#endif

#define _Py_STRINGIZE(X) _Py_STRINGIZE1(X)
#define _Py_STRINGIZE1(X) #X

/* set the COMPILER and support tier (1 for x64, 3 elsewhere;
 * WeavePy's shipped target is x86_64-pc-windows-msvc) */
#ifdef MS_WIN64
#if defined(_M_X64) || defined(_M_AMD64)
#define COMPILER ("[MSC v." _Py_STRINGIZE(_MSC_VER) " 64 bit (AMD64)]")
#define PY_SUPPORT_TIER 1
#elif defined(_M_ARM64)
#define COMPILER ("[MSC v." _Py_STRINGIZE(_MSC_VER) " 64 bit (ARM64)]")
#define PY_SUPPORT_TIER 3
#else
#define COMPILER ("[MSC v." _Py_STRINGIZE(_MSC_VER) " 64 bit (Unknown)]")
#define PY_SUPPORT_TIER 0
#endif
#endif /* MS_WIN64 */

/* Debug builds: MSVC's _DEBUG selects CPython's debug ABI. WeavePy
 * does not ship python313_d.dll, so a Debug extension build fails at
 * link with a clear missing-python313_d.lib error — the same failure
 * a release-only CPython install produces. */
#ifdef _DEBUG
#define Py_DEBUG 1
#endif

/* --- shared core + autolink ---------------------------------------- */

/* The Python runtime is a DLL (python313.dll — RFC 0064 WS1). */
#define MS_COREDLL 1
#define Py_ENABLE_SHARED 1

/* Declspec shaping for the stock headers' PyAPI_FUNC/PyAPI_DATA. */
#define HAVE_DECLSPEC_DLL

#ifdef MS_COREDLL
#if !defined(Py_BUILD_CORE) && !defined(Py_BUILD_CORE_BUILTIN)
/* not building the core — must be an extension or embedder: have
 * MSVC pull the import library automatically. */
#if defined(_MSC_VER)
#if defined(_DEBUG)
#pragma comment(lib, "python313_d.lib")
#elif defined(Py_LIMITED_API)
/* CPython points the limited API at python3.lib (the stable-ABI
 * forwarder DLL's import library). WeavePy does not ship the
 * forwarder yet (RFC 0064 Future work), so limited-API builds link
 * the full runtime library — the resulting .pyd imports
 * python313.dll and works on WeavePy 3.13. */
#pragma comment(lib, "python313.lib")
#else
#pragma comment(lib, "python313.lib")
#endif /* _DEBUG */
#endif /* _MSC_VER */
#endif /* Py_BUILD_CORE */
#endif /* MS_COREDLL */

/* --- type sizes (LLP64) -------------------------------------------- */

#define SIZEOF_SHORT 2
#define SIZEOF_INT 4
#define SIZEOF_LONG 4
#define SIZEOF_LONG_LONG 8
#define SIZEOF_FLOAT 4
#define SIZEOF_DOUBLE 8
#define SIZEOF_WCHAR_T 2
#define SIZEOF_FPOS_T 8
#define SIZEOF_TIME_T 8
/* off_t is 32 bits on Windows; large files go through fpos_t. */
#define SIZEOF_OFF_T 4
#define HAVE_LARGEFILE_SUPPORT 1

#ifdef MS_WIN64
#define SIZEOF_VOID_P 8
#define SIZEOF_SIZE_T 8
#define SIZEOF_HKEY 8
#define SIZEOF_PID_T 4
#else
#define SIZEOF_VOID_P 4
#define SIZEOF_SIZE_T 4
#define SIZEOF_HKEY 4
#define SIZEOF_PID_T 4
#endif

#define WORD_BIT 32

/* MSVC provides ssize_t via SSIZE_T (BaseTsd.h); the stock headers
 * only need the macro that says the typedef exists once pyport.h has
 * mapped it. CPython's PC/pyconfig.h does exactly this. */
#if defined(MS_WIN64)
typedef __int64 ssize_t;
#else
typedef _W64 int ssize_t;
#endif
#define HAVE_SSIZE_T 1

/* --- capabilities the stock headers key off ------------------------ */

#define WITH_DOC_STRINGS 1
#define HAVE_DYNAMIC_LOADING 1
#define HAVE_STRERROR 1
#define HAVE_CLOCK 1
#define HAVE_IO_H 1
#define HAVE_SYS_UTIME_H 1
#define HAVE_SYS_TYPES_H 1
#define HAVE_SYS_STAT_H 1
#define HAVE_ERRNO_H 1
#define HAVE_STDDEF_H 1
#define HAVE_STDINT_H 1
#define HAVE_WCHAR_H 1
#define HAVE_FCNTL_H 1
#define HAVE_DIRECT_H 1
#define HAVE_PROCESS_H 1
#define HAVE_SIGNAL_H 1

/* Threading: native NT threads, exactly one flavour. */
#define NT_THREADS 1
#define WITH_THREAD 1

/* IEEE-754 doubles, little-endian (every supported Windows arch). */
#define DOUBLE_IS_LITTLE_ENDIAN_IEEE754 1

/* IPv6 (Winsock2 has shipped it since XP). */
#define ENABLE_IPV6 1

/* Sockets are real handles on NT. */
#define USE_SOCKET 1

/* Not a debug/free-threaded/tracing build (mirrors
 * _weave_sysconfigdata). */
/* #undef Py_GIL_DISABLED */
/* #undef Py_TRACE_REFS */
/* #undef Py_REF_DEBUG */

#endif /* !Py_CONFIG_H */
