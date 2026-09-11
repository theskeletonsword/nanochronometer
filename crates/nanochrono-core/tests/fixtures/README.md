# Test fixtures

## `elan-i2c-hid.rdesc`

The HID report descriptor of an Elan `04F3:327E` touchpad, an I2C-HID device
(`ELAN06FA` / `PNP0C50`) on a Lenovo notebook. Read from
`/sys/bus/hid/devices/0018:04F3:327E.0004/report_descriptor`.

It is here because a report descriptor is the one part of a HID device that
cannot be reasoned about from the specification alone: what a *real* touchpad
declares — three collections, a vendor page in the middle, a mouse report
sharing the descriptor with a Precision Touchpad digitizer — is the thing the
parser has to survive. A synthetic descriptor proves the parser understands
the item encoding; this one proves it finds the right report in a descriptor
written by someone who was not thinking about it.

675 bytes. Not copyrightable subject matter: it is a hardware description
emitted by the device, of the form the USB-IF HID specification prescribes.

## `ite-notebook-keyboard.rdesc`

The HID report descriptor of an ITE `048D:C995` keyboard — the built-in
keyboard of the same Lenovo notebook. Read from
`/sys/bus/hid/devices/0003:048D:C995.0001/report_descriptor`.

Chosen over a textbook boot-keyboard descriptor because of what comes *first*
in it: three vendor-defined collections on page `0xff89`, one of them
declaring a 191-byte report, before the keyboard collection appears at all. A
parser that stops at the first collection, or that only records variable
fields, finds nothing here — and finding nothing looks exactly like a machine
with no keyboard.

Not copyrightable subject matter: a hardware description emitted by the
device, of the form the USB-IF HID specification prescribes.
