from pathlib import Path
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
