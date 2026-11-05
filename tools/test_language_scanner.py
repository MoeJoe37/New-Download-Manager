#!/usr/bin/env python3
"""Regression coverage for translation discovery across formatting and module boundaries."""
from pathlib import Path
import tempfile
import unittest

import ndm_lng


class LanguageScanner(unittest.TestCase):
    def test_formatted_widget_calls_keep_labels_and_stop_at_statement_boundaries(self):
        before = '''
            fn draw(ui: &mut Ui) {
                let mut d = D::new(ui);
                d.check(7.0, 20.0, 154.0, 10.0, &mut on, "Use Speed Limiter").changed();
                d.text(1.0, 2.0, 30.0, 40.0, dynamic_text);
                let internal = "Internal parser fragment";
                let message = Dialog::Info { title: "Download list", text: "Saved safely" };
            }
        '''
        after = '''
            fn draw(ui: &mut Ui) {
                let mut d = D::new(ui);
                d
                    .check(
                        7.0, 20.0, 154.0, 10.0, &mut on,
                        "Use Speed Limiter",
                    )
                    .changed();
                d.text(1.0, 2.0, 30.0, 40.0, dynamic_text);
                let internal = "Internal parser fragment";
                let message = Dialog::Info {
                    title:
                        "Download list",
                    text:
                        "Saved safely",
                };
            }
        '''
        previous_root = ndm_lng.ROOT
        with tempfile.TemporaryDirectory(prefix="ndm-language-") as directory:
            root = Path(directory)
            (root / "src/nested").mkdir(parents=True)
            source = root / "src/nested/view.rs"
            (root / "src/nested/interaction_tests.rs").write_text('fn fixture() { tr("Fixture-only text"); }')
            try:
                ndm_lng.ROOT = root
                source.write_text(before)
                expected = set(ndm_lng.strings())
                source.write_text(after)
                actual = set(ndm_lng.strings())
            finally:
                ndm_lng.ROOT = previous_root
        self.assertEqual(expected, actual)
        self.assertTrue({"Use Speed Limiter", "Download list", "Saved safely"}.issubset(actual))
        self.assertNotIn("Internal parser fragment", actual)
        self.assertNotIn("Fixture-only text", actual)


if __name__ == "__main__":
    unittest.main()
