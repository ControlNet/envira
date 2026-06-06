from pathlib import Path
import unittest


REPO_ROOT = Path(__file__).resolve().parents[1]


class TestLegacyScripts(unittest.TestCase):
    def test_run_user_fnm_install_uses_retry_wrapper(self):
        script_text = (REPO_ROOT / "run_user.sh").read_text()

        self.assertIn(
            'run_with_retries "curl -fsSL https://fnm.vercel.app/install | bash"',
            script_text,
        )
        self.assertNotIn("curl -o- https://fnm.vercel.app/install | bash", script_text)
