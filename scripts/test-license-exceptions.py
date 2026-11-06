"""Negative checks for policy changes that could silently widen the license gate."""
import copy
import importlib.util
from pathlib import Path
import unittest

spec = importlib.util.spec_from_file_location("license_policy", Path(__file__).with_name("check-license-exceptions.py"))
policy_check = importlib.util.module_from_spec(spec)
spec.loader.exec_module(policy_check)


class LicenseExceptionsTests(unittest.TestCase):
    def setUp(self):
        self.config = {"licenses": {"allow": ["MIT"], "exceptions": [{"crate": "serialport", "allow": ["MPL-2.0"]}]}}
        self.policy = {"exceptions": [{"crate": "serialport", "allow": ["MPL-2.0"],
                                     "reason": "Serial support with upstream notices and source available.",
                                     "source": "https://crates.io/crates/serialport"}]}

    def test_documented_exception_is_accepted(self):
        policy_check.validate(self.config, self.policy)

    def test_missing_or_placeholder_reason_is_rejected(self):
        for reason in ("", "N/A", "TODO: explain this exception later"):
            with self.subTest(reason=reason), self.assertRaises(ValueError):
                policy = copy.deepcopy(self.policy)
                policy["exceptions"][0]["reason"] = reason
                policy_check.validate(self.config, policy)

    def test_extra_exception_or_changed_license_is_rejected(self):
        config = copy.deepcopy(self.config)
        config["licenses"]["exceptions"].append({"crate": "surprise", "allow": ["GPL-3.0-only"]})
        with self.assertRaises(ValueError):
            policy_check.validate(config, self.policy)
        config = copy.deepcopy(self.config)
        config["licenses"]["exceptions"][0]["allow"] = ["GPL-3.0-only"]
        with self.assertRaises(ValueError):
            policy_check.validate(config, self.policy)

    def test_global_mpl_allow_cannot_bypass_exception_review(self):
        self.config["licenses"]["allow"].append("MPL-2.0")
        with self.assertRaises(ValueError):
            policy_check.validate(self.config, self.policy)


if __name__ == "__main__":
    unittest.main()
