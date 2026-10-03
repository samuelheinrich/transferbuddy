#!/usr/bin/env python3
"""Build the shared-version workspace and produce a native .app + .dmg.

Optional TB_MAC_SIGNING_IDENTITY signs locally. TB_NOTARY_PROFILE submits to
Apple notarization using an existing Keychain profile. No publication occurs.
"""
import argparse
import json
import os
from pathlib import Path
import plistlib
import shutil
import subprocess
import sys
import tomllib

ROOT = Path(__file__).resolve().parents[1]

def run(*args, **kwargs):
    subprocess.run([str(a) for a in args], check=True, cwd=ROOT, **kwargs)

def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--target', choices=['aarch64-apple-darwin', 'x86_64-apple-darwin'])
    p.add_argument('--skip-build', action='store_true')
    p.add_argument('--packager', default='cargo-packager')
    p.add_argument('--out-dir', type=Path, help='Output directory; use a new directory while an older app is running')
    opts = p.parse_args()
    if sys.platform != 'darwin':
        p.error('macOS packaging requires macOS and Xcode Command Line Tools')
    version = tomllib.loads((ROOT / 'Cargo.toml').read_text())['workspace']['package']['version']
    output = (opts.out_dir or (ROOT / 'dist' / (opts.target or 'macos-native'))).resolve()
    output.mkdir(parents=True, exist_ok=True)
    env = os.environ.copy()
    env['MACOSX_DEPLOYMENT_TARGET'] = '13.0'
    if not opts.skip_build:
        build = ['cargo', 'build', '--locked', '--release', '--workspace']
        if opts.target:
            build += ['--target', opts.target]
        run(*build, env=env)
    # A local target directory avoids cloud-provider reads of build caches.
    binary_dir = Path(env.get('CARGO_TARGET_DIR', ROOT / 'target'))
    if not binary_dir.is_absolute():
        binary_dir = ROOT / binary_dir
    if opts.target:
        binary_dir /= opts.target
    binary_dir /= 'release'
    for name in ['transferbuddy-desktop', 'transferbuddy', 'transferbuddy-port-helper']:
        if not (binary_dir / name).is_file():
            raise SystemExit(f'Missing binary: {binary_dir / name}')
    cfg = json.loads((ROOT / 'packaging/packager.json').read_text())
    cfg.update(version=version, outDir=str(output), binariesDir=str(binary_dir))
    cfg['icons'] = [str(ROOT / icon) for icon in cfg.get('icons', [])]
    cfg['macos']['infoPlistPath'] = str(ROOT / cfg['macos']['infoPlistPath'])
    for resource in cfg['resources']:
        resource['src'] = str(ROOT / resource['src'])
    if opts.target:
        cfg['targetTriple'] = opts.target
    config = output / 'packager.json'
    config.write_text(json.dumps(cfg, indent=2))
    run(opts.packager, '--config', config, '--formats', 'app')
    bundle = output / 'TransferBuddy.app'
    contents = bundle / 'Contents'
    # Keep the daemon in the SMAppService layout regardless of packager resource handling.
    daemon = contents / 'Library' / 'LaunchDaemons'
    daemon.mkdir(parents=True, exist_ok=True)
    shutil.copy2(ROOT / 'packaging/macos/com.transferbuddy.port-helper.plist', daemon)
    info = plistlib.loads((contents / 'Info.plist').read_bytes())
    assert info['CFBundleIdentifier'] == 'com.transferbuddy.desktop'
    assert info['CFBundleShortVersionString'] == version
    assert info['CFBundleExecutable'] == 'transferbuddy-desktop'
    assert info['LSMinimumSystemVersion'] == '13.0'
    assert info.get('NSLocalNetworkUsageDescription')
    identity = env.get('TB_MAC_SIGNING_IDENTITY')
    if identity:
        run('codesign', '--force', '--options', 'runtime', '--timestamp', '--identifier',
            'com.transferbuddy.port-helper', '--sign', identity, contents / 'MacOS/transferbuddy-port-helper')
        run('codesign', '--force', '--options', 'runtime', '--timestamp', '--identifier',
            'com.transferbuddy.cli', '--sign', identity,
            contents / 'MacOS/transferbuddy')
        run('codesign', '--force', '--options', 'runtime', '--timestamp', '--identifier',
            'com.transferbuddy.desktop', '--sign', identity, bundle)
        run('codesign', '--verify', '--deep', '--strict', bundle)
    else:
        # Bind bundle metadata/resources even for local builds. Linker-only
        # signatures do not bind Info.plist to the application identity.
        for name, identifier in [('transferbuddy-port-helper', 'com.transferbuddy.port-helper'),
                                 ('transferbuddy', 'com.transferbuddy.cli')]:
            run('codesign', '--force', '--sign', '-', '--timestamp=none', '--identifier',
                identifier, contents / 'MacOS' / name)
        run('codesign', '--force', '--sign', '-', '--timestamp=none', '--identifier',
            'com.transferbuddy.desktop', bundle)
        run('codesign', '--verify', '--deep', '--strict', bundle)
    staging = output / 'dmg-stage'
    if staging.exists():
        shutil.rmtree(staging)
    staging.mkdir()
    run('ditto', bundle, staging / bundle.name)
    (staging / 'Applications').symlink_to('/Applications')
    dmg = output / f'TransferBuddy-{version}-{opts.target or "native"}.dmg'
    if dmg.exists():
        dmg.unlink()
    run('hdiutil', 'create', '-volname', f'TransferBuddy {version}', '-srcfolder', staging,
        '-ov', '-format', 'UDZO', dmg)
    profile = env.get('TB_NOTARY_PROFILE')
    if profile:
        if not identity:
            raise SystemExit('Notarization requires TB_MAC_SIGNING_IDENTITY')
        run('xcrun', 'notarytool', 'submit', dmg, '--keychain-profile', profile, '--wait')
        run('xcrun', 'stapler', 'staple', dmg)
        run('xcrun', 'stapler', 'validate', dmg)
    shutil.rmtree(staging)
    print(f'Created {bundle}\nCreated {dmg}')

if __name__ == '__main__':
    main()
