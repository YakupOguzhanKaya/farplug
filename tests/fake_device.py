#!/usr/bin/env python3
"""A USB/IP server that emulates one full-speed device in userspace.

It stands in for a real gateway so the client can be tested against the real
vhci_hcd driver on a machine (like Docker Desktop's VM) that has no usbip-host.
Written independently of the Rust protocol crate, so it also cross-checks it.

KIND is "plain" (a vendor-specific device with no endpoints, the default),
"cp210x" or "cdcacm": a USB-serial adapter that echoes what it is sent and
prints the line settings it is given, for the userspace serial client, or
"hid": a keyboard and mouse receiver that sends a few scripted reports.

usage: fake_device.py PORT BUSID [KIND]
"""
import socket
import struct
import sys
import threading

PORT, BUSID = int(sys.argv[1]), sys.argv[2]
KIND = sys.argv[3] if len(sys.argv) > 3 else "plain"
VERSION = 0x0111
BUSNUM, DEVNUM, SPEED_FULL = 3, 9, 2

def endpoint(addr, kind, size):
    return struct.pack("<BBBBHB", 7, 5, addr, kind, size, 0)


def interface(num, n_eps, cls, sub=0, proto=0):
    return struct.pack("<BBBBBBBBB", 9, 4, num, 0, n_eps, cls, sub, proto, 0)


def config(n_ifaces, body):
    return struct.pack("<BBHBBBBB", 9, 2, 9 + len(body), n_ifaces, 1, 0, 0x80, 50) + body


VID, PID, DEV_CLASS = {"plain": (0x1209, 0x0001, 0xFF), "cp210x": (0x10C4, 0xEA60, 0),
                       "cdcacm": (0x1209, 0x0002, 2), "hid": (0x046D, 0xC548, 0)}[KIND]
DEVICE_DESC = struct.pack("<BBHBBBBHHHBBBB", 18, 1, 0x0200, DEV_CLASS, 0, 0, 64,
                          VID, PID, 0x0100, 1, 2, 3, 1)
if KIND == "cp210x":
    IFACES = [(0xFF, 0, 0)]
    CONFIG_DESC = config(1, interface(0, 2, 0xFF) + endpoint(0x81, 2, 64) + endpoint(0x01, 2, 64))
elif KIND == "cdcacm":
    IFACES = [(2, 2, 1), (0x0A, 0, 0)]
    CONFIG_DESC = config(2, interface(0, 1, 2, 2, 1) + bytes([5, 0x24, 0, 0x10, 1])
                         + endpoint(0x83, 3, 8)
                         + interface(1, 2, 0x0A) + endpoint(0x81, 2, 64) + endpoint(0x02, 2, 64))
elif KIND == "hid":
    IFACES = [(3, 1, 1), (3, 1, 2)]
    CONFIG_DESC = config(2, interface(0, 1, 3, 1, 1) + endpoint(0x81, 3, 8)
                         + interface(1, 1, 3, 1, 2) + endpoint(0x82, 3, 4))
else:
    IFACES = [(0xFF, 0, 0)]
    CONFIG_DESC = config(1, interface(0, 0, 0xFF))

# hid: reports per IN endpoint, sent one per request; afterwards requests
# stay pending like those to an idle device.
SCRIPT = {1: [bytes([0x02, 0, 0x04, 0, 0, 0, 0, 0]), bytes(8)],
          2: [bytes([0x01, 5, 0xFE, 0xFF]), bytes(4)]}
STRINGS = ["farplug", "Fake Device", "TEST0001"]


def string_desc(i):
    if i == 0:
        return struct.pack("<BBH", 4, 3, 0x0409)
    s = STRINGS[i - 1].encode("utf-16-le")
    return struct.pack("<BB", 2 + len(s), 3) + s


def udev(busid):
    return (struct.pack(">256s32s", b"/fake/" + busid.encode(), busid.encode())
            + struct.pack(">IIIHHHBBBBBB", BUSNUM, DEVNUM, SPEED_FULL, VID, PID,
                          0x0100, DEV_CLASS, 0, 0, 1, 1, len(IFACES)))


def recv(sock, n):
    buf = b""
    while len(buf) < n:
        chunk = sock.recv(n - len(buf))
        if not chunk:
            raise EOFError
        buf += chunk
    return buf


