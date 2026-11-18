import json
import sys
import tempfile
import unittest
from pathlib import Path

REPO = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(REPO))

import lenovo_tracker as lt  # noqa: E402

VALID_TOML = """\
[api]
url = "https://support.lenovo.com/de/de/api/v4/lbom/clearanceParts/get"
referer = "https://support.lenovo.com/de/de/parts-lookup/clearance-sale"
timeout_seconds = 60
retries = 3

[tracker]
interval_minutes = 30
commodity_keys = ["system boards"]

[mail]
mode = "sendmail"
sendmail_path = "/usr/sbin/sendmail"
from = "Lenovo@aomme.de"
to = ["Lenovo@aomme.de"]
extra_recipients = ["other@example.com"]
subject_prefix = "[Lenovo Tracker]"

[paths]
state_dir = "/var/lib/lenovotracker"
"""


def write_config(tmpdir, content):
    p = Path(tmpdir) / "config.toml"
    p.write_text(content, encoding="utf-8")
    return str(p)


class TestLoadConfig(unittest.TestCase):
    def test_load_valid_config(self):
        with tempfile.TemporaryDirectory() as td:
            cfg = lt.load_config(write_config(td, VALID_TOML))
            self.assertEqual(cfg.api_url, "https://support.lenovo.com/de/de/api/v4/lbom/clearanceParts/get")
            self.assertEqual(cfg.mail_from, "Lenovo@aomme.de")
            self.assertEqual(cfg.mail_to, ["Lenovo@aomme.de"])
            self.assertEqual(cfg.extra_recipients, ["other@example.com"])
            self.assertEqual(cfg.commodity_keys, ["system boards"])  # lowercase
            self.assertEqual(cfg.interval_minutes, 30)
            self.assertEqual(cfg.mail_mode, "sendmail")
            self.assertEqual(cfg.state_dir, "/var/lib/lenovotracker")

    def test_missing_file_raises(self):
        with self.assertRaises(lt.ConfigError):
            lt.load_config("/nicht/existiert/config.toml")

    def test_missing_from_raises(self):
        toml = VALID_TOML.replace('from = "Lenovo@aomme.de"\n', "")
        with tempfile.TemporaryDirectory() as td:
            with self.assertRaises(lt.ConfigError):
                lt.load_config(write_config(td, toml))

    def test_invalid_mode_raises(self):
        toml = VALID_TOML.replace('mode = "sendmail"', 'mode = "pigeon"')
        with tempfile.TemporaryDirectory() as td:
            with self.assertRaises(lt.ConfigError):
                lt.load_config(write_config(td, toml))

    def test_smtp_without_host_raises(self):
        toml = VALID_TOML.replace('mode = "sendmail"', 'mode = "smtp"')
        with tempfile.TemporaryDirectory() as td:
            with self.assertRaises(lt.ConfigError):
                lt.load_config(write_config(td, toml))

    def test_load_config_accepts_string_recipients(self):
        toml = VALID_TOML.replace('to = ["Lenovo@aomme.de"]', 'to = "Lenovo@aomme.de"')
        with tempfile.TemporaryDirectory() as td:
            cfg = lt.load_config(write_config(td, toml))
            self.assertEqual(cfg.mail_to, ["Lenovo@aomme.de"])


class FakeResponse:
    def __init__(self, status_code=200, payload=None, raw=b"not json"):
        self.status_code = status_code
        self._payload = payload
        self._raw = raw

    def json(self):
        if self._payload is None:
            raise ValueError("kein JSON")
        return self._payload


class FakeSession:
    def __init__(self, responses):
        self.responses = list(responses)  # Liste von FakeResponse oder Exception-Instanzen
        self.calls = []

    def get(self, url, headers=None, timeout=None):
        self.calls.append({"url": url, "headers": headers, "timeout": timeout})
        item = self.responses.pop(0)
        if isinstance(item, Exception):
            raise item
        return item


def make_cfg(**overrides):
    return lt.Config(
        api_url="https://support.lenovo.com/de/de/api/v4/lbom/clearanceParts/get",
        referer=lt.PAGE_URL,
        mail_from="Lenovo@aomme.de",
        mail_to=["Lenovo@aomme.de"],
        **overrides,
    )


GOOD_PAYLOAD = {"code": 0, "data": [{"id": "01YT022", "commodityKey": "system boards", "stock": 2}]}


