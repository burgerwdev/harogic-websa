// C++ runtime + FDK system shim for bare wasm32-unknown-unknown (no libc/libc++).
// malloc/free/calloc/realloc are provided by the Rust host; memcpy/memset/memmove/
// memcmp by Rust's compiler-builtins. This file supplies the C++ runtime glue, a few
// string helpers, and the FDK_* memory/string wrappers genericStds.cpp would provide.
#include <stddef.h>
#include <stdint.h>

typedef unsigned int UINT;
typedef int INT;

extern "C" {
void *malloc(size_t n);
void free(void *p);
void *calloc(size_t n, size_t sz);
void *realloc(void *p, size_t n);
void *memcpy(void *d, const void *s, size_t n);
void *memmove(void *d, const void *s, size_t n);
void *memset(void *d, int c, size_t n);
int memcmp(const void *a, const void *b, size_t n);
}

// --- C++ runtime ---
void *operator new(size_t n) { return malloc(n); }
void *operator new[](size_t n) { return malloc(n); }
void operator delete(void *p) noexcept { free(p); }
void operator delete[](void *p) noexcept { free(p); }
void operator delete(void *p, size_t) noexcept { free(p); }
void operator delete[](void *p, size_t) noexcept { free(p); }

// --- string helpers ---
extern "C" size_t strlen(const char *s) { size_t n = 0; while (s[n]) n++; return n; }
extern "C" char *strcpy(char *d, const char *s) { char *r = d; while ((*d++ = *s++)) {} return r; }
extern "C" int strcmp(const char *a, const char *b) { while (*a && *a == *b) { a++; b++; } return (unsigned char)*a - (unsigned char)*b; }

// --- FDK system wrappers (genericStds.cpp) ---
extern "C" void *FDKcalloc(UINT n, UINT size) { return calloc(n, size); }
extern "C" void *FDKmalloc(UINT size) { return malloc(size); }
extern "C" void FDKfree(void *ptr) { free(ptr); }
// FDK asks for aligned buffers (up to 16 bytes in the SBR decoder), and FDK's own
// genericStds backs them with FDKcalloc — "malloc and clear": CAacDecoder_Init's
// persistent channel info is read before it is written (HCR side info), so the memory
// MUST come back zeroed. Mirror that: calloc, align up, stash the raw pointer in the
// word before the returned address so FDKafree can find it.
static void *aalloc(UINT size, UINT alignment) {
    if (alignment < sizeof(void *)) alignment = sizeof(void *);
    void *raw = calloc(1, (size_t)size + alignment - 1 + sizeof(void *));
    if (!raw) return 0;
    uintptr_t addr = reinterpret_cast<uintptr_t>(raw) + sizeof(void *);
    addr = (addr + alignment - 1) & ~static_cast<uintptr_t>(alignment - 1);
    *reinterpret_cast<void **>(addr - sizeof(void *)) = raw;
    return reinterpret_cast<void *>(addr);
}
extern "C" void *FDKaalloc(UINT size, UINT alignment) { return aalloc(size, alignment); }
extern "C" void FDKafree(void *ptr) {
    if (!ptr) return;
    free(*reinterpret_cast<void **>(reinterpret_cast<uintptr_t>(ptr) - sizeof(void *)));
}
extern "C" void *FDKcalloc_L(UINT n, UINT size, int /*s*/) { return calloc(n, size); }
extern "C" void *FDKaalloc_L(UINT size, UINT alignment, int /*s*/) { return aalloc(size, alignment); }
extern "C" void FDKfree_L(void *ptr) { free(ptr); }
extern "C" void FDKafree_L(void *ptr) { FDKafree(ptr); }
extern "C" void FDKmemcpy(void *dst, const void *src, UINT size) { memcpy(dst, src, size); }
extern "C" void FDKmemmove(void *dst, const void *src, UINT size) { memmove(dst, src, size); }
extern "C" void FDKmemclear(void *memPtr, UINT size) { memset(memPtr, 0, size); }
extern "C" void FDKmemset(void *memPtr, INT value, UINT size) { memset(memPtr, value, size); }
extern "C" INT FDKmemcmp(const void *s1, const void *s2, UINT size) { return memcmp(s1, s2, size); }
// Only used by FDK_toolsGetLibInfo (version string); not on the decode path.
extern "C" int FDKsprintf(char *str, const char *, ...) { if (str) str[0] = 0; return 0; }
