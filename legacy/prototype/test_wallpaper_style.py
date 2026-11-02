import importlib.util
import json
import os
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch, Mock

MODULE = Path(__file__).resolve().parents[1] / 'wallpaper_accent.py'
spec = importlib.util.spec_from_file_location('wallpaper_style', MODULE)
w = importlib.util.module_from_spec(spec)
spec.loader.exec_module(w)

class WallpaperStyleTest(unittest.TestCase):
    def test_accent_tracks_hue_and_handles_monochrome(self):
        red = w.choose_accent(bytes([255, 0, 0]) * 64)
        blue = w.choose_accent(bytes([0, 0, 255]) * 64)
        self.assertGreater(red[0], red[2])
        self.assertGreater(blue[2], blue[0])
        self.assertEqual(w.choose_accent(bytes([0, 0, 0]) * 64), (61, 220, 151))

    def test_kvantum_recolors_and_preserves_unrelated_mappings(self):
        with tempfile.TemporaryDirectory() as tmp, patch.dict(os.environ, {'HOME': tmp, 'XDG_CONFIG_HOME': tmp+'/config', 'XDG_STATE_HOME': tmp+'/state'}):
            root = Path(tmp)/'config/Kvantum'; base = root/'Base'; base.mkdir(parents=True)
            (root/'kvantum.kvconfig').write_text('[General]\ntheme=Base\n[Applications]\nOther=dolphin, untouched\n')
            (base/'Base.kvconfig').write_text('[%General]\ntranslucent_windows=false\n[GeneralColors]\nhighlight.color=#3ddc97\n')
            (base/'Base.svg').write_text('<svg fill="#3ddc97"/>')
            with patch.object(w.shutil, 'which', return_value=None):
                self.assertTrue(w.make_kvantum((100,120,240), {'kvantum_apps':['dolphin']}))
            text=(root/'kvantum.kvconfig').read_text()
            self.assertIn('Other=untouched', text)
            self.assertIn('WallpaperStyle6478F0Glass=dolphin', text)
            self.assertIn('#6478f0',(root/'WallpaperStyle6478F0Glass/WallpaperStyle6478F0Glass.svg').read_text())
            self.assertIn('translucent_windows=true',(root/'WallpaperStyle6478F0Glass/WallpaperStyle6478F0Glass.kvconfig').read_text())

    def test_effect_generates_per_app_override_and_desktop_guard(self):
        with tempfile.TemporaryDirectory() as tmp, patch.dict(os.environ, {'HOME': tmp}):
            response = Mock(stdout='true\n')
            with patch.object(w.subprocess, 'run', return_value=response):
                w.install_effect({'opacity': {'browser': 0.91}, 'app_overrides': {'my-app': 0.73}})
            script = (Path(tmp)/'.local/share/kwin/effects/wallpaper-style/contents/code/main.js').read_text()
            self.assertIn('"my-app": 0.73', script)
            self.assertIn('browser.test(cls) ? 0.91', script)
            self.assertIn('userAmount(String(window.windowClass)) !== null', script)
            self.assertIn('window.onCurrentDesktop', script)
            self.assertIn('window.minimized', script)

    def test_effect_rejects_invalid_opacity_before_changing_settings(self):
        with self.assertRaises(ValueError): w.install_effect({'opacity': {'default': -1}})

if __name__ == '__main__': unittest.main()
