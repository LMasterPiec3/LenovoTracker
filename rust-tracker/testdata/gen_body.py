#!/usr/bin/env python3
"""Erzeugt einen synthetischen Clearance-API-Body (~3,3 MB) für den Benchmark.

Deterministisch (fester Seed, eigener LCG), Schema wie die echte API
(Lenovo-API.md §5/§6), Verteilung wie §7: 9.255 Teile, davon 3.125 Boards.
Alle 14 Felder sind gefüllt, damit der Parse-Aufwand realistisch ist.

Aufruf:  python3 testdata/gen_body.py [ziel.json]

Echten Body stattdessen im CT ziehen und auf den Host kopieren:
  curl-impersonate --impersonate chrome124 --compressed -s -o body.json \\
    -H 'Accept: application/json' \\
    https://support.lenovo.com/de/de/api/v4/lbom/clearanceParts/get
  pct pull 105 /tmp/body.json rust-tracker/testdata/clearance-de.json
"""
import json
import os
import sys

# commodityId, commodityKey, Anzahl – Lenovo-API.md §7 (DE 2026-11-19)
COMMODITIES = [
    ("CK", "cover with keyboard", 4968),
    ("PL", "system boards", 3125),
    ("KI", "keyboard internal", 385),
    ("KB", "keyboards external", 248),
    ("HS", "heat sink", 187),
    ("FA", "fans", 113),
    ("LF", "lcd assembly", 69),
    ("PU", "processors", 42),
    ("CL", "wireless lan adapter", 37),
    ("CW", "wireless wan adapter", 15),
    ("", "optical drives", 13),
    ("", "ac adapter", 12),
    ("", "hard drives", 8),
    ("", "pointing device", 8),
    ("", "docking stations", 8),
    ("", "lcd panel", 5),
    ("", "drive external hdd", 4),
    ("", "cables internal", 2),
    ("", "power supplies internal", 2),
    ("", "whole unit exchange", 1),
    ("", "misc internal", 1),
    ("", "rechargeable battery internal", 1),
    ("", "reusable item reusable tape", 1),
]

CPUS = ["i3-7100U", "i5-8250U", "i5-10210U", "i7-8565U", "i7-9750H", "i7-12800H",
        "R5 5600U", "R7 2700U", "R7 5800H", "R7 6850H"]
GPUS = ["RTX3000", "T1200", "UMA", "GTX1650", "RTX A2000"]
BOARD_SUFFIX = ["NvPro YT WIN(HDCP)", "NOK YT dT2", "WIN Y-DYdT2L3Y-A", "MB UMA FP"]


class Lcg:
    """Kleiner deterministischer Generator (kein random-Modul nötig)."""

    def __init__(self, seed=42):
        self.state = seed

    def next(self):
        self.state = (self.state * 6364136223846793005 + 1442695040888963407) % (1 << 64)
        return self.state >> 33

    def below(self, n):
        return self.next() % n

    def choice(self, seq):
        return seq[self.below(len(seq))]


def part_id(idx):
    # meist 10-stellig (5B20S42095), teils kurz (01YT022) – über idx eindeutig
    if idx % 4 == 0:
        return "02D%04d" % idx
    return ("5B20S" if idx % 3 else "5B21J") + "%05d" % idx


def part_name(rng, commodity):
    if commodity == "system boards":
        cpu = rng.choice(CPUS)
        return "Lenovo BDPLANAR %s %d MB %s" % (cpu, 1 + rng.below(8), rng.choice(BOARD_SUFFIX))
    if commodity == "cover with keyboard":
        return "Lenovo cover with keyboard %s %s" % (
            rng.choice(["gris", "black", "silber"]),
            rng.choice(["5-14IKB", "T480", "E15 Gen2", "X1 Carbon", "L13"]),
        )
    return "Lenovo %s spare %d" % (commodity, 100 + rng.below(900))


def main():
    target = sys.argv[1] if len(sys.argv) > 1 else os.path.join(
        os.path.dirname(os.path.abspath(__file__)), "clearance-de.json"
    )
    rng = Lcg()
    data = []
    idx = 0
    for commodity_id, commodity_key, count in COMMODITIES:
        for _ in range(count):
            idx += 1
            stock = 1 + rng.below(30)
            price = round(33.0 + rng.below(680000) / 100.0, 2)
            products = []
            for _ in range(rng.below(5)):
                products.append("20J%d" % rng.below(10))
            data.append({
                "id": part_id(idx),
                "country": "DE",
                "language": "DE",
                "commodityId": commodity_id,
                "commodityKey": commodity_key,
                "commodityVal": commodity_key.upper(),
                "name": part_name(rng, commodity_key),
                "products": products,
                "partSaleType": "Local_Promotion" if rng.below(100) else "Local",
                "price": price,
                "promotionPrice": round(price * (0.1 + rng.below(30) / 100.0), 2),
                "stock": stock,
                "saleCount": 0,
                "updateTime": "2026-11-21T10:00:00.%03dZ" % rng.below(20),
            })
    envelope = {
        "code": 0,
        "costMillis": 197,
        "requestId": "1AC6C206A23D",
        "responseHost": "eservice-lenovo-bom-578675594c-rjbxt",
        "msg": {"desc": "Success", "value": None},
        "data": data,
    }
    body = json.dumps(envelope, separators=(",", ":"), ensure_ascii=True)
    with open(target, "w", encoding="ascii", newline="\n") as fh:
        fh.write(body)
    boards = sum(1 for p in data if p["commodityKey"] == "system boards")
    print(
        "geschrieben: %s\n  %d Bytes, %d Teile, %d Boards, Board-Stock-Summe %d"
        % (target, len(body), len(data), boards,
           sum(p["stock"] for p in data if p["commodityKey"] == "system boards"))
    )


if __name__ == "__main__":
    main()
