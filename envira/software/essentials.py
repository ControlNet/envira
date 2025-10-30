import subprocess
import shutil
import os
import glob

from .base import Software
from ..util import on, impl, home
from ..util.result import Result, Success, Failure, Skip


def format_subprocess_error(e: subprocess.CalledProcessError, operation: str) -> str:
    """Helper function to format subprocess errors with detailed information"""
    error_msg = f"{operation} failed (exit code {e.returncode})"
    
    # Try to get error details from various sources
    error_details = None
    if hasattr(e, 'stderr') and e.stderr:
        error_details = e.stderr.decode() if isinstance(e.stderr, bytes) else str(e.stderr)
    elif hasattr(e, 'stdout') and e.stdout:
        error_details = e.stdout.decode() if isinstance(e.stdout, bytes) else str(e.stdout)
    elif hasattr(e, 'output') and e.output:
        error_details = e.output.decode() if isinstance(e.output, bytes) else str(e.output)
    
    if error_details:
        # Clean up the error message - take last few lines if it's long
        lines = error_details.strip().split('\n')
        if len(lines) > 3:
            error_details = '\n'.join(lines[-3:])
        error_msg += f": {error_details}"
    
    return error_msg


class TestOnly(Software):
    def __init__(self):
        super().__init__("test-only")
    
    def install_sudo(self) -> Result:
        try:
            # Commands that produce visible output for testing streaming
            subprocess.run(["echo", "Starting test installation..."], check=True, capture_output=True)
            subprocess.run(["echo", "Simulating package download..."], check=True, capture_output=True)
            
            # Use a command that produces lots of output to test streaming
            subprocess.run(["find", "/usr/bin", "-name", "*", "-type", "f"], check=True, capture_output=True)
            
            subprocess.run(["echo", "Installation progress: 25%"], check=True, capture_output=True)
            subprocess.run(["sleep", "1"], check=True, capture_output=True)
            subprocess.run(["echo", "Installation progress: 50%"], check=True, capture_output=True)
            subprocess.run(["sleep", "1"], check=True, capture_output=True)
            subprocess.run(["echo", "Installation progress: 75%"], check=True, capture_output=True)
            subprocess.run(["sleep", "1"], check=True, capture_output=True)
            subprocess.run(["echo", "Installation progress: 100%"], check=True, capture_output=True)
            subprocess.run(["echo", "Test installation completed successfully!"], check=True, capture_output=True)
            
            return Success("Test only installed")
        except subprocess.CalledProcessError as e:
            return Failure(format_subprocess_error(e, "test installation"))

    def install_user(self) -> Success | Failure | Skip:
        try:
            # User-scope test with different output
            subprocess.run(["echo", "Starting user-scope test installation..."], check=True, capture_output=True)
            subprocess.run(["echo", "Simulating package manager sync..."], check=True, capture_output=True)
            
            # Use a command that will produce lots of output for testing streaming
            subprocess.run(["find", "/usr/share", "-name", "*.txt", "-type", "f"], check=True, capture_output=True)
            
            subprocess.run(["echo", "Downloading test package (1/3)..."], check=True, capture_output=True)
            subprocess.run(["sleep", "2"], check=True, capture_output=True)
            subprocess.run(["echo", "Downloading test package (2/3)..."], check=True, capture_output=True)
            subprocess.run(["sleep", "2"], check=True, capture_output=True)
            subprocess.run(["echo", "Downloading test package (3/3)..."], check=True, capture_output=True)
            subprocess.run(["sleep", "2"], check=True, capture_output=True)
            
            subprocess.run(["echo", "Installing dependencies..."], check=True, capture_output=True)
            subprocess.run(["find", "/usr/bin", "-name", "python*"], check=True, capture_output=True)
            
            subprocess.run(["echo", "Configuring environment..."], check=True, capture_output=True)
            subprocess.run(["echo", "Setting up user directories..."], check=True, capture_output=True)
            subprocess.run(["echo", "Installing test software..."], check=True, capture_output=True)
            subprocess.run(["echo", "Running post-installation scripts..."], check=True, capture_output=True)
            subprocess.run(["echo", "Test installation completed successfully!"], check=True, capture_output=True)
            
            return Success("Test only installed (user)")
        except subprocess.CalledProcessError as e:
            return Failure(format_subprocess_error(e, "user test installation"))

    def upgrade_sudo(self) -> Success | Failure | Skip:
        return Skip()

    def upgrade_user(self) -> Success | Failure | Skip:
        return Skip()

    def is_installed_sudo(self) -> bool | None:
        return False  # Always show as not installed for testing

    def is_installed_user(self) -> bool | None:
        return False  # Always show as not installed for testing


class Essentials(Software):
    def __init__(self):
        super().__init__("essentials")
    
    @on.ubuntu
    @on.linuxmint
    @on.pop
    @impl.preferred
    def install_sudo(self) -> Result:
        os.environ["DEBIAN_FRONTEND"] = "noninteractive"
        packages = "iputils-ping net-tools python3-venv apt-utils make openssh-server gedit vim git git-lfs curl wget zsh gcc make perl build-essential libfuse2 python3-pip screen tmux ncdu pipx xsel screenfetch neofetch p7zip-full unzip mosh nmap"
        try:
            subprocess.run(["apt", "update"], check=True, capture_output=True)
            subprocess.run(["apt", "install", "-y"] + packages.split(), check=True, capture_output=True)
            return Success("Essentials installed via apt")
        except subprocess.CalledProcessError as e:
            return Failure(format_subprocess_error(e, "apt install"))

    @on.arch
    @on.endeavouros
    @impl.preferred
    def install_sudo(self) -> Result:
        packages = "gedit vim git git-lfs curl wget zsh gcc make perl base-devel binutils screen tmux ncdu python-pipx xsel screenfetch p7zip unzip mosh iperf3 nmap"
        try:
            subprocess.run(["pacman", "-Sy", "--noconfirm"] + packages.split(), check=True, capture_output=True)
            return Success("Essentials installed via pacman")
        except subprocess.CalledProcessError as e:
            return Failure(format_subprocess_error(e, "pacman install"))

    @on.manjaro
    @impl.preferred  
    def install_sudo(self) -> Result:
        packages = "gedit vim git git-lfs curl wget zsh gcc make perl base-devel binutils screen tmux ncdu python-pipx xsel screenfetch neofetch p7zip unzip yay mosh iperf3 nmap"
        try:
            subprocess.run(["pacman", "-Sy", "--noconfirm"] + packages.split(), check=True, capture_output=True)
            return Success("Essentials installed via pacman (Manjaro)")
        except subprocess.CalledProcessError as e:
            return Failure(format_subprocess_error(e, "pacman install"))
        
    @on.fedora
    @impl.preferred
    def install_sudo(self) -> Result:
        packages = "python3 pipx gedit vim git git-lfs curl wget zsh gcc make perl screen tmux ncdu xsel unzip screenfetch neofetch mosh iperf3 nmap"
        try:
            subprocess.run(["dnf", "install", "-y"] + packages.split(), check=True, capture_output=True)
            return Success("Essentials installed via dnf")
        except subprocess.CalledProcessError as e:
            return Failure(e)

    @on.opensuse
    @impl.preferred
    def install_sudo(self) -> Result:
        packages = "python3 python3-pip gedit vim git git-lfs curl wget zsh gcc make perl screen tmux ncdu xsel screenfetch neofetch p7zip unzip mosh iperf nmap"
        try:
            subprocess.run(["zypper", "install", "-y"] + packages.split(), check=True, capture_output=True)
            subprocess.run(["python3", "-m", "pip", "install", "--user", "pipx"], check=True, capture_output=True)
            subprocess.run(["python3", "-m", "pipx", "ensurepath"], check=True, capture_output=True)
            return Success("Essentials installed via zypper")
        except subprocess.CalledProcessError as e:
            return Failure(e)

    @on.other
    @impl.preferred
    def install_sudo(self) -> Result:
        """Try to detect package manager and install essentials"""
        # Try to detect the system and package manager
        raise NotImplementedError("Only supported on Ubuntu, Linux Mint, Arch, Manjaro, EndeavourOS, Fedora, and OpenSUSE")

    @on.arch
    @on.endeavouros
    @impl.preferred
    def install_user(self) -> Result:
        try:
            subprocess.run(["git", "clone", "https://aur.archlinux.org/yay.git"], check=True, capture_output=True)
            subprocess.run(["makepkg", "-si", "--noconfirm"], cwd="yay", check=True, capture_output=True)
            subprocess.run(["rm", "-rf", "yay"], check=True, capture_output=True)
            return Success("Essentials installed via yay")
        except subprocess.CalledProcessError as e:
            subprocess.run(["rm", "-rf", "yay"], check=True, capture_output=True)
            return Failure(e)
        
    @on.other
    @impl.preferred
    def install_user(self) -> Result:
        return Skip()

    def upgrade_sudo(self) -> Result:
        return self.install_sudo()

    def upgrade_user(self) -> Result:
        return self.install_user()

    def is_installed_sudo(self) -> bool | None:
        # Check if some key packages are installed
        key_packages = ["git", "curl", "wget", "vim"]
        for package in key_packages:
            if not shutil.which(package):
                return False
        return True

    @on.arch
    @on.endeavouros
    @impl.preferred
    def is_installed_user(self) -> bool | None:
        if shutil.which("yay") is None:
            return False
        return True
    
    @on.other
    @impl.preferred
    def is_installed_user(self) -> bool | None:
        return None


