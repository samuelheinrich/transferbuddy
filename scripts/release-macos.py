#!/usr/bin/env python3
"""Build macOS desktop DMGs and standalone CLI archives, without publishing.

Requires macOS, Python 3.11+, Xcode Command Line Tools, Rust targets and
cargo-packager 0.11.8. Signing/notarization options pass through to the packager.
"""
import argparse
import hashlib
import os
from pathlib import Path
import platform
import shutil
import subprocess
import tarfile
import tempfile
import tomllib

ROOT = Path(__file__).resolve().parents[1]
TARGETS = {"aarch64-apple-darwin": "arm64", "x86_64-apple-darwin": "x86_64"}


def run(*args, **kwargs):
    return subprocess.run([str(a) for a in args], cwd=ROOT, check=True, **kwargs)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--target", action="append", choices=TARGETS)
    parser.add_argument("--packager", default="cargo-packager")
    parser.add_argument("--skip-build", action="store_true")
    parser.add_argument("--out-dir", type=Path)
    options = parser.parse_args()
    if platform.system() != "Darwin":
        parser.error("macOS releases require a macOS host")
    version = tomllib.loads((ROOT / "Cargo.toml").read_text())["workspace"]["package"]["version"]
    output = (options.out_dir or ROOT / "dist" / f"release-{version}").resolve()
    output.mkdir(parents=True, exist_ok=True)
    host = platform.machine()
    for target in options.target or TARGETS:
        native = TARGETS[target] == host
        package_dir = output / target
        command = ["python3", ROOT / "scripts/package-macos.py", "--packager",
                   options.packager, "--out-dir", package_dir]
        if not native:
            command += ["--target", target]
        if options.skip_build:
            command.append("--skip-build")
        run(*command)
        bundle = package_dir / "TransferBuddy.app"
        binaries = bundle / "Contents/MacOS"
        for name in ("transferbuddy", "transferbuddy-desktop", "transferbuddy-port-helper"):
            archs = run("lipo", "-archs", binaries / name, capture_output=True, text=True).stdout.strip()
            if archs != TARGETS[target]:
                raise SystemExit(f"Wrong architecture in {name}: {archs}, expected {TARGETS[target]}")
        for name in ("transferbuddy", "transferbuddy-desktop"):
            reported = run(binaries / name, "--version", capture_output=True, text=True).stdout.strip()
            if reported.split()[-1] != version:
                raise SystemExit(f"Wrong version: {reported}, expected {version}")
        run("codesign", "--verify", "--deep", "--strict", bundle)
        original_dmg = package_dir / f'TransferBuddy-{version}-{"native" if native else target}.dmg'
        dmg = output / f"TransferBuddy-{version}-{target}.dmg"
        shutil.move(original_dmg, dmg)
        run("hdiutil", "verify", dmg)
        with tempfile.TemporaryDirectory(prefix="transferbuddy-cli-") as temporary:
            stage = Path(temporary)
            shutil.copy2(binaries / "transferbuddy", stage / "transferbuddy")
            shutil.copy2(ROOT / "LICENSE", stage / "LICENSE")
            (stage / "README.txt").write_text(
                f"TransferBuddy {version} CLI/TUI for {TARGETS[target]} macOS 13+\n\n"
                "Install: sudo install -m 755 transferbuddy /usr/local/bin/transferbuddy\n"
                "Check: transferbuddy --version\n"
                "Run: cd /path/to/files && transferbuddy\n"
                "Help: transferbuddy --help\n"
                "Documentation: https://github.com/samuelheinrich/transferbuddy\n"
                "This build is ad-hoc signed unless a Developer ID identity was supplied.\n"
                "If macOS blocks a downloaded binary, use Privacy & Security > Open Anyway\n"
                "after verifying its source and the SHA256SUMS.txt checksum.\n"
            )
            archive = output / f"transferbuddy-{version}-{target}.tar.gz"
            with tarfile.open(archive, "w:gz") as tar:
                for name in ("transferbuddy", "LICENSE", "README.txt"):
                    tar.add(stage / name, arcname=name)
        print(f"Created {dmg}\nCreated {archive}", flush=True)
    # Include artifacts from earlier invocations when building one target at a time.
    assets = sorted([*output.glob(f"TransferBuddy-{version}-*.dmg"),
                     *output.glob(f"transferbuddy-{version}-*.tar.gz")])
    checksums = []
    for asset in assets:
        with asset.open("rb") as stream:
            digest = hashlib.file_digest(stream, "sha256").hexdigest()
        checksums.append(f"{digest}  {asset.name}\n")
    (output / "SHA256SUMS.txt").write_text("".join(checksums))
    print(f"Release artifacts and checksums: {output}", flush=True)


if __name__ == "__main__":
    main()
