using NanoChronometer;

using var nc = new NanoChrono();
nc.Calibrate(50);
Console.WriteLine($"native overhead cycles: {nc.NativeOverheadCycles()}");
Console.WriteLine($"C# -> native P/Invoke cycles: {nc.FfiOverheadCycles(10000)}");
Console.WriteLine($"native -> C# callback min cycles: {nc.CallbackMinCycles(_ => {})}");
