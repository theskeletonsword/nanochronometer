const { NanoChronometer } = require('..');
const nc = new NanoChronometer();
nc.calibrate(50);
console.log('native overhead cycles:', nc.nativeOverheadCycles().toString());
console.log('Node -> native ffi-napi cycles:', nc.ffiOverheadCycles(10000).toString());
console.log('native -> JS callback min cycles:', nc.callbackMinCycles(() => {}, 10000).toString());
nc.close();
