#!/usr/bin/env python3
"""A USB/IP server that emulates one trivial full-speed device in userspace.

It stands in for a real gateway so the client can be tested against the real
vhci_hcd driver on a machine (like Docker Desktop's VM) that has no usbip-host.
Written independently of the Rust protocol crate, so it also cross-checks it.

usage: fake_device.py PORT BUSID
"""
import socket
import struct
import sys
import threading

PORT, BUSID = int(sys.argv[1]), sys.argv[2]
VERSION = 0x0111
BUSNUM, DEVNUM, SPEED_FULL = 3, 9, 2

DEVICE_DESC = struct.pack("<BBHBBBBHHHBBBB", 18, 1, 0x0200, 0xFF, 0, 0, 64,
                          0x1209, 0x0001, 0x0100, 1, 2, 3, 1)
CONFIG_DESC = (struct.pack("<BBHBBBBB", 9, 2, 18, 1, 1, 0, 0x80, 50)
               + struct.pack("<BBBBBBBBB", 9, 4, 0, 0, 0, 0xFF, 0, 0, 0))
STRINGS = ["farplug", "Fake Device", "TEST0001"]


def string_desc(i):
    if i == 0:
        return struct.pack("<BBH", 4, 3, 0x0409)
    s = STRINGS[i - 1].encode("utf-16-le")
    return struct.pack("<BB", 2 + len(s), 3) + s


def udev(busid):
    return (struct.pack(">256s32s", b"/fake/" + busid.encode(), busid.encode())
            + struct.pack(">IIIHHHBBBBBB", BUSNUM, DEVNUM, SPEED_FULL, 0x1209, 0x0001,
                          0x0100, 0xFF, 0, 0, 1, 1, 1))


def recv(sock, n):
    buf = b""
    while len(buf) < n:
        chunk = sock.recv(n - len(buf))
        if not chunk:
            raise EOFError
        buf += chunk
    return buf


def control(setup):
    req_type, req, value, index, length = struct.unpack("<BBHHH", setup)
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
    while True:
        cmd, seq, _devid, direction, ep = struct.unpack(">IIIII", recv(sock, 20))
        rest = recv(sock, 28)
        if cmd == 1:  # CMD_SUBMIT
            _flags, length, _sf, _npk, _intv = struct.unpack(">IiiiI", rest[:20])
            setup = rest[20:28]
            if direction == 0 and length > 0:
                recv(sock, length)
            status, data = control(setup) if ep == 0 else (-32, b"")
            if direction == 0:
                data = b""
            sock.sendall(struct.pack(">IIIIIiiiii8x", 3, seq, 0, 0, 0, status,
                                     len(data), 0, 0, 0) + data)
        elif cmd == 2:  # CMD_UNLINK
            sock.sendall(struct.pack(">IIIIIi24x", 4, seq, 0, 0, 0, 0))
        else:
            raise ValueError(f"unknown command {cmd}")


def handle(sock):
    version, code, _status = struct.unpack(">HHI", recv(sock, 8))
    assert version == VERSION, hex(version)
    if code == 0x8005:  # OP_REQ_DEVLIST
        sock.sendall(struct.pack(">HHII", VERSION, 0x0005, 0, 1) + udev(BUSID)
                     + bytes([0xFF, 0, 0, 0]))
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
