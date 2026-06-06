from pathlib import Path
import unittest


REPO_ROOT = Path(__file__).resolve().parents[1]


class TestCiWorkflows(unittest.TestCase):
    def test_legacy_ci_includes_supported_ubuntu_integration_matrix(self):
        workflow = REPO_ROOT / ".github" / "workflows" / "lagacy_test.yaml"
        workflow_text = workflow.read_text()

        ubuntu_versions = ("22.04", "24.04", "26.04")

        for version in ubuntu_versions:
            self.assertIn(f"ubuntu{version}", workflow_text)

            for mode in ("sudo", "user"):
                dockerfile = REPO_ROOT / ".github" / "integration-test" / f"Dockerfile-ubuntu{version}-{mode}"
                self.assertTrue(dockerfile.exists(), f"missing {dockerfile}")
                self.assertIn(f"FROM ubuntu:{version}", dockerfile.read_text())
