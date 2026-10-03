#ifndef _STDLIB_H
#define _STDLIB_H
#define NULL ((void*)0)
typedef unsigned long size_t;
void *malloc(size_t n); void free(void *p);
void *calloc(size_t n, size_t sz); void *realloc(void *p, size_t n);
void abort(void);
#endif
