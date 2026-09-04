package main

import (
	"fmt"
	"github.com/nanochronometer/wrappers/go/nanochrono"
)

func main() {
	nc := nanochrono.New()
	if nc == nil { panic("nanochrono not loaded") }
	defer nc.Close()
	nc.Calibrate(50)
	fmt.Println("native overhead cycles:", nc.NativeOverheadCycles())
	fmt.Println("Go -> C FFI cycles:", nc.FFIOverheadCycles(10000))
	fmt.Println("C callback noop min cycles:", nc.CCallbackNoopMinCycles(10000))
}