class Bat(Software):
    def __init__(self):
        super().__init__("bat", {"essentials"})

    @on.ubuntu
    @on.linuxmint
    @on.pop
    @impl.preferred
    def install_sudo(self) -> Result:
        try:
            subprocess.run(["apt", "install", "-y", "bat"], check=True, capture_output=True)
            subprocess.run(["ln", "-s", "/usr/bin/batcat", "/usr/bin/bat"], check=True, capture_output=True)
            return Success("Bat installed via apt")
        except subprocess.CalledProcessError as e:
            return Failure(format_subprocess_error(e, "bat apt install"))
        
    @on.arch
    @on.manjaro
    @on.endeavouros
    @impl.preferred
    def install_sudo(self) -> Result:
        try:
            subprocess.run(["pacman", "-Sy", "--noconfirm", "bat"], check=True, capture_output=True)
            return Success("Bat installed via pacman")
        except subprocess.CalledProcessError as e:
            return Failure(e)
        
    @on.fedora
    @impl.preferred
    def install_sudo(self) -> Result:
        try:
            subprocess.run(["dnf", "install", "-y", "bat"], check=True, capture_output=True)
            return Success("Bat installed via dnf")
        except subprocess.CalledProcessError as e:
            return Failure(e)
        
    @on.opensuse
    @impl.preferred
    def install_sudo(self) -> Result:
        try:
            subprocess.run(["zypper", "install", "-y", "bat"], check=True, capture_output=True)
            return Success("Bat installed via zypper")
        except subprocess.CalledProcessError as e:
            return Failure(e)
        
    @on.other
    @impl.preferred
    def install_sudo(self) -> Result:
        raise NotImplementedError("Only supported on Ubuntu, Linux Mint, Pop!_OS, Fedora, and OpenSUSE")
    
    def install_user(self) -> Result:
        try:
            subprocess.run(["wget", "-O", "bat.zip", "https://github.com/sharkdp/bat/releases/download/v0.25.0/bat-v0.25.0-x86_64-unknown-linux-musl.tar.gz"], check=True, capture_output=True)
            subprocess.run(["tar", "-xvzf", "bat.zip", "-C", f"{home}/.local/bin"], check=True, capture_output=True)
            subprocess.run(["mv", f"{home}/.local/bin/bat-v0.25.0-x86_64-unknown-linux-musl/bat", f"{home}/.local/bin/bat"], check=True, capture_output=True)
            subprocess.run(["rm", "-r", f"{home}/.local/bin/bat-v0.25.0-x86_64-unknown-linux-musl"], check=True, capture_output=True)
            subprocess.run(["rm", "bat.zip"], check=True, capture_output=True)
            return Success("Bat installed via wget")
        except subprocess.CalledProcessError as e:
            return Failure(e)

    def upgrade_sudo(self) -> Result:
        return self.install_sudo()
        
    def upgrade_user(self) -> Result:
        return Skip()
    
    def is_installed_user(self) -> bool | None:
        return os.path.exists(f"{home}/.local/bin/bat")
    
    def is_installed_sudo(self) -> bool | None:
        return os.path.exists(f"/usr/bin/bat")


class Ctop(Software):
    def __init__(self):
        super().__init__("ctop", {"essentials"})

    @on.ubuntu
    @on.linuxmint
    @on.pop
    @on.fedora
    @on.opensuse
    @impl.preferred
    def install_sudo(self) -> Result:
        try:
            subprocess.run(["wget", "https://github.com/bcicen/ctop/releases/download/v0.7.7/ctop-0.7.7-linux-amd64", "-O", "/usr/local/bin/ctop"], check=True, capture_output=True)
            subprocess.run(["chmod", "+x", "/usr/local/bin/ctop"], check=True, capture_output=True)
            return Success("Ctop installed via apt")
        except subprocess.CalledProcessError as e:
            return Failure(e)
        
    @on.arch
    @on.manjaro
    @on.endeavouros
    @impl.preferred
    def install_sudo(self) -> Result:
        try:
            subprocess.run(["wget", "https://github.com/bcicen/ctop/releases/download/v0.7.7/ctop-0.7.7-linux-amd64", "-O", "/usr/local/bin/ctop"], check=True, capture_output=True)
            subprocess.run(["chmod", "+x", "/usr/local/bin/ctop"], check=True, capture_output=True)
            return Success("Ctop installed via wget")
        except subprocess.CalledProcessError as e:
            return Failure(e)
        
    @on.other
    @impl.preferred
    def install_sudo(self) -> Result:
        raise NotImplementedError("Only supported on Ubuntu, Linux Mint, Pop!_OS, Fedora, and OpenSUSE")

    def install_user(self) -> Result:
        try:
            subprocess.run(["wget", "https://github.com/bcicen/ctop/releases/download/v0.7.7/ctop-0.7.7-linux-amd64", "-O", f"{home}/.local/bin/ctop"], check=True, capture_output=True)
            subprocess.run(["chmod", "+x", f"{home}/.local/bin/ctop"], check=True, capture_output=True)
            return Success("Ctop installed via wget")
        except subprocess.CalledProcessError as e:
            return Failure(e)

    def upgrade_sudo(self) -> Result:
        return self.install_sudo()

    def upgrade_user(self) -> Result:
        return Skip()

    def is_installed_sudo(self) -> bool | None:
        return os.path.exists(f"/usr/local/bin/ctop")

    def is_installed_user(self) -> bool | None:
        return os.path.exists(f"{home}/.local/bin/ctop")
    

class Fastfetch(Software):
    def __init__(self):
        super().__init__("fastfetch", {"essentials"})

    @on.ubuntu
    @on.linuxmint
    @on.pop
    @impl.preferred
    def install_sudo(self) -> Result:
        try:
            subprocess.run(["add-apt-repository", "-y", "ppa:zhangsongcui3371/fastfetch"], check=True, capture_output=True)
            subprocess.run(["apt", "update"], check=True, capture_output=True)
            subprocess.run(["apt", "install", "-y", "fastfetch"], check=True, capture_output=True)
            return Success("Fastfetch installed via apt")
        except subprocess.CalledProcessError as e:
            return Failure(e)
        
    @on.arch
    @on.manjaro
    @on.endeavouros
    @impl.preferred
    def install_sudo(self) -> Result:
        try:
            subprocess.run(["pacman", "-Sy", "--noconfirm", "fastfetch"], check=True, capture_output=True)
            return Success("Fastfetch installed via pacman")
        except subprocess.CalledProcessError as e:
            return Failure(e)
        
    @on.fedora
    @impl.preferred
    def install_sudo(self) -> Result:
        try:
            subprocess.run(["dnf", "install", "-y", "fastfetch"], check=True, capture_output=True)
            return Success("Fastfetch installed via dnf")
        except subprocess.CalledProcessError as e:
            return Failure(e)
        
    @on.opensuse
    @impl.preferred
    def install_sudo(self) -> Result:
        try:
            subprocess.run(["zypper", "install", "-y", "fastfetch"], check=True, capture_output=True)
            return Success("Fastfetch installed via zypper")
        except subprocess.CalledProcessError as e:
            return Failure(e)
        
    @on.other
    @impl.preferred
    def install_sudo(self) -> Result:
        raise NotImplementedError("Only supported on Ubuntu, Linux Mint, Pop!_OS, Fedora, and OpenSUSE")
    
    def install_user(self) -> Result:
        try:
            subprocess.run(["wget", "https://github.com/fastfetch-cli/fastfetch/releases/download/2.44.0/fastfetch-linux-amd64.zip", "-O", "fastfetch.zip"], check=True, capture_output=True)
            subprocess.run(["unzip", "fastfetch.zip"], check=True, capture_output=True)
            subprocess.run(["mv", "fastfetch-linux-amd64/usr/bin/fastfetch", f"{home}/.local/bin/fastfetch"], check=True, capture_output=True)
            subprocess.run(["rm", "-rf", "fastfetch-linux-amd64", "fastfetch.zip"], check=True, capture_output=True)
            return Success("Fastfetch installed via wget")
        except subprocess.CalledProcessError as e:
            return Failure(e)
        
    def upgrade_sudo(self) -> Result:
        return self.install_sudo()
    
    def upgrade_user(self) -> Result:
        return Skip()
    
    def is_installed_user(self) -> bool | None:
        return os.path.exists(f"{home}/.local/bin/fastfetch")
    
    def is_installed_sudo(self) -> bool | None:
        return os.path.exists(f"/usr/bin/fastfetch")

