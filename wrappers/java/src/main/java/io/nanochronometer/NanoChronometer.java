package io.nanochronometer;

import com.sun.jna.*;
import com.sun.jna.ptr.PointerByReference;

public final class NanoChronometer implements AutoCloseable {
    interface Lib extends Library {
        Lib INSTANCE = Native.load(System.getProperty("nanochrono.lib", "nanochrono"), Lib.class);
        Pointer nc_create();
        void nc_destroy(Pointer ctx);
        int nc_calibrate(Pointer ctx, int ms);
        long nc_start(Pointer ctx);
        long nc_stop(Pointer ctx);
        long nc_now_cycles(Pointer ctx);
        long nc_cycles_to_ns(Pointer ctx, long cycles);
        long nc_elapsed_ns(Pointer ctx);
        long nc_measure_overhead_cycles(Pointer ctx);
        long nc_measure_ffi_overhead_cycles(Pointer ctx, int iterations);
        long nc_measure_callback_min_cycles(Pointer ctx, VoidCallback cb, Pointer arg, int iterations);
        int nc_analyze_samples(long[] samples, int count, SampleStats stats);
        double nc_welch_t_score(long[] a, int na, long[] b, int nb);
    }
    public interface VoidCallback extends Callback { void invoke(Pointer arg); }
    public static class SampleStats extends Structure {
        public long count, min_cycles, max_cycles, mean_cycles, median_cycles, p90_cycles, p99_cycles;
        public double variance_cycles, stdev_cycles;
        @Override protected java.util.List<String> getFieldOrder() {
            return java.util.Arrays.asList("count","min_cycles","max_cycles","mean_cycles","median_cycles","p90_cycles","p99_cycles","variance_cycles","stdev_cycles");
        }
    }

    private Pointer ctx;
    public NanoChronometer() {
        ctx = Lib.INSTANCE.nc_create();
        if (ctx == null) throw new IllegalStateException("nc_create failed");
    }
    public boolean calibrate(int ms) { return Lib.INSTANCE.nc_calibrate(ctx, ms) != 0; }
    public long start() { return Lib.INSTANCE.nc_start(ctx); }
    public long stop() { return Lib.INSTANCE.nc_stop(ctx); }
    public long nowCycles() { return Lib.INSTANCE.nc_now_cycles(ctx); }
    public long cyclesToNs(long cycles) { return Lib.INSTANCE.nc_cycles_to_ns(ctx, cycles); }
    public long elapsedNs() { return Lib.INSTANCE.nc_elapsed_ns(ctx); }
    public long nativeOverheadCycles() { return Lib.INSTANCE.nc_measure_overhead_cycles(ctx); }
    public long ffiOverheadCycles(int iterations) { return Lib.INSTANCE.nc_measure_ffi_overhead_cycles(ctx, iterations); }
    public long callbackMinCycles(VoidCallback cb, int iterations) { return Lib.INSTANCE.nc_measure_callback_min_cycles(ctx, cb, null, iterations); }
    public static SampleStats analyzeSamples(long[] samples) {
        SampleStats s = new SampleStats();
        if (Lib.INSTANCE.nc_analyze_samples(samples, samples.length, s) == 0) throw new IllegalArgumentException("bad samples");
        return s;
    }
    public static double welchTScore(long[] a, long[] b) { return Lib.INSTANCE.nc_welch_t_score(a, a.length, b, b.length); }
    public void close() { if (ctx != null) { Lib.INSTANCE.nc_destroy(ctx); ctx = null; } }
}
