# Stable clock calibration: cycles/ticks -> nanoseconds

Flow for extreme benchmarking / HFT-style low-overhead timing:

1. Stabilize the CPU where the OS/firmware allows it: disable turbo/boost, lock the governor to performance, pin affinity, isolate the benchmark core, and reduce deep C-states.
2. Calibrate the selected hardware counter route. The framework measures raw counter delta versus monotonic nanoseconds and computes `cycles_per_ns`.
3. Convert raw deltas using:

```text
ns = cycles / cycles_per_ns
```

On x64, routes include RDTSC, LFENCE+RDTSC, MFENCE+LFENCE+RDTSC and RDTSCP+LFENCE. On ARM64, routes include CNTVCT_EL0/CNTVCT_EL0+ISB and optional PMCCNTR_EL0 when userspace PMU access is enabled.

CLI examples:

```bash
nanochrono_cli --stable-calibrate --pin-cpu 0 --ms 1000
nanochrono_cli --cycles-to-ns 3200 --cycles-per-ns 3.200000
```

Critical path rule: after calibration, do not query NTP, do not call the kernel clock, and do not allocate. Read the raw counter, subtract, then convert with the calibrated factor.

Important: MiFID II/RTS 25 compliance requires an audited end-to-end time synchronization architecture, traceability, maximum divergence controls, monitoring, and operational evidence. This library provides measurement primitives and calibration support; it does not certify compliance by itself.
