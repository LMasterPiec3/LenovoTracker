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


if __name__ == "__main__":
    unittest.main()
