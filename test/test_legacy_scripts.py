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

    def test_run_sh_installs_neofetch_when_ubuntu_repo_provides_it(self):
        script_text = (REPO_ROOT / "run.sh").read_text()

        self.assertIn(
            "if apt-cache show neofetch >/dev/null 2>&1; then",
            script_text,
        )
        self.assertIn("sudo apt install -y neofetch", script_text)

    def test_legacy_installers_use_current_agent_clis(self):
        for script_name in ("run.sh", "run_user.sh"):
            script_text = (REPO_ROOT / script_name).read_text()

            self.assertIn(
                "curl -fsSL https://antigravity.google/cli/install.sh | bash",
                script_text,
            )
            self.assertNotIn("@google/gemini-cli", script_text)
            self.assertIn("export DISABLE_UPDATE_PROMPT=true", script_text)
            self.assertIn(
                "Enable Oh My Zsh automatic updates without prompting",
                script_text,
            )

    def test_legacy_installers_use_current_python_packages(self):
        for script_name in ("run.sh", "run_user.sh"):
            script_text = (REPO_ROOT / script_name).read_text()

            self.assertIn("pipx install speedtest-cli", script_text)
            self.assertIn("python -m pip install pynvim", script_text)
            self.assertNotIn("python -m pip install neovim", script_text)
            self.assertIn("LunarVim/LunarVim", script_text)
            self.assertNotIn("conda-libmamba-solver", script_text)
            self.assertNotIn("conda config --set solver libmamba", script_text)

    def test_legacy_installers_use_current_pinned_tool_versions(self):
        for script_name in ("run.sh", "run_user.sh"):
            script_text = (REPO_ROOT / script_name).read_text()

            self.assertIn("neovim/releases/download/v0.12.5", script_text)
            self.assertIn("nvim-linux-x86_64.tar.gz", script_text)
            self.assertNotIn("nvim-linux64.tar.gz", script_text)
            self.assertIn("bat/releases/download/v0.26.1", script_text)

        run_sh = (REPO_ROOT / "run.sh").read_text()
        self.assertIn("nerd-fonts/releases/download/v3.5.1", run_sh)

        run_user_sh = (REPO_ROOT / "run_user.sh").read_text()
        self.assertIn("ncdu-2.9.1-linux-x86_64.tar.gz", run_user_sh)

    def test_run_sh_uses_safe_current_distro_package_commands(self):
        script_text = (REPO_ROOT / "run.sh").read_text()

        self.assertIn('grep -qiE "^ID=arch$"', script_text)
        self.assertNotIn('grep -qiE "Arch"', script_text)
        self.assertNotIn("pacman -Sy --noconfirm", script_text)
        self.assertIn("pacman -Syu --noconfirm --needed", script_text)

        centos_install = re.search(r"sudo yum install -y (?P<packages>.+)", script_text)
        self.assertIsNotNone(centos_install)
        assert centos_install is not None
        self.assertNotIn("build-essential", centos_install.group("packages").split())

    def test_legacy_installers_use_current_yazi_and_huggingface_commands(self):
        for script_name in ("run.sh", "run_user.sh"):
            script_text = (REPO_ROOT / script_name).read_text()

            self.assertIn("cargo install --force yazi-build", script_text)
            self.assertIn("ya pkg add damjee/onedark", script_text)
            self.assertIn('dark = "onedark"', script_text)
            self.assertNotIn("ya pack", script_text)
            self.assertNotIn('use = "onedark"', script_text)

        run_sh = (REPO_ROOT / "run.sh").read_text()
        self.assertIn(
            'sudo ln -sf "$HOME/.local/bin/hf" /usr/local/bin/hf',
            run_sh,
        )
        self.assertNotIn("huggingface-cli", run_sh)
        self.assertIn("https://superfile.dev/install.sh", run_sh)
        self.assertNotIn("superfile.netlify.app", run_sh)
        self.assertIn(
            'sudo ln -sf "$HOME/.local/bin/spf" /usr/local/bin/spf',
            run_sh,
        )

    def test_legacy_installers_exclude_beads_and_use_current_codex_configuration(self):
        for script_name in ("run.sh", "run_user.sh"):
            script_text = (REPO_ROOT / script_name).read_text()

            self.assertNotIn("/beads/", script_text)
            self.assertNotIn("steveyegge/beads", script_text)
            self.assertIn("[sandbox_workspace_write]", script_text)
            self.assertIn("network_access = true", script_text)

    def test_verify_tracks_current_commands_and_configuration(self):
        verify_text = (REPO_ROOT / "verify.sh").read_text()

        self.assertIn('check_optional_cmd "agy"', verify_text)
        self.assertIn('check_optional_cmd "hf"', verify_text)
        self.assertNotIn('check_optional_cmd "gemini"', verify_text)
        self.assertNotIn("huggingface-cli", verify_text)
        self.assertIn('check_optional_cmd "neofetch"', verify_text)
        self.assertIn('check_optional_cmd "speedtest"', verify_text)
        self.assertIn('check_optional_cmd "lvim"', verify_text)
        self.assertIn("check_codex_network_access", verify_text)
        self.assertIn('section == "[sandbox_workspace_write]"', verify_text)
        self.assertIn('check_optional_cmd "spf"', verify_text)
