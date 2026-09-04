package io.nanochronometer;

public final class OverheadExample {
    public static void main(String[] args) {
        try (NanoChronometer nc = new NanoChronometer()) {
            nc.calibrate(50);
            System.out.println("native overhead cycles: " + nc.nativeOverheadCycles());
            System.out.println("Java -> native JNA cycles: " + nc.ffiOverheadCycles(10000));
            System.out.println("native -> Java callback min cycles: " + nc.callbackMinCycles(arg -> {}, 10000));
        }
    }
}
