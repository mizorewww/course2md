import json
from pathlib import Path
import tempfile
import unittest

import licenses


class SwiftNoticesTests(unittest.TestCase):
    def package(self, root, *locations):
        (root / "Package.resolved").write_text(
            json.dumps({"pins": [{"location": location} for location in locations]}))

    def test_every_pin_contributes_its_license_files(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            self.package(root, "https://github.com/a/one.git", "https://github.com/a/Two")
            for name, file in [("one", "LICENSE.txt"), ("one", "NOTICE"), ("Two", "COPYING")]:
                (root / ".build/checkouts" / name).mkdir(parents=True, exist_ok=True)
                (root / ".build/checkouts" / name / file).write_text(f"text of {file}")
            (root / ".build/checkouts/one/README.md").write_text("not a license")
            notices = licenses.swift_notices(root)
            for expected in ["text of LICENSE.txt", "text of NOTICE", "text of COPYING"]:
                self.assertIn(expected, notices)
            self.assertNotIn("not a license", notices)

    def test_a_package_without_a_license_file_is_an_error(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            self.package(root, "https://github.com/a/absent.git")
            with self.assertRaisesRegex(OSError, "absent"):
                licenses.swift_notices(root)


if __name__ == "__main__":
    unittest.main()
