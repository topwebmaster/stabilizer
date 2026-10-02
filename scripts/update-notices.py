#!/usr/bin/env python3
"""Collect original license/notice files for locked Rust dependencies."""
import json
import pathlib
import subprocess

ROOT = pathlib.Path(__file__).resolve().parent.parent


def main():
    metadata = json.loads(subprocess.check_output(['cargo', 'metadata', '--locked', '--format-version=1'], cwd=ROOT, text=True))
    packages = sorted((p for p in metadata['packages'] if p['name'] != 'stabilizer'), key=lambda p: (p['name'], p['version']))
    parts = ['# Third-party notices\n\nStabilizer original code is under the no-resale license in LICENSE. Dependencies retain their own licenses and rights. This file is generated from the locked source dependencies; it also includes build-time crates. Native GTK, libadwaita and GLib are linked dynamically from Ubuntu system packages, under their respective LGPL licenses.\n',
             '| Crate | Version | License |\n|---|---|---|\n']
    for package in packages:
        parts.append(f"| {package['name']} | {package['version']} | {package.get('license') or 'See upstream license files'} |\n")
    for package in packages:
        directory = pathlib.Path(package['manifest_path']).parent
        files = set()
        for pattern in ('LICENSE*', 'LICENCE*', 'COPYING*', 'NOTICE*'):
            files.update(p for p in directory.glob(pattern) if p.is_file())
        if package.get('license_file'):
            files.add(directory / package['license_file'])
        parts.append(f"\n## {package['name']} {package['version']}\n\n")
        for file in sorted(files):
            if file.exists():
                parts.append(f"### {file.name}\n\n```text\n{file.read_text(errors='replace').rstrip()}\n```\n")
        if not files:
            parts.append(f"License declaration: {package.get('license')}. Upstream: {package.get('repository') or package.get('homepage') or 'https://crates.io/crates/' + package['name']}.\n")
    for name in ('libgtk-4-1', 'libadwaita-1-0', 'libglib2.0-0t64'):
        path = pathlib.Path('/usr/share/doc') / name / 'copyright'
        if path.exists():
            parts.append(f'\n## Ubuntu native library: {name}\n\n```text\n{path.read_text(errors="replace").rstrip()}\n```\n')
    for name in ('LGPL-2.1', 'GPL-2', 'Apache-2.0'):
        path = pathlib.Path('/usr/share/common-licenses') / name
        if path.exists():
            parts.append(f'\n## Common license referenced above: {name}\n\n```text\n{path.read_text().rstrip()}\n```\n')
    (ROOT / 'THIRD_PARTY_NOTICES.md').write_text(''.join(parts))
    print(f'Collected notices for {len(packages)} locked dependencies')


if __name__ == '__main__':
    main()
