"""Require documented, crate-specific license exceptions; never allow MPL globally."""
import argparse
from pathlib import Path
import tomllib


def validate(config, policy):
    licenses = config.get("licenses", {})
    if "MPL-2.0" in licenses.get("allow", []):
        raise ValueError("MPL-2.0 must remain a crate-specific exception")
    documented = {}
    for entry in policy.get("exceptions", []):
        name = entry["crate"]
        reason = entry.get("reason", "").strip()
        if len(reason) < 20 or any(marker in reason.upper() for marker in ("TODO", "TBD")):
            raise ValueError(f"{name}: a concrete exception reason is required")
        if not entry.get("source", "").startswith("https://"):
            raise ValueError(f"{name}: source URL is required")
        if name in documented:
            raise ValueError(f"duplicate documented exception: {name}")
        documented[name] = set(entry["allow"])
    actual = {}
    for entry in licenses.get("exceptions", []):
        name = entry["crate"]
        if name in actual:
            raise ValueError(f"duplicate cargo-deny exception: {name}")
        actual[name] = set(entry["allow"])
    if actual != documented:
        raise ValueError("cargo-deny exceptions and documented reasons must match exactly")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--config", type=Path, default=Path("deny.toml"))
    parser.add_argument("--policy", type=Path, default=Path("policy/license-exceptions.toml"))
    args = parser.parse_args()
    with args.config.open("rb") as stream:
        config = tomllib.load(stream)
    with args.policy.open("rb") as stream:
        policy = tomllib.load(stream)
    validate(config, policy)
    print("License exception reasons: OK")


if __name__ == "__main__":
    main()
