/*
 * Expat configuration for WeavePy. This file is not part of the expat
 * distribution; it mirrors CPython's Modules/expat/expat_config.h without
 * depending on pyconfig.h (MSVC has no system expat_config.h).
 */
#ifndef EXPAT_CONFIG_H
#define EXPAT_CONFIG_H 1

#if defined(__BYTE_ORDER__) && (__BYTE_ORDER__ == __ORDER_BIG_ENDIAN__)
#define BYTEORDER 4321
#else
#define BYTEORDER 1234
#endif

#define HAVE_MEMMOVE 1

#define XML_NS 1
#define XML_DTD 1
#define XML_GE 1
#define XML_CONTEXT_BYTES 1024

/* Entropy is supplied per-parser via XML_SetHashSalt (see README). */
#define XML_POOR_ENTROPY 1
#undef HAVE_ARC4RANDOM
#undef HAVE_ARC4RANDOM_BUF
#undef HAVE_GETENTROPY
#undef HAVE_GETRANDOM
#undef HAVE_SYSCALL_GETRANDOM

#endif /* EXPAT_CONFIG_H */
