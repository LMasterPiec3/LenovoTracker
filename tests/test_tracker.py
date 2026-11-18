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


def make_cfg(state_dir=None, **overrides):
    if state_dir is None:
        state_dir = tempfile.mkdtemp()
    defaults = dict(state_dir=state_dir)
    defaults.update(overrides)
    return lt.Config(
        api_url="https://support.lenovo.com/de/de/api/v4/lbom/clearanceParts/get",
        referer=lt.PAGE_URL,
        mail_from="Lenovo@aomme.de",
        mail_to=["Lenovo@aomme.de"],
        **defaults,
    )


class CapturingDeliver:
    def __init__(self):
        self.messages = []

    def __call__(self, cfg, msg):
        self.messages.append(msg)


def fixture_payload(mutate=None):
    payload = copy.deepcopy(FIXTURE)
    if mutate:
        mutate(payload["data"])
    return payload


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


class TestState(unittest.TestCase):
    def test_load_state_missing_returns_none(self):
        with tempfile.TemporaryDirectory() as td:
            self.assertIsNone(lt.load_state(Path(td) / "state.json"))

    def test_save_and_load_roundtrip(self):
        with tempfile.TemporaryDirectory() as td:
            path = Path(td) / "state.json"
            lt.save_state(path, "2026-11-18T20:00:00+00:00", 100, 3, {"A": {"s": 5, "n": "A"}})
            state = lt.load_state(path)
            self.assertEqual(state["total_stock"], 100)
            self.assertEqual(state["part_count"], 3)
            self.assertEqual(state["parts"]["A"]["s"], 5)

    def test_load_state_corrupt_returns_none(self):
        with tempfile.TemporaryDirectory() as td:
            path = Path(td) / "state.json"
            path.write_text("{kaputt", encoding="utf-8")
            self.assertIsNone(lt.load_state(path))

    def test_append_history_writes_header_once(self):
        with tempfile.TemporaryDirectory() as td:
            path = Path(td) / "history.csv"
            lt.append_history(path, "2026-11-18T20:00:00+00:00", 100, 3)
            lt.append_history(path, "2026-11-18T20:30:00+00:00", 101, 3)
            lines = path.read_text(encoding="utf-8").strip().splitlines()
            self.assertEqual(lines[0], "timestamp,total_stock,part_count")
            self.assertEqual(lines[1], "2026-11-18T20:00:00+00:00,100,3")
            self.assertEqual(lines[2], "2026-11-18T20:30:00+00:00,101,3")


DIFF = lt.build_diff(
    {"01YT022": {"s": 2, "n": "i3-7100U System Board"},
     "5A10W86303": {"s": 97, "n": "Mainboard <Top> & Board"}},
    {"01YT022": {"s": 30, "n": "i3-7100U System Board"},
     "5A10W86303": {"s": 112, "n": "Mainboard <Top> & Board"},
     "NEU0001": {"s": 5, "n": "Neues Board"}},
)  # alt 99 → neu 147, delta 48; increased: 01YT022 (+28), 5A10W86303 (+15); new_parts: NEU0001


class TestMail(unittest.TestCase):
    def setUp(self):
        self.cfg = make_cfg()

    def test_fmt_de(self):
        self.assertEqual(lt.fmt_de(16489), "16.489")
        self.assertEqual(lt.fmt_de(999), "999")

    def test_subject_contains_delta(self):
        subject = lt.build_subject(self.cfg, DIFF)
        self.assertTrue(subject.startswith("[Lenovo Tracker]"))
        self.assertIn("99 → 147 (+48)", subject)

    def test_mail_text_lists_top_increase(self):
        text = lt.build_mail_text(DIFF, lt.PAGE_URL, "2026-11-18T20:00:00+00:00")
        self.assertIn("+28", text)
        self.assertIn("01YT022", text)
        self.assertIn("Neu im Sale: 1", text)

    def test_mail_html_escapes_part_names(self):
        html = lt.build_mail_html(DIFF, lt.PAGE_URL, "2026-11-18T20:00:00+00:00")
        self.assertIn("Mainboard &lt;Top&gt; &amp; Board", html)  # Teil ist in der increased-Liste
        self.assertNotIn("<Top>", html)

    def test_compose_mail_headers_and_bodies(self):
        msg = lt.compose_mail(self.cfg, "Betreff", "TEXT", "<html>HTML</html>")
        self.assertEqual(msg["From"], "Lenovo@aomme.de")
        self.assertEqual(msg["To"], "Lenovo@aomme.de")
        self.assertEqual(msg["Subject"], "Betreff")
        raw = msg.as_bytes().decode("utf-8")
        self.assertIn("TEXT", raw)
        self.assertIn("<html>HTML</html>", raw)

    def test_compose_mail_with_extra_recipients(self):
        cfg = make_cfg(extra_recipients=["chef@example.com"])
        msg = lt.compose_mail(cfg, "B", "T", "<p>H</p>")
        self.assertEqual(msg["To"], "Lenovo@aomme.de, chef@example.com")


