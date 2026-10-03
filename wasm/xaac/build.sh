#!/bin/bash
# Rebuild libxaac.a (wasm32-unknown-unknown) from the Ittiam libxaac sources.
#
# Usage: wasm/xaac/build.sh /path/to/libxaac
#
# libxaac is the xHE-AAC (MPEG-D USAC) decoder. It is plain C and uses a handful of
# hosted libc headers (<string.h>, <math.h>, <stdlib.h>, <setjmp.h>, …) that a bare
# wasm32-unknown-unknown target does not ship; this script provides minimal stubs.
# malloc/free come from the Rust host, memcpy/memset from Rust's compiler-builtins,
# and the math functions from Rust's vendored libm. setjmp/longjmp are stubbed to
# "no error" / trap: libxaac uses them only for bitstream-error recovery, which a
# clean decode never triggers.
set -euo pipefail

SRC="${1:?usage: wasm/xaac/build.sh /path/to/libxaac}"
HERE="$(cd "$(dirname "$0")" && pwd)"

INC="$HERE/include"
mkdir -p "$INC"
cat > "$INC/string.h" <<'H'
#ifndef _STRING_H
#define _STRING_H
#define NULL ((void*)0)
typedef unsigned long size_t;
void *memcpy(void *d, const void *s, size_t n);
void *memmove(void *d, const void *s, size_t n);
void *memset(void *d, int c, size_t n);
int memcmp(const void *a, const void *b, size_t n);
size_t strlen(const char *s);
char *strcpy(char *d, const char *s);
char *strncpy(char *d, const char *s, size_t n);
int strcmp(const char *a, const char *b);
int strncmp(const char *a, const char *b, size_t n);
char *strcat(char *d, const char *s);
char *strchr(const char *s, int c);
#endif
H
cat > "$INC/stdlib.h" <<'H'
#ifndef _STDLIB_H
#define _STDLIB_H
#define NULL ((void*)0)
typedef unsigned long size_t;
void *malloc(size_t n); void free(void *p);
void *calloc(size_t n, size_t sz); void *realloc(void *p, size_t n);
void abort(void);
#endif
H
cat > "$INC/stdio.h" <<'H'
#ifndef _STDIO_H
#define _STDIO_H
#define NULL ((void*)0)
#endif
H
cat > "$INC/assert.h" <<'H'
#ifndef _ASSERT_H
#define _ASSERT_H
#define assert(x) ((void)0)
#endif
H
cat > "$INC/memory.h" <<'H'
#ifndef _MEMORY_H
#define _MEMORY_H
#include "string.h"
#endif
H
cat > "$INC/setjmp.h" <<'H'
#ifndef _SETJMP_H
#define _SETJMP_H
typedef int jmp_buf[1];
int setjmp(jmp_buf env);
void longjmp(jmp_buf env, int val);
#endif
H
cat > "$INC/math.h" <<'H'
#ifndef _MATH_H
#define _MATH_H
#define M_PI 3.14159265358979323846
double sin(double); double cos(double); double tan(double);
double sqrt(double); double fabs(double); float fabsf(float);
double pow(double,double); float powf(float,float);
double log(double); double log10(double); double exp(double);
double floor(double); double ceil(double); double fmod(double,double);
double atan(double); double atan2(double,double); double asin(double); double acos(double);
double cbrt(double); float cbrtf(float);
double sinh(double); double cosh(double); double tanh(double); double hypot(double,double);
float sinf(float); float cosf(float); float tanf(float);
float sqrtf(float); float logf(float); float log10f(float); float expf(float);
float floorf(float); float ceilf(float); float atanf(float); float atan2f(float,float);
float fminf(float,float); float fmaxf(float,float);
double fmin(double,double); double fmax(double,double);
double round(double); float roundf(float); double trunc(double);
#endif
H

CC="clang --target=wasm32-unknown-unknown -ffreestanding -fwrapv -O2 -std=c99 -DLOUDNESS_LEVELING_SUPPORT"
INCS="-I$INC -I$SRC/decoder -I$SRC/common"

OUT="$(mktemp -d)"
trap 'rm -rf "$OUT"' EXIT

# Decoder + common sources (decoder/libxaacdec.cmake, common/common.cmake).
FILES=$(cat "$SRC/decoder/libxaacdec.cmake" "$SRC/common/common.cmake" \
  | grep -oE '(decoder|common)/[a-z0-9_]+\.c' | sort -u)

for f in $FILES; do
  o="$OUT/$(basename "$f" .c).o"
  $CC $INCS -c "$SRC/$f" -o "$o"
done

# setjmp/longjmp stubs (see the header comment).
cat > "$OUT/xaac_shim.c" <<'S'
#include "setjmp.h"
int setjmp(jmp_buf env) { (void)env; return 0; }
void longjmp(jmp_buf env, int val) { (void)env; (void)val; __builtin_trap(); }
S
$CC $INCS -c "$OUT/xaac_shim.c" -o "$OUT/xaac_shim.o"

llvm-ar rcs "$HERE/libxaac.a" "$OUT"/*.o
echo "wrote $HERE/libxaac.a ($(ls -la "$HERE/libxaac.a" | awk '{print $5}') bytes)"