class Btop(Software):
    def __init__(self):
        super().__init__("btop", {"essentials"})

    def install_sudo(self) -> Result:
        """git clone https://github.com/aristocratos/btop && cd btop && make && sudo make install && cd .. && rm -rf btop"""
        try:
            subprocess.run(["git", "clone", "https://github.com/aristocratos/btop"], check=True, capture_output=True)
            subprocess.run(["make"], cwd="btop", check=True, capture_output=True)
            subprocess.run(["make", "install"], cwd="btop", check=True, capture_output=True)
            subprocess.run(["rm", "-rf", "btop"], check=True, capture_output=True)
            return Success("Btop installed via git")
        except subprocess.CalledProcessError as e:
            subprocess.run(["rm", "-rf", "btop"], check=True, capture_output=True)
            return Failure(e)
        
    def upgrade_sudo(self) -> Result:
        return self.install_sudo()

    def install_user(self) -> Result:
        try:
            subprocess.run(["git", "clone", "https://github.com/aristocratos/btop"], check=True, capture_output=True)
            subprocess.run(["make"], cwd="btop", check=True, capture_output=True)
            subprocess.run(["make", "install", f"PREFIX={home}/.local"], cwd="btop", check=True, capture_output=True)
            subprocess.run(["rm", "-rf", "btop"], check=True, capture_output=True)
            return Success("Btop installed via git")
        except subprocess.CalledProcessError as e:
            return Failure(e)
        
    def upgrade_user(self) -> Result:
        return self.install_user()

    def is_installed_sudo(self) -> bool | None:
        return os.path.exists(f"/usr/local/bin/btop") or os.path.exists(f"/usr/bin/btop")
    
    def is_installed_user(self) -> bool | None:
        return os.path.exists(f"{home}/.local/bin/btop")
        

class Vnc(Software):
    def __init__(self):
        super().__init__("vnc", {"essentials"})
        """sudo dnf install -y tigervnc-server"""

    @on.ubuntu
    @on.linuxmint
    @on.pop
    @impl.preferred
    def install_sudo(self) -> Result:
        """sudo apt install -y tigervnc-standalone-server tigervnc-common tigervnc-xorg-extension"""
        try:
            subprocess.run(["apt", "install", "-y", "tigervnc-standalone-server", "tigervnc-common", "tigervnc-xorg-extension"], check=True, capture_output=True)
            return Success("Vnc installed via apt")
        except subprocess.CalledProcessError as e:
            return Failure(e)
        
    @on.arch
    @on.endeavouros
    @on.manjaro
    @impl.preferred
    def install_sudo(self) -> Result:
        """sudo pacman -Sy --noconfirm tigervnc"""
        try:
            subprocess.run(["pacman", "-Sy", "--noconfirm", "tigervnc"], check=True, capture_output=True)
            return Success("Vnc installed via pacman")
        except subprocess.CalledProcessError as e:
            return Failure(e)
        
    @on.opensuse
    @impl.preferred
    def install_sudo(self) -> Result:
        """sudo zypper install -y tigervnc"""
        try:
            subprocess.run(["zypper", "install", "-y", "tigervnc"], check=True, capture_output=True)
            return Success("Vnc installed via zypper")
        except subprocess.CalledProcessError as e:
            return Failure(e)
        
    @on.fedora
    @impl.preferred
    def install_sudo(self) -> Result:
        """sudo dnf install -y tigervnc-server"""
        try:
            subprocess.run(["dnf", "install", "-y", "tigervnc-server"], check=True, capture_output=True)
            return Success("Vnc installed via dnf")
        except subprocess.CalledProcessError as e:
            return Failure(e)
        
    @on.other
    @impl.preferred
    def install_sudo(self) -> Result:
        raise NotImplementedError("Only supported on Ubuntu, Linux Mint, Pop!_OS, Fedora, and OpenSUSE")
    
    def install_user(self) -> Result:
        return Skip()
    
    def upgrade_sudo(self) -> Result:
        return self.install_sudo()
    
    def upgrade_user(self) -> Result:
        return Skip()
    
    def is_installed_sudo(self) -> bool | None:
        return (shutil.which("tigervncserver") is not None) or (shutil.which("vncserver") is not None) or (shutil.which("Xvnc") is not None)

    def is_installed_user(self) -> bool | None:
        return None

# -----------------------------
# Shell and environment setup
# -----------------------------

class OhMyZsh(Software):
    def __init__(self):
        super().__init__("oh-my-zsh", {"essentials"})

    def install_sudo(self) -> Result:
        # oh-my-zsh is per-user
        return Skip()

    def upgrade_sudo(self) -> Result:
        return Skip()

    def is_installed_sudo(self) -> bool | None:
        return None

    def is_installed_user(self) -> bool | None:
        return os.path.isdir(f"{home}/.oh-my-zsh")

    def install_user(self) -> Result:
        try:
            # Download installer to temp and run unattended
            subprocess.run(["curl", "-fsSL",
                            "https://raw.githubusercontent.com/ohmyzsh/ohmyzsh/master/tools/install.sh",
                            "-o", "omz_install.sh"], check=True, capture_output=True)
            subprocess.run(["sh", "omz_install.sh", "--unattended"], check=True, capture_output=True)
            subprocess.run(["rm", "-f", "omz_install.sh"], check=True, capture_output=True)
            return Success("oh-my-zsh installed")
        except subprocess.CalledProcessError as e:
            return Failure(format_subprocess_error(e, "oh-my-zsh install"))


class ZshTheme(Software):
    def __init__(self):
        super().__init__("zsh-theme", {"oh-my-zsh"})

    def install_sudo(self) -> Result:
        return Skip()

    def upgrade_sudo(self) -> Result:
        return Skip()

    def is_installed_sudo(self) -> bool | None:
        return None

    def is_installed_user(self) -> bool | None:
        theme_path = f"{home}/.oh-my-zsh/themes/mzz-ys.zsh-theme"
        if not os.path.exists(theme_path):
            return False
        # Check .zshrc theme line
        zshrc = f"{home}/.zshrc"
        if os.path.exists(zshrc):
            try:
                with open(zshrc, "r", encoding="utf-8", errors="ignore") as f:
                    content = f.read()
                    return "ZSH_THEME=\"mzz-ys\"" in content
            except Exception:
                return False
        return False

    def install_user(self) -> Result:
        try:
            # Download theme file
            subprocess.run([
                "curl", "-fsSL",
                "https://raw.githubusercontent.com/ControlNet/my-zsh-theme-env/main/files/mzz-ys.zsh-theme",
                "-o", f"{home}/.oh-my-zsh/themes/mzz-ys.zsh-theme"
            ], check=True, capture_output=True)

            # Update .zshrc: theme, plugins, PATH
            zshrc = f"{home}/.zshrc"
            tmp = f"{home}/temp.zshrc"
            if os.path.exists(zshrc):
                # Use sed pipeline similar to legacy script
                sed_script = (
                    "cat \"{z}\" | sed 's/ZSH_THEME=\\\"robbyrussell\\\"/ZSH_THEME=\\\"mzz-ys\\\"\\nZSH_DISABLE_COMPFIX=\\\"true\\\"/' "
                    "| sed 's/plugins=(git)/plugins=(git zsh-autosuggestions zsh-syntax-highlighting)/' "
                    "| sed 's/# export PATH=$HOME\\/bin:$HOME\\/.local\\/bin:\\/usr\\/local\\/bin:$PATH/export PATH=$HOME\\/.cargo\\/bin:$HOME\\/.local\\/bin:$HOME\\/bin:\\/usr\\/local\\/bin:$PATH/' "
                    "> \"{t}\""
                ).format(z=zshrc, t=tmp)
                subprocess.run(["bash", "-lc", sed_script], check=True, capture_output=True)
                subprocess.run(["mv", tmp, zshrc], check=True, capture_output=True)

            # Disable OMZ update prompt and set TERM
            subprocess.run(["bash", "-lc",
                            f'echo "export DISABLE_UPDATE_PROMPT=true" | cat - {zshrc} > {home}/temp && mv {home}/temp {zshrc}'],
                           check=True, capture_output=True)
            subprocess.run(["bash", "-lc",
                            f'echo "export TERM=xterm-256color" | cat - {zshrc} > {home}/temp && mv {home}/temp {zshrc}'],
                           check=True, capture_output=True)

            # Clone plugins
            subprocess.run(["git", "clone",
                            "https://github.com/zsh-users/zsh-autosuggestions",
                            f"{home}/.oh-my-zsh/custom/plugins/zsh-autosuggestions"],
                           check=False, capture_output=True)
            subprocess.run(["git", "clone",
                            "https://github.com/zsh-users/zsh-syntax-highlighting.git",
                            f"{home}/.oh-my-zsh/custom/plugins/zsh-syntax-highlighting"],
                           check=False, capture_output=True)

            return Success("zsh theme and plugins configured")
        except subprocess.CalledProcessError as e:
            return Failure(format_subprocess_error(e, "zsh theme setup"))


