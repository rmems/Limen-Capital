#!/usr/bin/env python3
"""Independent little-endian oracle. Check by default; --write regenerates."""
import argparse
from pathlib import Path
import struct

ROOT = Path(__file__).resolve().parent
# Canonical placeholder universe, FNV-1a of newline-joined symbols.
HASH = 0x72B15F3EEE1D443E
TIMESTAMP = 1_700_000_000_000_000_001
ASSETS = [x for i in range(7) for x in (1 + i / 10, (i + 1) / 10)]
TRAILING = [0.9, 0.0001, 1_000_000, 0.05, 0.1, 45, 200, 0.5, 0.1, 0.01, 0.0002]
LEGACY_READOUT = [0.8, 0.1, 0.2, 0.3] + [0] * 12
READOUT = [0.8, 0.1, 0.2, 0.3] + [i / 100 for i in range(4, 16)]
RELEVANCE = [0.4, 0.3, 0.2, 0.1]


def fixtures():
    market = struct.pack("<25f", *(ASSETS + TRAILING))
    return {
        "marketpulse.bin": struct.pack("<Q", TIMESTAMP) + market + bytes(12),
        "readout.bin": struct.pack("<q20f", 42, *(LEGACY_READOUT + RELEVANCE)),
        "marketpulse_v2.bin": struct.pack("<4sBBHQQHH", b"LNRV", 2, 1, 0,
                                        TIMESTAMP, HASH, 7, 0) + market,
        "readout_v2.bin": struct.pack("<4sBBHqQHH20f", b"LNRV", 2, 2, 0,
                                    42, HASH, 8, 0, *(READOUT + RELEVANCE)),
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--write", action="store_true")
    args = parser.parse_args()
    for name, expected in fixtures().items():
        path = ROOT / "fixtures" / name
        if args.write:
            path.write_bytes(expected)
        elif path.read_bytes() != expected:
            raise SystemExit(f"{name} differs from the independent wire oracle")
        print(f"{name}: {len(expected)} bytes OK")


if __name__ == "__main__":
    main()
