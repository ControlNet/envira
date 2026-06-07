from pathlib import Path
import re
import unittest


REPO_ROOT = Path(__file__).resolve().parents[1]


class TestLegacyScripts(unittest.TestCase):
    def test_fnm_install_uses_retry_wrapper(self):
        for script_name in ("run.sh", "run_user.sh"):
            script_text = (REPO_ROOT / script_name).read_text()

            self.assertIn(
                'run_with_retries "curl -fsSL https://fnm.vercel.app/install | bash"',
                script_text,
            )
            self.assertNotIn("curl -o- https://fnm.vercel.app/install | bash", script_text)

    def test_run_sh_ubuntu_apt_packages_support_current_lts_and_devel(self):
        script_text = (REPO_ROOT / "run.sh").read_text()
        apt_install_match = re.search(
            r"sudo apt install -y (?P<packages>.+)",
            script_text,
        )
        self.assertIsNotNone(apt_install_match, "run.sh must install Ubuntu apt packages")
        assert apt_install_match is not None

        packages = set(apt_install_match.group("packages").split())

        self.assertNotIn(
            "neofetch",
            packages,
            "neofetch is unavailable in Ubuntu 26.04 apt repositories",
        )
        self.assertIn("software-properties-common", packages)
        self.assertIn("pciutils", packages)
        self.assertIn("fontconfig", packages)