class TmuxConfig(Software):
    def __init__(self):
        super().__init__("tmux-config", {"essentials"})

    def install_sudo(self) -> Result:
        return Skip()

    def upgrade_sudo(self) -> Result:
        return Skip()

    def is_installed_sudo(self) -> bool | None:
        return None

    def is_installed_user(self) -> bool | None:
        tmux_conf = f"{home}/.tmux.conf"
        if not os.path.exists(tmux_conf):
            return False
        try:
            with open(tmux_conf, "r", encoding="utf-8", errors="ignore") as f:
                return 'set -g default-terminal "screen-256color"' in f.read()
        except Exception:
            return False

    def install_user(self) -> Result:
        try:
            with open(f"{home}/.tmux.conf", "a", encoding="utf-8") as f:
                f.write('set -g default-terminal "screen-256color"\n')
            return Success("tmux configured")
        except Exception as e:
            return Failure(e)


class GitConfig(Software):
    def __init__(self):
        super().__init__("git-config", {"essentials"})

    def install_sudo(self) -> Result:
        return Skip()

    def upgrade_sudo(self) -> Result:
        return Skip()

    def is_installed_sudo(self) -> bool | None:
        return None

    def is_installed_user(self) -> bool | None:
        try:
            alias = subprocess.run(["git", "config", "--global", "--get", "alias.lsd"],
                                   check=False, capture_output=True, text=True)
            helper = subprocess.run(["git", "config", "--global", "--get", "credential.helper"],
                                    check=False, capture_output=True, text=True)
            return (alias.stdout.strip() != "" and helper.stdout.strip() == "store")
        except Exception:
            return False

    def install_user(self) -> Result:
        try:
            subprocess.run(["git", "config", "--global", "alias.lsd",
                            "log --graph --decorate --pretty=oneline --abbrev-commit --all"],
                           check=True, capture_output=True)
            subprocess.run(["git", "config", "--global", "credential.helper", "store"],
                           check=True, capture_output=True)
            return Success("git config updated")
        except subprocess.CalledProcessError as e:
            return Failure(format_subprocess_error(e, "git config"))


# -----------------------------
# Runtime managers and languages
# -----------------------------

class Nvm(Software):
    def __init__(self):
        super().__init__("nvm", {"essentials"})

    def install_sudo(self) -> Result:
        return Skip()

    def upgrade_sudo(self) -> Result:
        return Skip()

    def is_installed_sudo(self) -> bool | None:
        return None

    def is_installed_user(self) -> bool | None:
        return os.path.exists(f"{home}/.nvm/nvm.sh")

    def install_user(self) -> Result:
        try:
            subprocess.run(["curl", "-o-",
                            "https://raw.githubusercontent.com/nvm-sh/nvm/v0.39.7/install.sh"],
                           check=True, capture_output=True)
            # The above prints script to stdout; pipe into bash via shell
            subprocess.run(["bash", "-lc",
                            "curl -o- https://raw.githubusercontent.com/nvm-sh/nvm/v0.39.7/install.sh | bash"],
                           check=True, capture_output=True)

            # Append NVM init to .zshrc
            subprocess.run(["bash", "-lc",
                            f"echo 'export NVM_DIR=\"$HOME/.nvm\"' >> {home}/.zshrc"],
                           check=True, capture_output=True)
            subprocess.run(["bash", "-lc",
                            f"echo '[ -s \"$NVM_DIR/nvm.sh\" ] && \\ . \"$NVM_DIR/nvm.sh\"' >> {home}/.zshrc"],
                           check=True, capture_output=True)
            subprocess.run(["bash", "-lc",
                            f"echo '[ -s \"$NVM_DIR/bash_completion\" ] && \\ . \"$NVM_DIR/bash_completion\"' >> {home}/.zshrc"],
                           check=True, capture_output=True)

            return Success("nvm installed")
        except subprocess.CalledProcessError as e:
            return Failure(format_subprocess_error(e, "nvm install"))


class NodeJS(Software):
    def __init__(self):
        super().__init__("nodejs", {"nvm"})

    def install_sudo(self) -> Result:
        return Skip()

    def upgrade_sudo(self) -> Result:
        return Skip()

    def is_installed_sudo(self) -> bool | None:
        return None

    def is_installed_user(self) -> bool | None:
        # Check if any Node v20 exists under NVM
        paths = glob.glob(f"{home}/.nvm/versions/node/v20*")
        return len(paths) > 0

    def install_user(self) -> Result:
        try:
            cmd = (
                'export NVM_DIR="$HOME/.nvm"; '
                '[ -s "$NVM_DIR/nvm.sh" ] && . "$NVM_DIR/nvm.sh"; '
                'nvm install 20'
            )
            subprocess.run(["bash", "-lc", cmd], check=True, capture_output=True)
            return Success("Node.js 20 installed via nvm")
        except subprocess.CalledProcessError as e:
            return Failure(format_subprocess_error(e, "node install"))


class Miniconda(Software):
    def __init__(self):
        super().__init__("miniconda", {"essentials"})

    def install_sudo(self) -> Result:
        return Skip()

    def upgrade_sudo(self) -> Result:
        return Skip()

    def is_installed_sudo(self) -> bool | None:
        return None

    def is_installed_user(self) -> bool | None:
        return os.path.exists(f"{home}/miniconda3/bin/conda")

    def install_user(self) -> Result:
        try:
            subprocess.run(["curl", "-s", "-L", "-o", "miniconda_installer.sh",
                            "https://repo.anaconda.com/miniconda/Miniconda3-latest-Linux-x86_64.sh"],
                           check=True, capture_output=True)
            subprocess.run(["bash", "miniconda_installer.sh", "-b"], check=True, capture_output=True)
            subprocess.run(["rm", "miniconda_installer.sh"], check=True, capture_output=True)
            # Initialize conda for zsh and hide prefix
            subprocess.run([f"{home}/miniconda3/bin/conda", "init", "zsh"], check=True, capture_output=True)
            with open(f"{home}/.condarc", "a", encoding="utf-8") as f:
                f.write("changeps1: false\n")
            return Success("Miniconda installed")
        except subprocess.CalledProcessError as e:
            return Failure(format_subprocess_error(e, "miniconda install"))
        except Exception as e:
            return Failure(e)


class Rust(Software):
    def __init__(self):
        super().__init__("rust", {"essentials"})

    def install_sudo(self) -> Result:
        return Skip()

    def upgrade_sudo(self) -> Result:
        return Skip()

    def is_installed_sudo(self) -> bool | None:
        return None

    def is_installed_user(self) -> bool | None:
        return shutil.which("rustc") is not None or os.path.exists(f"{home}/.cargo/bin/rustc")

    def install_user(self) -> Result:
        try:
            subprocess.run(["bash", "-lc", "curl https://sh.rustup.rs -sSf | sh -s -- -y"],
                           check=True, capture_output=True)
            return Success("Rust installed")
        except subprocess.CalledProcessError as e:
            return Failure(format_subprocess_error(e, "rustup install"))


class GoLang(Software):
    def __init__(self):
        super().__init__("golang", {"essentials"})

    def install_sudo(self) -> Result:
        return Skip()

    def upgrade_sudo(self) -> Result:
        return Skip()

    def is_installed_sudo(self) -> bool | None:
        return None

    def is_installed_user(self) -> bool | None:
        return shutil.which("go") is not None or os.path.exists(f"{home}/.go/bin/go")

    def install_user(self) -> Result:
        try:
            subprocess.run(["bash", "-lc", "wget -q -O - https://git.io/vQhTU | bash"],
                           check=True, capture_output=True)
            # Set Go env variables in .zshrc
            with open(f"{home}/.zshrc", "a", encoding="utf-8") as f:
                f.write("# GoLang\n")
                f.write("export GOROOT=$HOME/.go\n")
                f.write("export PATH=$GOROOT/bin:$PATH\n")
                f.write("export GOPATH=$HOME/go\n")
                f.write("export PATH=$GOPATH/bin:$PATH\n")
            return Success("Go installed")
        except subprocess.CalledProcessError as e:
            return Failure(format_subprocess_error(e, "go install"))
        except Exception as e:
            return Failure(e)


