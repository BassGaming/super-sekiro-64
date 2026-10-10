/* dirent.h stub: libsm64's tool helpers include it but never list directories at runtime */
#pragma once
#include <stddef.h>
typedef struct DIR DIR;
struct dirent { char d_name[260]; };
static DIR *opendir(const char *p) { (void)p; return NULL; }
static struct dirent *readdir(DIR *d) { (void)d; return NULL; }
static int closedir(DIR *d) { (void)d; return 0; }
