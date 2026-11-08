from pathlib import Path
import re
import tempfile
import unittest

import windows_installer
from windows_installer import (APP_ID, compile_arguments, doctor_problems, file_version,
                               find_iscc, missing_files)

SCRIPT = windows_installer.SCRIPT.read_text(encoding="utf-8")


def setup_directive(name):
    match = re.search(rf"^{name}=(.*)$", SCRIPT, re.MULTILINE)
    return match and match.group(1)


class VersionTests(unittest.TestCase):
    def test_windows_version_resources_take_the_numeric_core(self):
        self.assertEqual(file_version("3.0.0-alpha.5"), "3.0.0.0")
        self.assertEqual(file_version("3.1.2"), "3.1.2.0")
        with self.assertRaises(ValueError):
            file_version("3.0")


class ScriptTests(unittest.TestCase):
    def test_the_app_id_never_changes(self):
        # Upgrades find the earlier install by this id; a new one installs a
        # second copy that the old uninstaller no longer knows about.
        self.assertEqual(APP_ID, "F33C27B9-52FF-4FEC-9338-1A61D8C8605B")
        self.assertEqual(setup_directive("AppId"), "{{{#AppId}}")

    def test_installs_per_user_without_administrator_rights(self):
        self.assertEqual(setup_directive("PrivilegesRequired"), "lowest")
        self.assertEqual(setup_directive("DefaultDirName"), r"{autopf}\course2md")
        self.assertIsNone(setup_directive("PrivilegesRequiredOverridesAllowed"))

    def test_an_upgrade_closes_the_running_app_by_force(self):
        # The app's window does not answer the Restart Manager's session-end
        # messages, so a polite close would leave setup waiting or failing.
        self.assertEqual(setup_directive("CloseApplications"), "force")

    def test_installs_the_whole_package_with_its_layout(self):
        self.assertIn(r'Source: "{#SourceDir}\*"; DestDir: "{app}"; '
                      'Flags: ignoreversion recursesubdirs createallsubdirs', SCRIPT)

    def test_never_deletes_user_data(self):
        # Settings, model caches and libraries live outside {app}; the script
        # may delete only inside its own directory.
        self.assertNotIn("[UninstallDelete]", SCRIPT)
        self.assertNotIn("[Code]", SCRIPT)
        for line in SCRIPT.splitlines():
            if line.startswith("Type: filesandordirs"):
                self.assertRegex(line, r'Name: "\{app\}\\[^"]+"$')

    def test_every_define_the_script_uses_is_passed(self):
        arguments = compile_arguments("ISCC.exe", "3.0.0-alpha.5", Path("pkg"), Path("out"))
        passed = {argument[2:].split("=", 1)[0] for argument in arguments if argument.startswith("/D")}
        used = set(re.findall(r"\{#(\w+)\}", SCRIPT)) - {"SourcePath"}
        self.assertEqual(used, passed)
        self.assertIn(f"/DAppId={APP_ID}", arguments)
        self.assertEqual(arguments[-1], str(windows_installer.SCRIPT))

    def test_signing_is_optional_and_covers_the_uninstaller(self):
        unsigned = compile_arguments("ISCC.exe", "3.0.0", Path("pkg"), Path("out"))
        self.assertFalse([argument for argument in unsigned
                          if argument.startswith("/S") or argument == "/DSign"])
        signed = compile_arguments("ISCC.exe", "3.0.0", Path("pkg"), Path("out"), sign="cmd $f")
        self.assertIn("/Scourse2md=cmd $f", signed)
        self.assertIn("/DSign", signed)
        self.assertIn("#ifdef Sign\nSignTool=course2md\n#endif", SCRIPT)
        # Inno Setup signs the embedded uninstaller with SignTool unless told not to.
        self.assertIsNone(setup_directive("SignedUninstaller"))

    def test_sign_command_quotes_paths_and_escapes_dollars(self):
        command = windows_installer.sign_command(
            r"C:\Program Files (x86)\signtool.exe", r"D:\t\a.pfx", "pa$s")
        self.assertEqual(command, r"$qC:\Program Files (x86)\signtool.exe$q sign /f $qD:\t\a.pfx$q "
                         "/p $qpa$$s$q /fd sha256 /tr http://timestamp.digicert.com /td sha256 $f")

    def test_every_language_file_exists_and_starts_with_a_bom(self):
        for name in re.findall(r'MessagesFile: "\{#SourcePath\}([^"]+)"', SCRIPT):
            data = (windows_installer.SCRIPT.parent / name).read_bytes()
            self.assertTrue(data.startswith(b"\xef\xbb\xbf"), name)

    def test_names_the_release_asset(self):
        self.assertEqual(windows_installer.OUTPUT_NAME, "course2md-desktop-windows-x64-setup")


class FindCompilerTests(unittest.TestCase):
    def test_iscc_variable_wins_then_the_newest_install_folder(self):
        self.assertEqual(find_iscc({"ISCC": "C:/x/ISCC.exe"}), Path("C:/x/ISCC.exe"))
        with tempfile.TemporaryDirectory() as tmp:
            for version in ("5", "6"):
                folder = Path(tmp) / f"Inno Setup {version}"
                folder.mkdir()
                (folder / "ISCC.exe").write_bytes(b"")
            found = find_iscc({"ProgramFiles(x86)": tmp, "PATH": ""})
            self.assertEqual(found, Path(tmp) / "Inno Setup 6/ISCC.exe")
        self.assertIsNone(find_iscc({"PATH": ""}))


class SmokeHelperTests(unittest.TestCase):
    def test_missing_files_compares_relative_paths(self):
        with tempfile.TemporaryDirectory() as package, tempfile.TemporaryDirectory() as app:
            for root in (package, app):
                (Path(root) / "yt-dlp").mkdir()
                (Path(root) / "yt-dlp/yt-dlp.exe").write_bytes(b"")
            (Path(package) / "ffmpeg.exe").write_bytes(b"")
            self.assertEqual(missing_files(package, app), [Path("ffmpeg.exe")])

    def test_replaced_reads_the_file_entry_of_that_file(self):
        app = Path("D:/a/course2md app")
        log = (f"-- File entry --\nDest filename: {app / 'ffmpeg.exe'}\nDest file exists.\n"
               "Installing the file.\nSuccessfully installed the file.\n"
               f"-- File entry --\nDest filename: {app / 'course2md-desktop.exe'}\n"
               "Dest file exists.\nInstalling the file.\n")
        self.assertTrue(windows_installer._replaced(log, app / "ffmpeg.exe"))
        self.assertFalse(windows_installer._replaced(log, app / "course2md-desktop.exe"))

    def test_doctor_must_find_each_tool_in_the_install_directory(self):
        app = Path("D:/a/_temp/course2md app")
        lines = [f"    Included with the app · {app / name}" for name in
                 ("ffmpeg.exe", "ffprobe.exe", "aria2c.exe")]
        self.assertEqual(doctor_problems("\n".join(lines), app), ["yt-dlp"])
        lines.append(f"    included with the app · {app / 'yt-dlp' / 'yt-dlp.exe'}")
        self.assertEqual(doctor_problems("\n".join(lines), app), [])


if __name__ == "__main__":
    unittest.main()