class Fzf(Software):
    def __init__(self):
        super().__init__("fzf", {"essentials"})

    def install_sudo(self) -> Result:
        return Skip()

    def upgrade_sudo(self) -> Result:
        return Skip()

    def is_installed_sudo(self) -> bool | None:
        return None

    def is_installed_user(self) -> bool | None:
        return os.path.exists(f"{home}/.fzf/bin/fzf") or shutil.which("fzf") is not None

    def install_user(self) -> Result:
        try:
            subprocess.run(["git", "clone", "--depth", "1",
                            "https://github.com/junegunn/fzf.git", f"{home}/.fzf"],
                           check=False, capture_output=True)
            subprocess.run([f"{home}/.fzf/install", "--all"], check=True, capture_output=True)
            with open(f"{home}/.zshrc", "a", encoding="utf-8") as f:
                f.write('[ -f ~/.fzf.zsh ] && source ~/.fzf.zsh\n')
            return Success("fzf installed")
        except subprocess.CalledProcessError as e:
            return Failure(format_subprocess_error(e, "fzf install"))


# -----------------------------
# Editors and tools
# -----------------------------

class Lazygit(Software):
    def __init__(self):
        super().__init__("lazygit", {"essentials"})

    def is_installed_sudo(self) -> bool | None:
        return shutil.which("lazygit") is not None

    def is_installed_user(self) -> bool | None:
        return shutil.which("lazygit") is not None or os.path.exists(f"{home}/.local/bin/lazygit")

    def install_sudo(self) -> Result:
        try:
            script = (
                'LAZYGIT_VERSION=$(curl -s "https://api.github.com/repos/jesseduffield/lazygit/releases/latest" | '
                "grep -Po '" + '"' + '\\"tag_name\\": \\"v\\K[^"]*' + '"' + "); "
                'curl -Lo lazygit.tar.gz "https://github.com/jesseduffield/lazygit/releases/latest/download/lazygit_${LAZYGIT_VERSION}_Linux_x86_64.tar.gz"; '
                'tar xf lazygit.tar.gz lazygit; install lazygit /usr/local/bin/lazygit; rm lazygit.tar.gz lazygit'
            )
            subprocess.run(["bash", "-lc", script], check=True, capture_output=True)
            return Success("lazygit installed")
        except subprocess.CalledProcessError as e:
            return Failure(format_subprocess_error(e, "lazygit install"))

    def install_user(self) -> Result:
        try:
            script = (
                'LAZYGIT_VERSION=$(curl -s "https://api.github.com/repos/jesseduffield/lazygit/releases/latest" | '
                "grep -Po '" + '"' + '\\"tag_name\\": \\"v\\K[^"]*' + '"' + "); "
                'curl -Lo lazygit.tar.gz "https://github.com/jesseduffield/lazygit/releases/latest/download/lazygit_${LAZYGIT_VERSION}_Linux_x86_64.tar.gz"; '
                'tar xf lazygit.tar.gz lazygit; install lazygit ~/.local/bin/lazygit; rm lazygit.tar.gz lazygit'
            )
            subprocess.run(["bash", "-lc", script], check=True, capture_output=True)
            return Success("lazygit installed (user)")
        except subprocess.CalledProcessError as e:
            return Failure(format_subprocess_error(e, "lazygit install"))

    def upgrade_sudo(self) -> Result:
        return self.install_sudo()

    def upgrade_user(self) -> Result:
        return self.install_user()


class Neovim(Software):
    def __init__(self):
        super().__init__("neovim", {"essentials"})

    def install_sudo(self) -> Result:
        return Skip()

    def upgrade_sudo(self) -> Result:
        return Skip()

    def is_installed_sudo(self) -> bool | None:
        return None

    def is_installed_user(self) -> bool | None:
        return shutil.which("nvim") is not None or os.path.exists(f"{home}/.local/bin/nvim")

    def install_user(self) -> Result:
        try:
            subprocess.run(["curl", "-LO",
                            "https://github.com/neovim/neovim/releases/download/v0.9.5/nvim-linux64.tar.gz"],
                           check=True, capture_output=True)
            subprocess.run(["tar", "-xzf", "nvim-linux64.tar.gz"], check=True, capture_output=True)
            subprocess.run(["mv", "nvim-linux64", f"{home}/.nvim"], check=True, capture_output=True)
            subprocess.run(["ln", "-sf", f"{home}/.nvim/bin/nvim", f"{home}/.local/bin/nvim"],
                           check=True, capture_output=True)
            subprocess.run(["rm", "-f", "nvim-linux64.tar.gz"], check=True, capture_output=True)
            return Success("Neovim installed")
        except subprocess.CalledProcessError as e:
            return Failure(format_subprocess_error(e, "neovim install"))


class LunarVim(Software):
    def __init__(self):
        super().__init__("lunarvim", {"neovim", "miniconda"})

    def install_sudo(self) -> Result:
        return Skip()

    def upgrade_sudo(self) -> Result:
        return Skip()

    def is_installed_sudo(self) -> bool | None:
        return None

    def is_installed_user(self) -> bool | None:
        return shutil.which("lvim") is not None or os.path.exists(f"{home}/.local/bin/lvim")

    def install_user(self) -> Result:
        try:
            script = (
                "LV_BRANCH='release-1.4/neovim-0.9' "
                "bash <(curl -s https://raw.githubusercontent.com/LunarVim/LunarVim/release-1.4/neovim-0.9/utils/installer/install.sh) -y"
            )
            subprocess.run(["bash", "-lc", script], check=True, capture_output=True)
            # Ensure Python neovim package
            subprocess.run([f"{home}/miniconda3/bin/python", "-m", "pip", "install", "neovim"],
                           check=True, capture_output=True)
            return Success("LunarVim installed")
        except subprocess.CalledProcessError as e:
            return Failure(format_subprocess_error(e, "lunarvim install"))


# -----------------------------
# Containers
# -----------------------------

class Docker(Software):
    def __init__(self):
        super().__init__("docker", {"essentials"})

    @on.ubuntu
    @on.linuxmint
    @on.pop
    @on.fedora
    @on.other
    @impl.preferred
    def install_sudo(self) -> Result:
        try:
            subprocess.run(["bash", "-lc", "curl -fsSL https://get.docker.com | sh"],
                           check=True, capture_output=True)
            subprocess.run(["groupadd", "-f", "docker"], check=False, capture_output=True)
            # Add current user to docker group
            user = os.environ.get("SUDO_USER") or os.environ.get("USER") or "root"
            subprocess.run(["usermod", "-aG", "docker", user], check=False, capture_output=True)
            return Success("Docker installed")
        except subprocess.CalledProcessError as e:
            return Failure(format_subprocess_error(e, "docker install"))

    def upgrade_sudo(self) -> Result:
        return self.install_sudo()

    def install_user(self) -> Result:
        return Skip()

    def upgrade_user(self) -> Result:
        return Skip()

    def is_installed_sudo(self) -> bool | None:
        return shutil.which("docker") is not None

    def is_installed_user(self) -> bool | None:
        return None


class Lazydocker(Software):
    def __init__(self):
        super().__init__("lazydocker", {"essentials"})

    def install_sudo(self) -> Result:
        try:
            subprocess.run(["bash", "-lc",
                            "curl https://raw.githubusercontent.com/jesseduffield/lazydocker/master/scripts/install_update_linux.sh | bash"],
                           check=True, capture_output=True)
            return Success("lazydocker installed")
        except subprocess.CalledProcessError as e:
            return Failure(format_subprocess_error(e, "lazydocker install"))

    def upgrade_sudo(self) -> Result:
        return self.install_sudo()

    def install_user(self) -> Result:
        try:
            subprocess.run(["bash", "-lc",
                            "curl https://raw.githubusercontent.com/jesseduffield/lazydocker/master/scripts/install_update_linux.sh | bash"],
                           check=True, capture_output=True)
            return Success("lazydocker installed (user)")
        except subprocess.CalledProcessError as e:
            return Failure(format_subprocess_error(e, "lazydocker install"))

    def upgrade_user(self) -> Result:
        return self.install_user()

    def is_installed_sudo(self) -> bool | None:
        return shutil.which("lazydocker") is not None

    def is_installed_user(self) -> bool | None:
        return shutil.which("lazydocker") is not None


# -----------------------------
# Package managers and tooling
# -----------------------------

