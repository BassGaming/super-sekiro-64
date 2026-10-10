/* POSIX strings.h shim for building libsm64 with MSVC headers */
#pragma once
#include <string.h>
#define strcasecmp _stricmp
#define strncasecmp _strnicmp
