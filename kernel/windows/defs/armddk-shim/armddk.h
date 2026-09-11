#ifndef _ARMDDK_
#define _ARMDDK_
#if defined(__aarch64__) || defined(_ARM64_)
UCHAR __cdecl KfRaiseIrql(UCHAR NewIrql);
void __cdecl KfLowerIrql(UCHAR NewIrql);
#define YieldProcessor() __asm__ __volatile__("yield")
#else
#error "armddk.h shim is aarch64-only"
#endif
#endif