class Pixi(Software):
    def __init__(self):
        super().__init__("pixi", {"essentials"})

    def install_sudo(self) -> Result:
        return Skip()

    def upgrade_sudo(self) -> Result:
        return Skip()

    def is_installed_sudo(self) -> bool | None:
        return None

    def is_installed_user(self) -> bool | None:
        return os.path.exists(f"{home}/.pixi/bin/pixi") or shutil.which("pixi") is not None

    def install_user(self) -> Result:
        try:
            subprocess.run(["bash", "-lc", "curl -fsSL https://pixi.sh/install.sh | bash"],
                           check=True, capture_output=True)
            # Completions and config
            subprocess.run(["bash", "-lc",
                            f"echo 'eval \"$(pixi completion --shell zsh)\"' >> {home}/.zshrc"],
                           check=True, capture_output=True)
            subprocess.run(["mkdir", "-p", f"{home}/.config/pixi"], check=True, capture_output=True)
            with open(f"{home}/.config/pixi/config.toml", "w", encoding="utf-8") as f:
                f.write("shell.change-ps1 = false\n")
            # Symlink
            if os.path.exists(f"{home}/.pixi/bin/pixi"):
                subprocess.run(["ln", "-sf", f"{home}/.pixi/bin/pixi", f"{home}/.local/bin/pixi"],
                               check=False, capture_output=True)
            return Success("pixi installed")
        except subprocess.CalledProcessError as e:
            return Failure(format_subprocess_error(e, "pixi install"))
        except Exception as e:
            return Failure(e)


class UV(Software):
    def __init__(self):
        super().__init__("uv", {"essentials"})

    def install_sudo(self) -> Result:
        return Skip()

    def upgrade_sudo(self) -> Result:
        return Skip()

    def is_installed_sudo(self) -> bool | None:
        return None

    def is_installed_user(self) -> bool | None:
        return shutil.which("uv") is not None

    def install_user(self) -> Result:
        try:
            subprocess.run(["pipx", "install", "uv"], check=True, capture_output=True)
            return Success("uv installed")
        except subprocess.CalledProcessError as e:
            return Failure(format_subprocess_error(e, "uv install"))


# -----------------------------
# Utilities
# -----------------------------

class Zoxide(Software):
    def __init__(self):
        super().__init__("zoxide", {"essentials"})

    def install_sudo(self) -> Result:
        return Skip()

    def upgrade_sudo(self) -> Result:
        return Skip()

    def is_installed_sudo(self) -> bool | None:
        return None

    def is_installed_user(self) -> bool | None:
        return shutil.which("zoxide") is not None

    def install_user(self) -> Result:
        try:
            subprocess.run(["bash", "-lc",
                            "curl -sS https://raw.githubusercontent.com/ajeetdsouza/zoxide/main/install.sh | bash"],
                           check=True, capture_output=True)
            with open(f"{home}/.bashrc", "a", encoding="utf-8") as f:
                f.write('eval "$(zoxide init bash)"\n')
            with open(f"{home}/.zshrc", "a", encoding="utf-8") as f:
                f.write('eval "$(zoxide init zsh)"\n')
            return Success("zoxide installed")
        except subprocess.CalledProcessError as e:
            return Failure(format_subprocess_error(e, "zoxide install"))
        except Exception as e:
            return Failure(e)


class Micro(Software):
    def __init__(self):
        super().__init__("micro", {"essentials"})

    def is_installed_sudo(self) -> bool | None:
        return shutil.which("micro") is not None

    def is_installed_user(self) -> bool | None:
        return shutil.which("micro") is not None or os.path.exists(f"{home}/.local/bin/micro")

    def install_sudo(self) -> Result:
        try:
            script = (
                'tmpd=$(mktemp -d); cd "$tmpd"; '
                'curl https://getmic.ro | bash; '
                'install micro /usr/local/bin/micro; '
                'cd - >/dev/null; rm -rf "$tmpd"'
            )
            subprocess.run(["bash", "-lc", script], check=True, capture_output=True)
            return Success("micro installed")
        except subprocess.CalledProcessError as e:
            return Failure(format_subprocess_error(e, "micro install"))

    def install_user(self) -> Result:
        try:
            script = (
                'tmpd=$(mktemp -d); cd "$tmpd"; '
                'curl https://getmic.ro | bash; '
                f'mv micro {home}/.local/bin; '
                'cd - >/dev/null; rm -rf "$tmpd"'
            )
            subprocess.run(["bash", "-lc", script], check=True, capture_output=True)
            # alias nano -> micro
            with open(f"{home}/.zshrc", "a", encoding="utf-8") as f:
                f.write("alias nano='micro'\n")
            return Success("micro installed (user)")
        except subprocess.CalledProcessError as e:
            return Failure(format_subprocess_error(e, "micro install"))
        except Exception as e:
            return Failure(e)


class PM2(Software):
    def __init__(self):
        super().__init__("pm2", {"nodejs"})

    def install_sudo(self) -> Result:
        return Skip()

    def upgrade_sudo(self) -> Result:
        return Skip()

    def is_installed_sudo(self) -> bool | None:
        return None

    def is_installed_user(self) -> bool | None:
        return shutil.which("pm2") is not None

    def install_user(self) -> Result:
        try:
            script = (
                'npm config set prefix "$HOME/.local/"; '
                'npm install -g pm2'
            )
            subprocess.run(["bash", "-lc", script], check=True, capture_output=True)
            return Success("pm2 installed")
        except subprocess.CalledProcessError as e:
            return Failure(format_subprocess_error(e, "pm2 install"))


class SpeedtestCLI(Software):
    def __init__(self):
        super().__init__("speedtest-cli", {"essentials"})

    def install_sudo(self) -> Result:
        return Skip()

    def upgrade_sudo(self) -> Result:
        return Skip()

    def is_installed_sudo(self) -> bool | None:
        return None

    def is_installed_user(self) -> bool | None:
        return shutil.which("speedtest") is not None or shutil.which("speedtest-cli") is not None

    def install_user(self) -> Result:
        try:
            subprocess.run(["pipx", "install", "speedtest-cli"], check=True, capture_output=True)
            return Success("speedtest-cli installed")
        except subprocess.CalledProcessError as e:
            return Failure(format_subprocess_error(e, "speedtest-cli install"))


class Gdown(Software):
    def __init__(self):
        super().__init__("gdown", {"essentials"})

    def install_sudo(self) -> Result:
        return Skip()

    def upgrade_sudo(self) -> Result:
        return Skip()

    def is_installed_sudo(self) -> bool | None:
        return None

    def is_installed_user(self) -> bool | None:
        return shutil.which("gdown") is not None

    def install_user(self) -> Result:
        try:
            subprocess.run(["pipx", "install", "gdown"], check=True, capture_output=True)
            return Success("gdown installed")
        except subprocess.CalledProcessError as e:
            return Failure(format_subprocess_error(e, "gdown install"))


class TLDR(Software):
    def __init__(self):
        super().__init__("tldr", {"essentials"})

    def install_sudo(self) -> Result:
        return Skip()

    def upgrade_sudo(self) -> Result:
        return Skip()

    def is_installed_sudo(self) -> bool | None:
        return None

    def is_installed_user(self) -> bool | None:
        return shutil.which("tldr") is not None

    def install_user(self) -> Result:
        try:
            subprocess.run(["pipx", "install", "tldr"], check=True, capture_output=True)
            return Success("tldr installed")
        except subprocess.CalledProcessError as e:
            return Failure(format_subprocess_error(e, "tldr install"))


class HuggingfaceCLI(Software):
    def __init__(self):
        super().__init__("huggingface-cli", {"essentials"})

    def install_sudo(self) -> Result:
        return Skip()

    def upgrade_sudo(self) -> Result:
        return Skip()

    def is_installed_sudo(self) -> bool | None:
        return None

    def is_installed_user(self) -> bool | None:
        return shutil.which("huggingface-cli") is not None

    def install_user(self) -> Result:
        try:
            subprocess.run(["pipx", "install", "huggingface-hub[cli,hf_xet]"], check=True, capture_output=True)
            return Success("huggingface-cli installed")
        except subprocess.CalledProcessError as e:
            return Failure(format_subprocess_error(e, "huggingface-cli install"))


# -----------------------------
# Monitoring tools
# -----------------------------

class Bottom(Software):
    def __init__(self):
        super().__init__("bottom", {"rust"})

    def install_sudo(self) -> Result:
        try:
            subprocess.run(["cargo", "install", "bottom"], check=True, capture_output=True)
            return Success("bottom installed")
        except subprocess.CalledProcessError as e:
            return Failure(format_subprocess_error(e, "bottom install"))

    def upgrade_sudo(self) -> Result:
        return self.install_sudo()

    def install_user(self) -> Result:
        try:
            subprocess.run(["cargo", "install", "bottom"], check=True, capture_output=True)
            return Success("bottom installed (user)")
        except subprocess.CalledProcessError as e:
            return Failure(format_subprocess_error(e, "bottom install"))

    def upgrade_user(self) -> Result:
        return self.install_user()

    def is_installed_sudo(self) -> bool | None:
        return shutil.which("btm") is not None

    def is_installed_user(self) -> bool | None:
        return shutil.which("btm") is not None


