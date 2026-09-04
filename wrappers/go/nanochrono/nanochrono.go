package nanochrono

/*
#cgo CFLAGS: -I../../../
#cgo windows LDFLAGS: -L../../../build -lnanochrono
#cgo linux LDFLAGS: -L../../../build -lnanochrono -lm
#include "nanochrono.h"

static void go_noop_bridge(void *arg) { (void)arg; }
*/
import "C"
import "unsafe"

type Context struct{ ptr *C.nc_ctx_t }

type SampleStats struct {
	Count, MinCycles, MaxCycles, MeanCycles, MedianCycles, P90Cycles, P99Cycles uint64
	VarianceCycles, StdevCycles float64
}

func New() *Context {
	p := C.nc_create()
	if p == nil { return nil }
	return &Context{ptr: p}
}
func (c *Context) Close() { if c != nil && c.ptr != nil { C.nc_destroy(c.ptr); c.ptr = nil } }
func (c *Context) Calibrate(ms uint32) bool { return C.nc_calibrate(c.ptr, C.uint(ms)) != 0 }
func (c *Context) Start() uint64 { return uint64(C.nc_start(c.ptr)) }
func (c *Context) Stop() uint64 { return uint64(C.nc_stop(c.ptr)) }
func (c *Context) NowCycles() uint64 { return uint64(C.nc_now_cycles(c.ptr)) }
func (c *Context) CyclesToNS(cycles uint64) uint64 { return uint64(C.nc_cycles_to_ns(c.ptr, C.uint64_t(cycles))) }
func (c *Context) ElapsedNS() uint64 { return uint64(C.nc_elapsed_ns(c.ptr)) }
func (c *Context) NativeOverheadCycles() uint64 { return uint64(C.nc_measure_overhead_cycles(c.ptr)) }
func (c *Context) FFIOverheadCycles(iterations uint32) uint64 { return uint64(C.nc_measure_ffi_overhead_cycles(c.ptr, C.uint(iterations))) }
func (c *Context) CCallbackNoopMinCycles(iterations uint32) uint64 {
	return uint64(C.nc_measure_callback_min_cycles(c.ptr, (C.nc_void_callback_t)(unsafe.Pointer(C.go_noop_bridge)), nil, C.uint(iterations)))
}
func AnalyzeSamples(samples []uint64) (SampleStats, bool) {
	var out C.nc_sample_stats_t
	if len(samples) == 0 { return SampleStats{}, false }
	ok := C.nc_analyze_samples((*C.uint64_t)(unsafe.Pointer(&samples[0])), C.uint(len(samples)), &out) != 0
	return SampleStats{uint64(out.count), uint64(out.min_cycles), uint64(out.max_cycles), uint64(out.mean_cycles), uint64(out.median_cycles), uint64(out.p90_cycles), uint64(out.p99_cycles), float64(out.variance_cycles), float64(out.stdev_cycles)}, ok
}
func WelchTScore(a, b []uint64) float64 {
	if len(a) == 0 || len(b) == 0 { return 0 }
	return float64(C.nc_welch_t_score((*C.uint64_t)(unsafe.Pointer(&a[0])), C.uint(len(a)), (*C.uint64_t)(unsafe.Pointer(&b[0])), C.uint(len(b))))
}

type InstructionFamily uint32
const (
    InstrScalar InstructionFamily = iota; InstrAES; InstrSHA; InstrPCLMULQDQ; InstrCRC32; InstrSSE2; InstrAVX2; InstrAVX512; InstrNEON; InstrSVE; InstrSVE2; InstrSME
)
type InstructionResult struct { Status int32; Family, Backend uint32; Cycles, NS, Blocks, Checksum uint64 }
func InstructionAvailable(f InstructionFamily) bool { return C.nc_instruction_family_available(C.nc_instruction_family_t(f)) != 0 }
func (c *Context) MeasureInstruction(f InstructionFamily, iterations uint32) InstructionResult { var r C.nc_instruction_result_t; C.nc_measure_instruction_family_cycles(c.ptr, C.nc_instruction_family_t(f), C.uint(iterations), &r); return InstructionResult{int32(r.status), uint32(r.family), uint32(r.backend), uint64(r.cycles), uint64(r.ns), uint64(r.blocks), uint64(r.checksum)} }
func (c *Context) MeasureAES(blocks uint32) InstructionResult { var r C.nc_instruction_result_t; C.nc_measure_aesenc_cycles(c.ptr, C.uint(blocks), &r); return InstructionResult{int32(r.status), uint32(r.family), uint32(r.backend), uint64(r.cycles), uint64(r.ns), uint64(r.blocks), uint64(r.checksum)} }
func CryptoBackendMask() int { return int(C.nc_crypto_backend_mask()) }