class TestFetch(unittest.TestCase):
    def test_fetch_ok(self):
        cfg = make_cfg()
        session = FakeSession([FakeResponse(200, GOOD_PAYLOAD)])
        parts = lt.fetch_clearance_parts(cfg, session=session)
        self.assertEqual(len(parts), 1)
        self.assertEqual(session.calls[0]["headers"]["Accept"], "application/json, text/plain, */*")
        self.assertEqual(session.calls[0]["headers"]["Referer"], lt.PAGE_URL)

    def test_fetch_403_no_retry(self):
        cfg = make_cfg(retries=3)
        session = FakeSession([FakeResponse(403)])
        with self.assertRaises(lt.FetchError) as ctx:
            lt.fetch_clearance_parts(cfg, session=session, sleep=lambda s: None)
        self.assertEqual(ctx.exception.status, 403)
        self.assertEqual(len(session.calls), 1)  # kein Retry bei Bot-Block

    def test_fetch_retries_then_raises(self):
        cfg = make_cfg(retries=2)
        sleeps = []
        session = FakeSession([OSError("net down"), OSError("net down"), OSError("net down")])
        with self.assertRaises(lt.FetchError):
            lt.fetch_clearance_parts(cfg, session=session, sleep=sleeps.append)
        self.assertEqual(len(session.calls), 3)
        self.assertEqual(sleeps, [5, 15])  # BACKOFFS

    def test_fetch_rejects_bad_code(self):
        cfg = make_cfg(retries=0)
        session = FakeSession([FakeResponse(200, {"code": 104, "msg": "err"})])
        with self.assertRaises(lt.FetchError):
            lt.fetch_clearance_parts(cfg, session=session)

    def test_fetch_rejects_empty_data(self):
        cfg = make_cfg(retries=0)
        session = FakeSession([FakeResponse(200, {"code": 0, "data": []})])
        with self.assertRaises(lt.FetchError):
            lt.fetch_clearance_parts(cfg, session=session)

    def test_fetch_rejects_invalid_json(self):
        cfg = make_cfg(retries=0)
        session = FakeSession([FakeResponse(200, None, raw=b"<html>blocked</html>")])
        with self.assertRaises(lt.FetchError):
            lt.fetch_clearance_parts(cfg, session=session)


import copy

FIXTURE = json.loads((REPO / "tests" / "fixture_clearance.json").read_text(encoding="utf-8"))


class TestFilterDiff(unittest.TestCase):
    def test_filter_mainboards_case_insensitive(self):
        boards = lt.filter_mainboards(FIXTURE["data"], ["system boards"])
        self.assertEqual(len(boards), 4)
        self.assertEqual({b["id"] for b in boards}, {"01YT022", "00FC857", "5A10W86303", "03NOSTOCK"})

    def test_compute_totals(self):
        boards = lt.filter_mainboards(FIXTURE["data"], ["system boards"])
        self.assertEqual(lt.compute_totals(boards), (100, 4))

    def test_stock_of_handles_missing_and_garbage(self):
        self.assertEqual(lt._stock_of({}), 0)
        self.assertEqual(lt._stock_of({"stock": None}), 0)
        self.assertEqual(lt._stock_of({"stock": "7"}), 7)
        self.assertEqual(lt._stock_of({"stock": -5}), 0)
        self.assertEqual(lt._stock_of({"stock": "kaputt"}), 0)

    def test_build_diff_increase_sorted(self):
        old_map = {"A": {"s": 5, "n": "A"}, "B": {"s": 90, "n": "B"}}
        new_map = {"A": {"s": 25, "n": "A"}, "B": {"s": 95, "n": "B"}, "C": {"s": 3, "n": "C"}}
        diff = lt.build_diff(old_map, new_map)
        self.assertEqual(diff.delta, 28)  # 123 - 95
        self.assertEqual([d["id"] for d in diff.increased], ["A", "B"])  # delta 20 vor 5
        self.assertEqual(diff.increased[0]["delta"], 20)
        self.assertEqual([p["id"] for p in diff.new_parts], ["C"])

    def test_build_diff_decrease_and_equal(self):
        old_map = {"A": {"s": 5, "n": "A"}}
        self.assertEqual(lt.build_diff(old_map, {"A": {"s": 2, "n": "A"}}).delta, -3)
        self.assertEqual(lt.build_diff(old_map, {"A": {"s": 5, "n": "A"}}).delta, 0)
        self.assertEqual(lt.build_diff(old_map, {"A": {"s": 2, "n": "A"}}).increased, [])


if __name__ == "__main__":
    unittest.main()
