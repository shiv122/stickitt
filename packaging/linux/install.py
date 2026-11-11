#!/usr/bin/env python3
"""Install this local bundle for the current user, or into an explicit staging prefix."""

import argparse
from pathlib import Path
import shutil
import subprocess
import tempfile


def desktop_exec(path):
    # Desktop Entry quoting is different from shell quoting; '%' starts a field code.
    escaped = str(path).replace('%', '%%')
    for char in ('\\', '"', '`', '$'):
        escaped = escaped.replace(char, '\\' + char)
    return ('"' + escaped + '"').replace('\\', '\\\\')


def install(bundle, prefix):
    prefix = prefix.expanduser().resolve()
    if '\n' in str(prefix) or '\r' in str(prefix):
        raise ValueError('A desktop installation path cannot contain a newline')
    executable = prefix / 'bin/stickit'
    desktop = prefix / 'share/applications/dev.stickitt.Stickitt.desktop'
    executable.parent.mkdir(parents=True, exist_ok=True)
    desktop.parent.mkdir(parents=True, exist_ok=True)
    # Replace atomically so an already-running copy can keep using its executable.
    with tempfile.NamedTemporaryFile(dir=executable.parent, prefix='.stickit-', delete=False) as staged:
        temporary = Path(staged.name)
    try:
        shutil.copyfile(bundle / 'stickit', temporary)
        temporary.chmod(0o755)
        temporary.replace(executable)
    finally:
        temporary.unlink(missing_ok=True)
    text = (bundle / 'dev.stickitt.Stickitt.desktop').read_text()
    # Exec is absolute so menu launches also work when ~/.local/bin isn't in PATH.
    text = text.replace('Exec=stickit\n', f'Exec=/usr/bin/env -- {desktop_exec(executable)}\n')
    text = '\n'.join(line for line in text.splitlines() if not line.startswith('TryExec=')) + '\n'
    desktop.write_text(text)
    for size, filename in ((64, 'icon-64.png'), (1024, 'icon.png')):
        icon = prefix / f'share/icons/hicolor/{size}x{size}/apps/dev.stickitt.Stickitt.png'
        icon.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(bundle / filename, icon)
    updater = shutil.which('update-desktop-database')
    if updater:
        subprocess.run([updater, str(desktop.parent)], check=True)
    print(f'Installed {executable}')
    return executable, desktop


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--prefix', type=Path, default=Path.home() / '.local')
    args = parser.parse_args()
    install(Path(__file__).resolve().parent, args.prefix)
