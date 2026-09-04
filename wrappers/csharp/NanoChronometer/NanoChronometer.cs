using System;
using System.Runtime.InteropServices;

namespace NanoChronometer;

public sealed class NanoChrono : IDisposable
{
    private const string Lib = "nanochrono";
    private IntPtr ctx;

    [StructLayout(LayoutKind.Sequential)]
    public struct SampleStats
    {
        public ulong Count, MinCycles, MaxCycles, MeanCycles, MedianCycles, P90Cycles, P99Cycles;
        public double VarianceCycles, StdevCycles;
    }

    [UnmanagedFunctionPointer(CallingConvention.Cdecl)]
    public delegate void VoidCallback(IntPtr arg);

    [DllImport(Lib, CallingConvention = CallingConvention.Cdecl)] private static extern IntPtr nc_create();
    [DllImport(Lib, CallingConvention = CallingConvention.Cdecl)] private static extern void nc_destroy(IntPtr ctx);
    [DllImport(Lib, CallingConvention = CallingConvention.Cdecl)] private static extern int nc_calibrate(IntPtr ctx, uint ms);
    [DllImport(Lib, CallingConvention = CallingConvention.Cdecl)] private static extern ulong nc_start(IntPtr ctx);
    [DllImport(Lib, CallingConvention = CallingConvention.Cdecl)] private static extern ulong nc_stop(IntPtr ctx);
    [DllImport(Lib, CallingConvention = CallingConvention.Cdecl)] private static extern ulong nc_now_cycles(IntPtr ctx);
    [DllImport(Lib, CallingConvention = CallingConvention.Cdecl)] private static extern ulong nc_cycles_to_ns(IntPtr ctx, ulong cycles);
    [DllImport(Lib, CallingConvention = CallingConvention.Cdecl)] private static extern ulong nc_elapsed_ns(IntPtr ctx);
    [DllImport(Lib, CallingConvention = CallingConvention.Cdecl)] private static extern ulong nc_measure_overhead_cycles(IntPtr ctx);
    [DllImport(Lib, CallingConvention = CallingConvention.Cdecl)] private static extern ulong nc_measure_ffi_overhead_cycles(IntPtr ctx, uint iterations);
    [DllImport(Lib, CallingConvention = CallingConvention.Cdecl)] private static extern ulong nc_measure_callback_min_cycles(IntPtr ctx, VoidCallback cb, IntPtr arg, uint iterations);
    [DllImport(Lib, CallingConvention = CallingConvention.Cdecl)] private static extern int nc_analyze_samples([In] ulong[] samples, uint count, out SampleStats stats);
    [DllImport(Lib, CallingConvention = CallingConvention.Cdecl)] private static extern double nc_welch_t_score([In] ulong[] a, uint na, [In] ulong[] b, uint nb);

    public NanoChrono()
    {
        ctx = nc_create();
        if (ctx == IntPtr.Zero) throw new InvalidOperationException("nc_create failed");
    }

    public bool Calibrate(uint ms = 50) => nc_calibrate(ctx, ms) != 0;
    public ulong Start() => nc_start(ctx);
    public ulong Stop() => nc_stop(ctx);
    public ulong NowCycles() => nc_now_cycles(ctx);
    public ulong CyclesToNs(ulong cycles) => nc_cycles_to_ns(ctx, cycles);
    public ulong ElapsedNs() => nc_elapsed_ns(ctx);
    public ulong NativeOverheadCycles() => nc_measure_overhead_cycles(ctx);
    public ulong FfiOverheadCycles(uint iterations = 10000) => nc_measure_ffi_overhead_cycles(ctx, iterations);
    public ulong CallbackMinCycles(VoidCallback cb, uint iterations = 10000) => nc_measure_callback_min_cycles(ctx, cb, IntPtr.Zero, iterations);
    public static bool AnalyzeSamples(ulong[] samples, out SampleStats stats) => nc_analyze_samples(samples, (uint)samples.Length, out stats) != 0;
    public static double WelchTScore(ulong[] a, ulong[] b) => nc_welch_t_score(a, (uint)a.Length, b, (uint)b.Length);

    public void Dispose()
    {
        if (ctx != IntPtr.Zero) { nc_destroy(ctx); ctx = IntPtr.Zero; }
        GC.SuppressFinalize(this);
    }
    ~NanoChrono() => Dispose();
}