class Nvitop(Software):
    def __init__(self):
        super().__init__("nvitop", {"essentials"})

    def install_sudo(self) -> Result:
        return Skip()

    def upgrade_sudo(self) -> Result:
        return Skip()

    def is_installed_sudo(self) -> bool | None:
        return None

    def is_installed_user(self) -> bool | None:
        return shutil.which("nvitop") is not None

    def install_user(self) -> Result:
        try:
            subprocess.run(["pipx", "install", "nvitop"], check=True, capture_output=True)
            return Success("nvitop installed")
        except subprocess.CalledProcessError as e:
            return Failure(format_subprocess_error(e, "nvitop install"))


class Nviwatch(Software):
    def __init__(self):
        super().__init__("nviwatch", {"rust"})

    def install_sudo(self) -> Result:
        try:
            subprocess.run(["cargo", "install", "nviwatch"], check=True, capture_output=True)
            return Success("nviwatch installed")
        except subprocess.CalledProcessError as e:
            return Failure(format_subprocess_error(e, "nviwatch install"))

    def upgrade_sudo(self) -> Result:
        return self.install_sudo()

    def install_user(self) -> Result:
        try:
            subprocess.run(["cargo", "install", "nviwatch"], check=True, capture_output=True)
            return Success("nviwatch installed (user)")
        except subprocess.CalledProcessError as e:
            return Failure(format_subprocess_error(e, "nviwatch install"))

    def upgrade_user(self) -> Result:
        return self.install_user()

    def is_installed_sudo(self) -> bool | None:
        return shutil.which("nviwatch") is not None

    def is_installed_user(self) -> bool | None:
        return shutil.which("nviwatch") is not None


class Bandwhich(Software):
    def __init__(self):
        super().__init__("bandwhich", {"rust"})

    def install_sudo(self) -> Result:
        try:
            subprocess.run(["cargo", "install", "bandwhich"], check=True, capture_output=True)
            # Ensure system-wide binary if running as root
            if os.path.exists(f"{home}/.cargo/bin/bandwhich"):
                subprocess.run(["install", f"{home}/.cargo/bin/bandwhich", "/usr/local/bin"],
                               check=False, capture_output=True)
            return Success("bandwhich installed")
        except subprocess.CalledProcessError as e:
            return Failure(format_subprocess_error(e, "bandwhich install"))

    def upgrade_sudo(self) -> Result:
        return self.install_sudo()

    def install_user(self) -> Result:
        try:
            subprocess.run(["cargo", "install", "bandwhich"], check=True, capture_output=True)
            return Success("bandwhich installed (user)")
        except subprocess.CalledProcessError as e:
            return Failure(format_subprocess_error(e, "bandwhich install"))

    def upgrade_user(self) -> Result:
        return self.install_user()

    def is_installed_sudo(self) -> bool | None:
        return shutil.which("bandwhich") is not None

    def is_installed_user(self) -> bool | None:
        return shutil.which("bandwhich") is not None


# -----------------------------
# Services and extras
# -----------------------------

class Yazi(Software):
    def __init__(self):
        super().__init__("yazi", {"rust"})

    def install_sudo(self) -> Result:
        try:
            subprocess.run(["cargo", "install", "yazi-fm", "yazi-cli"], check=True, capture_output=True)
            return Success("yazi installed")
        except subprocess.CalledProcessError as e:
            return Failure(format_subprocess_error(e, "yazi install"))

    def upgrade_sudo(self) -> Result:
        return self.install_sudo()

    def install_user(self) -> Result:
        try:
            subprocess.run(["cargo", "install", "yazi-fm", "yazi-cli"], check=True, capture_output=True)
            # Basic theme config
            subprocess.run(["mkdir", "-p", f"{home}/.config/yazi"], check=True, capture_output=True)
            with open(f"{home}/.config/yazi/theme.toml", "w", encoding="utf-8") as f:
                f.write('[flavor]\nuse = "onedark"\n')
            return Success("yazi installed (user)")
        except subprocess.CalledProcessError as e:
            return Failure(format_subprocess_error(e, "yazi install"))
        except Exception as e:
            return Failure(e)

    def upgrade_user(self) -> Result:
        return self.install_user()

    def is_installed_sudo(self) -> bool | None:
        return shutil.which("yazi") is not None

    def is_installed_user(self) -> bool | None:
        return shutil.which("yazi") is not None


class Superfile(Software):
    def __init__(self):
        super().__init__("superfile", {"essentials"})

    def install_sudo(self) -> Result:
        try:
            # Prefer official installer (may install to /usr/local/bin)
            subprocess.run(["bash", "-lc", "bash -c \"$(curl -sLo- https://superfile.netlify.app/install.sh)\""],
                           check=True, capture_output=True)
            # Disable auto update if config exists
            cfg = f"{home}/.config/superfile/config.toml"
            if os.path.exists(cfg):
                subprocess.run(["sed", "-i", "-E",
                                's/^\s*auto_check_update\s*=.*/auto_check_update = false/', cfg],
                               check=False, capture_output=True)
            return Success("superfile installed")
        except subprocess.CalledProcessError as e:
            return Failure(format_subprocess_error(e, "superfile install"))

    def upgrade_sudo(self) -> Result:
        return self.install_sudo()

    def install_user(self) -> Result:
        try:
            # Fallback to tarball install
            subprocess.run(["wget",
                            "https://github.com/yorukot/superfile/releases/download/v1.1.5/superfile-linux-v1.1.5-amd64.tar.gz"],
                           check=True, capture_output=True)
            subprocess.run(["tar", "-xvf", "superfile-linux-v1.1.5-amd64.tar.gz"],
                           check=True, capture_output=True)
            subprocess.run(["mv", "dist/superfile-linux-v1.1.5-amd64/spf", f"{home}/.local/bin"],
                           check=True, capture_output=True)
            subprocess.run(["rm", "-r", "dist", "superfile-linux-v1.1.5-amd64.tar.gz"],
                           check=True, capture_output=True)
            cfg = f"{home}/.config/superfile/config.toml"
            if os.path.exists(cfg):
                subprocess.run(["sed", "-i", "-E",
                                's/^\s*auto_check_update\s*=.*/auto_check_update = false/', cfg],
                               check=False, capture_output=True)
            return Success("superfile installed (user)")
        except subprocess.CalledProcessError as e:
            return Failure(format_subprocess_error(e, "superfile install"))

    def upgrade_user(self) -> Result:
        return self.install_user()

    def is_installed_sudo(self) -> bool | None:
        return shutil.which("spf") is not None

    def is_installed_user(self) -> bool | None:
        return shutil.which("spf") is not None or os.path.exists(f"{home}/.local/bin/spf")


class MesloFont(Software):
    def __init__(self):
        super().__init__("meslo-font", {"essentials"})

    def install_sudo(self) -> Result:
        return Skip()

    def upgrade_sudo(self) -> Result:
        return Skip()

    def is_installed_sudo(self) -> bool | None:
        return None

    def is_installed_user(self) -> bool | None:
        font_dir = f"{home}/.local/share/fonts"
        if not os.path.isdir(font_dir):
            return False
        try:
            for fn in os.listdir(font_dir):
                if "Meslo" in fn:
                    return True
        except Exception:
            pass
        return False

    def install_user(self) -> Result:
        try:
            subprocess.run(["wget",
                            "https://github.com/ryanoasis/nerd-fonts/releases/download/v2.1.0/Meslo.zip"],
                           check=True, capture_output=True)
            subprocess.run(["mkdir", "-p", f"{home}/.local/share/fonts"], check=True, capture_output=True)
            subprocess.run(["unzip", "-o", "Meslo.zip", "-d", f"{home}/.local/share/fonts"],
                           check=True, capture_output=True)
            subprocess.run(["bash", "-lc", f"cd {home}/.local/share/fonts && rm -f *Windows*"],
                           check=True, capture_output=True)
            subprocess.run(["rm", "-f", "Meslo.zip"], check=True, capture_output=True)
            subprocess.run(["fc-cache", "-fv"], check=False, capture_output=True)
            return Success("Meslo font installed")
        except subprocess.CalledProcessError as e:
            return Failure(format_subprocess_error(e, "Meslo font install"))


