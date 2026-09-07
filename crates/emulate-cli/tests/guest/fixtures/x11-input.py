import ctypes as c
import sys
import time

x = c.CDLL("libX11.so.6")
x.XOpenDisplay.argtypes = [c.c_char_p]
x.XOpenDisplay.restype = c.c_void_p
x.XDefaultRootWindow.argtypes = [c.c_void_p]
x.XDefaultRootWindow.restype = c.c_ulong
x.XQueryPointer.argtypes = [c.c_void_p, c.c_ulong, c.POINTER(c.c_ulong),
                           c.POINTER(c.c_ulong), *([c.POINTER(c.c_int)] * 4),
                           c.POINTER(c.c_uint)]
x.XQueryKeymap.argtypes = [c.c_void_p, c.POINTER(c.c_ubyte)]
x.XCloseDisplay.argtypes = [c.c_void_p]
display = x.XOpenDisplay(b":0")
assert display, "cannot open X display"
root = x.XDefaultRootWindow(display)
expected = tuple(map(int, sys.argv[1:]))
try:
    for _ in range(100):
        root_return, child = c.c_ulong(), c.c_ulong()
        rx, ry, wx, wy = (c.c_int() for _ in range(4))
        mask = c.c_uint()
        assert x.XQueryPointer(display, root, c.byref(root_return), c.byref(child),
                               c.byref(rx), c.byref(ry), c.byref(wx), c.byref(wy),
                               c.byref(mask))
        keys = (c.c_ubyte * 32)()
        x.XQueryKeymap(display, keys)
        # X keycodes add 8 to Linux evdev codes; KEY_A is 30.
        state = (rx.value, ry.value, int(bool(mask.value & 256)),
                 int(bool(keys[38 // 8] & (1 << (38 % 8)))))
        if state == expected:
            print("X11_INPUT_" + "OK", *state, flush=True)
            break
        time.sleep(0.1)
    else:
        raise AssertionError((state, expected))
finally:
    x.XCloseDisplay(display)
