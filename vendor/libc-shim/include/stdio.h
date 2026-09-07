/* Minimal freestanding stdio.h shim for building riscv-tests env/v.
 * env/v does its own console I/O over HTIF; nothing here is actually
 * called, the header just needs to exist. */
#ifndef _SHIM_STDIO_H
#define _SHIM_STDIO_H

#include <stddef.h>
#include <stdarg.h>

int printf(const char *fmt, ...);
int sprintf(char *str, const char *fmt, ...);
int snprintf(char *str, size_t size, const char *fmt, ...);
int vsnprintf(char *str, size_t size, const char *fmt, va_list ap);
int putchar(int c);
int puts(const char *s);

#endif