def control(setup, data):
    req_type, req, value, index, length = struct.unpack("<BBHHH", setup)
    if req_type & 0x60:  # class or vendor: serial line settings
        if KIND == "cp210x" and req == 0x1E:
            print("SET_BAUDRATE", struct.unpack("<I", data)[0], flush=True)
        elif KIND == "cp210x" and req == 0x03:
            print(f"SET_LINE_CTL {value:#06x}", flush=True)
        elif KIND == "cdcacm" and req == 0x20:
            baud, stop, parity, bits = struct.unpack("<IBBB", data)
            print(f"SET_LINE_CODING {baud} {bits}{'NOEMS'[parity]}{stop + 1}", flush=True)
        else:
            print(f"REQUEST {req_type:#04x} {req:#04x} {value:#06x}", flush=True)
        return 0, b""
    if req == 6:  # GET_DESCRIPTOR
        kind, idx = value >> 8, value & 0xFF
        data = {1: DEVICE_DESC, 2: CONFIG_DESC}.get(kind)
        if kind == 3 and idx <= len(STRINGS):
            data = string_desc(idx)
        if data is None:
            return -32, b""  # -EPIPE: stall
        return 0, data[:length]
    if req == 0:  # GET_STATUS
        return 0, b"\0\0"[:length]
    if req in (9, 11):  # SET_CONFIGURATION, SET_INTERFACE
        return 0, b""
    return -32, b""


def serve_urbs(sock):
    echo = b""     # what the device was sent and will send back
    waiting = []   # IN transfers on the data endpoint, answered when data comes

    def reply(seq, status, data=b""):
        sock.sendall(struct.pack(">IIIIIiiiii8x", 3, seq, 0, 0, 0, status,
                                 len(data), 0, 0, 0) + data)

    while True:
        cmd, seq, _devid, direction, ep = struct.unpack(">IIIII", recv(sock, 20))
        rest = recv(sock, 28)
        if cmd == 1:  # CMD_SUBMIT
            _flags, length, _sf, _npk, _intv = struct.unpack(">IiiiI", rest[:20])
            setup = rest[20:28]
            out = recv(sock, length) if direction == 0 and length > 0 else b""
            if ep == 0:
                status, data = control(setup, out)
                reply(seq, status, data if direction == 1 else b"")
            elif KIND == "hid" and direction == 1 and ep in SCRIPT and _intv == 0:
                reply(seq, -22)  # -EINVAL: Linux refuses interrupt URBs without an interval
            elif KIND == "hid" and direction == 1 and ep in SCRIPT:
                if SCRIPT[ep]:
                    reply(seq, 0, SCRIPT[ep].pop(0))
                else:
                    waiting.append((seq, length))
            elif KIND != "plain" and direction == 0:
                echo += out
                reply(seq, 0)
            elif KIND != "plain" and direction == 1 and ep == 1:
                waiting.append((seq, length))
            else:
                reply(seq, -32)
            while echo and waiting:
                wseq, wlen = waiting.pop(0)
                reply(wseq, 0, echo[:wlen])
                echo = echo[wlen:]
        elif cmd == 2:  # CMD_UNLINK
            victim = struct.unpack(">I", rest[:4])[0]
            pending = [w for w in waiting if w[0] == victim]
            waiting = [w for w in waiting if w[0] != victim]
            status = -104 if pending else 0  # -ECONNRESET: cancelled in time
            sock.sendall(struct.pack(">IIIIIi24x", 4, seq, 0, 0, 0, status))
        else:
            raise ValueError(f"unknown command {cmd}")


def handle(sock):
    version, code, _status = struct.unpack(">HHI", recv(sock, 8))
    assert version == VERSION, hex(version)
    if code == 0x8005:  # OP_REQ_DEVLIST
        sock.sendall(struct.pack(">HHII", VERSION, 0x0005, 0, 1) + udev(BUSID)
                     + b"".join(bytes([c, sc, p, 0]) for c, sc, p in IFACES))
        return
    if code == 0x8003:  # OP_REQ_IMPORT
        want = recv(sock, 32).rstrip(b"\0").decode()
        if want != BUSID:
            sock.sendall(struct.pack(">HHI", VERSION, 0x0003, 4))
            return
        sock.sendall(struct.pack(">HHI", VERSION, 0x0003, 0) + udev(want))
        print(f"imported {want}", flush=True)
        try:
            serve_urbs(sock)
        except (EOFError, ConnectionError):
            print("client went away", flush=True)


def main():
    srv = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    srv.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    srv.bind(("127.0.0.1", PORT))
    srv.listen()
    print(f"fake device {BUSID} on port {PORT}", flush=True)
    while True:
        conn, _ = srv.accept()
        threading.Thread(target=lambda c=conn: (handle(c), c.close()), daemon=True).start()


main()
