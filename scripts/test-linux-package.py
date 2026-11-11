#!/usr/bin/env python3
"""Check the installer with disposable files, including Desktop Entry quoting."""

import importlib.util
from pathlib import Path
import shutil
import subprocess
import tempfile
import time
import unittest

ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location('installer', ROOT / 'packaging/linux/install.py')
installer = importlib.util.module_from_spec(spec)
spec.loader.exec_module(installer)


class InstallationTests(unittest.TestCase):
    def test_install_reinstall_and_menu_launch_with_special_path(self):
        from gi.repository import GioUnix
        with tempfile.TemporaryDirectory(prefix='stickitt-package-test-') as temporary:
            bundle = Path(temporary) / 'bundle'
            bundle.mkdir()
            shutil.copyfile(ROOT / 'packaging/linux/dev.stickitt.Stickitt.desktop', bundle / 'dev.stickitt.Stickitt.desktop')
            for filename in ('icon.png', 'icon-64.png'):
                shutil.copyfile(ROOT / 'assets' / filename, bundle / filename)
            (bundle / 'stickit').write_text('#!/usr/bin/env python3\nfrom pathlib import Path\nPath(__file__).resolve().with_name("launched").write_text("ok")\n')
            prefix = Path(temporary) / 'a path with $ and % and " and \\ characters'
            executable, desktop = installer.install(bundle, prefix)
            self.assertEqual(executable.stat().st_mode & 0o777, 0o755)
            app = GioUnix.DesktopAppInfo.new_from_filename(str(desktop))
            self.assertIsNotNone(app)
            self.assertTrue(app.launch([], None))
            marker = executable.with_name('launched')
            for _ in range(100):
                if marker.exists():
                    break
                time.sleep(0.02)
            self.assertEqual(marker.read_text(), 'ok')
            installer.install(bundle, prefix)
            self.assertEqual(executable.read_bytes(), (bundle / 'stickit').read_bytes())
            self.assertEqual((prefix / 'share/icons/hicolor/64x64/apps/dev.stickitt.Stickitt.png').read_bytes(), (bundle / 'icon-64.png').read_bytes())
            subprocess.run(['desktop-file-validate', str(desktop)], check=True)
            with self.assertRaises(ValueError):
                installer.install(bundle, Path(temporary) / 'invalid\nname')


if __name__ == '__main__':
    unittest.main()
