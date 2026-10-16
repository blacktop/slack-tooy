#!/usr/bin/env python3
"""Exercise the real decoder and its Seatbelt policy without private data."""

import argparse
import errno
import pathlib
import socket
import struct
import subprocess
import tempfile
import zlib

ROOT = pathlib.Path(__file__).resolve().parent.parent
PROFILE = ROOT / "macos/LocalDecoder/decoder.sb"


def run(program, arguments=(), data=b"", sandboxed=True, pass_fds=()):
    program = program.resolve(strict=True)
    command = [str(program), *arguments]
    if sandboxed:
        command = [str(program), "--sandbox", PROFILE.read_text(), *arguments]
    return subprocess.run(command, input=data, capture_output=True, timeout=20, env={},
                          pass_fds=pass_fds)


def png_chunk(name, data):
    return struct.pack(">I", len(data)) + name + data + struct.pack(">I", zlib.crc32(name + data))


def check(condition, message):
    if not condition:
        raise RuntimeError(message)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("decoder", type=pathlib.Path)
    parser.add_argument("probe", type=pathlib.Path)
    arguments = parser.parse_args()

    png = b"\x89PNG\r\n\x1a\n" + png_chunk(b"IHDR", struct.pack(">IIBBBBB", 1, 1, 8, 6, 0, 0, 0))
    png += png_chunk(b"IDAT", zlib.compress(b"\0\xff\0\0\x80")) + png_chunk(b"IEND", b"")
    result = run(arguments.decoder, data=struct.pack("<I", len(png)) + png)
    expected = struct.pack("<4sIII", b"STIM", 1, 1, 4) + bytes((255, 0, 0, 128))
    check(result.returncode == 0 and result.stdout == expected,
          f"Sandboxed image round trip failed (exit {result.returncode})")
    for frame in (b"", struct.pack("<I", 0), struct.pack("<I", 16 * 1024 * 1024 + 1),
                  struct.pack("<I", 4) + b"bad", struct.pack("<I", 4) + b"nope"):
        rejected = run(arguments.decoder, data=frame)
        check(rejected.returncode != 0 and not rejected.stdout, "Decoder accepted an invalid frame")

    with tempfile.TemporaryDirectory(prefix="slack-tooy-sandbox-check-") as directory:
        sentinel = pathlib.Path(directory) / "outside-allowlist.txt"
        sentinel.write_bytes(b"owned test sentinel\n")
        with socket.socket() as listener:
            listener.bind(("127.0.0.1", 0))
            listener.listen(2)
            cases = (("read", str(sentinel)), ("connect", str(listener.getsockname()[1])))
            for operation in cases:
                allowed = run(arguments.probe, operation, sandboxed=False)
                check(allowed.returncode == 0 and allowed.stdout == b"permitted\n",
                      f"{operation[0]} positive control failed")
                denied = run(arguments.probe, operation)
                accepted = {f"denied {errno.EPERM}\n".encode(), f"denied {errno.EACCES}\n".encode()}
                check(denied.returncode == 1 and denied.stdout in accepted,
                      f"Sandbox did not deny {operation[0]}")
        with sentinel.open("rb") as inherited:
            operation = ("fd", str(inherited.fileno()))
            allowed = run(arguments.probe, operation, sandboxed=False,
                          pass_fds=(inherited.fileno(),))
            check(allowed.returncode == 0 and allowed.stdout == b"permitted\n",
                  "Inherited descriptor positive control failed")
            inherited.seek(0)
            denied = run(arguments.probe, operation, pass_fds=(inherited.fileno(),))
            check(denied.returncode == 1 and denied.stdout == f"denied {errno.EBADF}\n".encode(),
                  "Sandbox launcher retained an inherited descriptor")
    print("Image decoder: pixels, malformed frames, file/network denial, and descriptor closure passed")


if __name__ == "__main__":
    main()