class GitHubCLI(Software):
    def __init__(self):
        super().__init__("gh-cli", {"essentials"})

    def install_sudo(self) -> Result:
        try:
            subprocess.run(["bash", "-lc", "curl -sS https://webi.sh/gh | sh"],
                           check=True, capture_output=True)
            return Success("GitHub CLI installed")
        except subprocess.CalledProcessError as e:
            return Failure(format_subprocess_error(e, "gh install"))

    def upgrade_sudo(self) -> Result:
        return self.install_sudo()

    def install_user(self) -> Result:
        try:
            subprocess.run(["bash", "-lc", "curl -sS https://webi.sh/gh | sh"],
                           check=True, capture_output=True)
            return Success("GitHub CLI installed (user)")
        except subprocess.CalledProcessError as e:
            return Failure(format_subprocess_error(e, "gh install"))

    def upgrade_user(self) -> Result:
        return self.install_user()

    def is_installed_sudo(self) -> bool | None:
        return shutil.which("gh") is not None

    def is_installed_user(self) -> bool | None:
        return shutil.which("gh") is not None


class Syncthing(Software):
    def __init__(self):
        super().__init__("syncthing", {"essentials"})

    def install_sudo(self) -> Result:
        return Skip()

    def upgrade_sudo(self) -> Result:
        return Skip()

    def is_installed_sudo(self) -> bool | None:
        return None

    def is_installed_user(self) -> bool | None:
        return (shutil.which("syncthing") is not None or
                os.path.exists(f"{home}/.config/systemd/user/syncthing.service"))

    def install_user(self) -> Result:
        try:
            # Install syncthing
            subprocess.run(["bash", "-lc", "curl -sS https://webinstall.dev/syncthing | bash"],
                           check=True, capture_output=True)
            # Setup systemd user service
            subprocess.run(["mkdir", "-p", f"{home}/.config/systemd/user"], check=True, capture_output=True)
            subprocess.run([
                "wget",
                "https://raw.githubusercontent.com/ControlNet/my-zsh-theme-env/main/files/syncthing.service",
                "-O", f"{home}/.config/systemd/user/syncthing.service"
            ], check=True, capture_output=True)
            subprocess.run(["systemctl", "--user", "enable", "syncthing.service"],
                           check=False, capture_output=True)
            subprocess.run(["systemctl", "--user", "start", "syncthing.service"],
                           check=False, capture_output=True)
            return Success("syncthing installed and service enabled")
        except subprocess.CalledProcessError as e:
            return Failure(format_subprocess_error(e, "syncthing install"))


class JupyterService(Software):
    def __init__(self):
        super().__init__("jupyter", {"miniconda"})

    def install_sudo(self) -> Result:
        return Skip()

    def upgrade_sudo(self) -> Result:
        return Skip()

    def is_installed_sudo(self) -> bool | None:
        return None

    def is_installed_user(self) -> bool | None:
        return (shutil.which("jupyter-lab") is not None or shutil.which("jupyter") is not None)

    def install_user(self) -> Result:
        try:
            # Install jupyter stack via conda
            subprocess.run([f"{home}/miniconda3/bin/conda", "install", "-y",
                            "ipywidgets", "ipykernel", "jupyterlab", "jupyter"],
                           check=True, capture_output=True)
            # Setup systemd user service
            subprocess.run(["mkdir", "-p", f"{home}/.config/systemd/user"], check=True, capture_output=True)
            subprocess.run([
                "wget",
                "https://raw.githubusercontent.com/ControlNet/my-zsh-theme-env/main/files/jupyter.service",
                "-O", f"{home}/.config/systemd/user/jupyter.service"
            ], check=True, capture_output=True)
            subprocess.run(["systemctl", "--user", "enable", "jupyter.service"],
                           check=False, capture_output=True)
            subprocess.run(["systemctl", "--user", "start", "jupyter.service"],
                           check=False, capture_output=True)
            return Success("Jupyter installed and service enabled")
        except subprocess.CalledProcessError as e:
            return Failure(format_subprocess_error(e, "jupyter install"))


class GitKraken(Software):
    def __init__(self):
        super().__init__("gitkraken", {"essentials"})

    @on.fedora
    @on.opensuse
    @impl.preferred
    def install_sudo(self) -> Result:
        try:
            subprocess.run(["wget", "https://release.gitkraken.com/linux/gitkraken-amd64.rpm"],
                           check=True, capture_output=True)
            # Use dnf or zypper based on distro
            if shutil.which("dnf"):
                subprocess.run(["dnf", "install", "-y", "./gitkraken-amd64.rpm"],
                               check=True, capture_output=True)
            else:
                subprocess.run(["zypper", "install", "--allow-unsigned-rpm", "-y", "./gitkraken-amd64.rpm"],
                               check=True, capture_output=True)
            subprocess.run(["rm", "-f", "gitkraken-amd64.rpm"], check=True, capture_output=True)
            return Success("GitKraken installed (rpm)")
        except subprocess.CalledProcessError as e:
            return Failure(format_subprocess_error(e, "gitkraken install"))

    @on.ubuntu
    @on.linuxmint
    @on.pop
    @impl.preferred
    def install_sudo(self) -> Result:
        try:
            subprocess.run(["wget", "https://release.gitkraken.com/linux/gitkraken-amd64.deb"],
                           check=True, capture_output=True)
            subprocess.run(["apt", "install", "-y", "./gitkraken-amd64.deb"],
                           check=True, capture_output=True)
            subprocess.run(["rm", "-f", "gitkraken-amd64.deb"], check=True, capture_output=True)
            return Success("GitKraken installed (deb)")
        except subprocess.CalledProcessError as e:
            return Failure(format_subprocess_error(e, "gitkraken install"))

    @on.arch
    @on.endeavouros
    @on.manjaro
    @impl.preferred
    def install_sudo(self) -> Result:
        # AUR helper required; handled as part of essentials.user on Arch
        try:
            subprocess.run(["bash", "-lc", "yay -Sy --noconfirm gitkraken"],
                           check=True, capture_output=True)
            return Success("GitKraken installed (AUR)")
        except subprocess.CalledProcessError as e:
            return Failure(format_subprocess_error(e, "gitkraken AUR install"))

    @on.other
    @impl.preferred
    def install_sudo(self) -> Result:
        return Failure(Exception("GitKraken install not supported on this OS"))

    def upgrade_sudo(self) -> Result:
        return self.install_sudo()

    def install_user(self) -> Result:
        try:
            subprocess.run(["wget", "https://release.gitkraken.com/linux/gitkraken-amd64.tar.gz"],
                           check=True, capture_output=True)
            subprocess.run(["tar", "-xvzf", "gitkraken-amd64.tar.gz"], check=True, capture_output=True)
            subprocess.run(["mv", "gitkraken", f"{home}/.gitkraken"], check=True, capture_output=True)
            subprocess.run(["rm", "-f", "gitkraken-amd64.tar.gz"], check=True, capture_output=True)
            subprocess.run(["ln", "-sf", f"{home}/.gitkraken/gitkraken", f"{home}/.local/bin/gitkraken"],
                           check=True, capture_output=True)
            return Success("GitKraken installed (user)")
        except subprocess.CalledProcessError as e:
            return Failure(format_subprocess_error(e, "gitkraken user install"))

    def upgrade_user(self) -> Result:
        return self.install_user()

    def is_installed_sudo(self) -> bool | None:
        return shutil.which("gitkraken") is not None

    def is_installed_user(self) -> bool | None:
        return shutil.which("gitkraken") is not None or os.path.exists(f"{home}/.local/bin/gitkraken")


# Create instances to register them in the Software.registry
essentials = Essentials()
bat = Bat()
ctop = Ctop()
fastfetch = Fastfetch()
btop = Btop()
vnc = Vnc()
oh_my_zsh = OhMyZsh()
zsh_theme = ZshTheme()
tmux_cfg = TmuxConfig()
git_cfg = GitConfig()
nvm = Nvm()
nodejs = NodeJS()
miniconda = Miniconda()
rust = Rust()
golang = GoLang()
fzf = Fzf()
lazygit = Lazygit()
neovim = Neovim()
lunarvim = LunarVim()
docker = Docker()
lazydocker = Lazydocker()
pixi = Pixi()
uv_tool = UV()
zoxide_tool = Zoxide()
micro_editor = Micro()
pm2_tool = PM2()
speedtest = SpeedtestCLI()
gdown_cli = Gdown()
tldr_cli = TLDR()
huggingface_cli = HuggingfaceCLI()
bottom_tool = Bottom()
nvitop_tool = Nvitop()
nviwatch_tool = Nviwatch()
bandwhich_tool = Bandwhich()
yazi_tool = Yazi()
superfile_tool = Superfile()
meslo_font = MesloFont()
gh_cli = GitHubCLI()
syncthing = Syncthing()
jupyter = JupyterService()
gitkraken = GitKraken()
test_only = TestOnly()