class TestRunOnce(unittest.TestCase):
    def test_run_once_baseline_no_mail(self):
        cfg = make_cfg()
        deliver = CapturingDeliver()
        session = FakeSession([FakeResponse(200, fixture_payload())])
        result = lt.run_once(cfg, session=session, deliver=deliver)
        self.assertTrue(result.baseline)
        self.assertFalse(result.sent_mail)
        self.assertEqual(result.total, 100)
        self.assertEqual(deliver.messages, [])
        state = lt.load_state(Path(cfg.state_dir) / "state.json")
        self.assertEqual(state["total_stock"], 100)

    def test_run_once_increase_sends_mail(self):
        cfg = make_cfg()
        deliver = CapturingDeliver()
        # Lauf 1: Baseline
        lt.run_once(cfg, session=FakeSession([FakeResponse(200, fixture_payload())]), deliver=deliver)
        # Lauf 2: Bestand steigt +15, neues Teil +3 → +18 gesamt
        def bump(data):
            for part in data:
                if part["id"] == "5A10W86303":
                    part["stock"] = 112
            data.append({"id": "NEU0001", "name": "Neues Board", "commodityKey": "system boards", "stock": 3})
        result = lt.run_once(
            cfg,
            session=FakeSession([FakeResponse(200, fixture_payload(bump))]),
            deliver=deliver,
        )
        self.assertTrue(result.sent_mail)
        self.assertEqual(result.total, 118)
        self.assertEqual(len(deliver.messages), 1)
        subject = deliver.messages[0]["Subject"]
        self.assertIn("100 → 118 (+18)", subject)

    def test_run_once_decrease_no_mail(self):
        cfg = make_cfg()
        deliver = CapturingDeliver()
        lt.run_once(cfg, session=FakeSession([FakeResponse(200, fixture_payload())]), deliver=deliver)
        def drop(data):
            for part in data:
                if part["id"] == "5A10W86303":
                    part["stock"] = 10
        result = lt.run_once(cfg, session=FakeSession([FakeResponse(200, fixture_payload(drop))]), deliver=deliver)
        self.assertFalse(result.sent_mail)
        self.assertEqual(result.total, 13)  # 2 + 1 + 10 + 0 (Teil ohne stock-Feld)
        self.assertEqual(deliver.messages, [])

    def test_run_once_fetch_error_keeps_state(self):
        cfg = make_cfg()
        deliver = CapturingDeliver()
        lt.run_once(cfg, session=FakeSession([FakeResponse(200, fixture_payload())]), deliver=deliver)
        before = lt.load_state(Path(cfg.state_dir) / "state.json")
        result = lt.run_once(cfg, session=FakeSession([OSError("boom")]), deliver=deliver, sleep=lambda s: None)
        self.assertIsNotNone(result.error)
        self.assertEqual(lt.load_state(Path(cfg.state_dir) / "state.json"), before)
        self.assertEqual(deliver.messages, [])

    def test_run_once_no_mainboards_zero_sum_no_mail(self):
        cfg = make_cfg()
        deliver = CapturingDeliver()
        lt.run_once(cfg, session=FakeSession([FakeResponse(200, fixture_payload())]), deliver=deliver)
        def no_boards(data):
            data[:] = [p for p in data if p.get("commodityKey") != "system boards"]
        result = lt.run_once(cfg, session=FakeSession([FakeResponse(200, fixture_payload(no_boards))]), deliver=deliver)
        self.assertEqual(result.total, 0)
        self.assertFalse(result.sent_mail)
        self.assertEqual(deliver.messages, [])
        self.assertEqual(lt.load_state(Path(cfg.state_dir) / "state.json")["total_stock"], 0)

    def test_run_once_dry_run_touches_nothing(self):
        cfg = make_cfg()
        deliver = CapturingDeliver()
        result = lt.run_once(cfg, session=FakeSession([FakeResponse(200, fixture_payload())]), deliver=deliver, dry_run=True)
        self.assertEqual(result.total, 100)
        self.assertFalse((Path(cfg.state_dir) / "state.json").exists())
        self.assertFalse((Path(cfg.state_dir) / "history.csv").exists())


if __name__ == "__main__":
    unittest.main()
