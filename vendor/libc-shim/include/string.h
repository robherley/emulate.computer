/* Minimal freestanding string.h shim for building riscv-tests env/v
 * with a bare-metal (no-newlib) toolchain. The implementations live in
 * riscv-tests/env/v/string.c. */
#ifndef _SHIM_STRING_H
#define _SHIM_STRING_H

#include <stddef.h>

void *memcpy(void *dest, const void *src, size_t len);
void *memset(void *dest, int byte, size_t len);
int memcmp(const void *s1, const void *s2, size_t n);
size_t strlen(const char *s);
int strcmp(const char *s1, const char *s2);
char *strcpy(char *dest, const char *src);
long atol(const char *str);

#endif
