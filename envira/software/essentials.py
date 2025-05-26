import subprocess
import shutil
import os

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
        return os.path.exists(f"/usr/local/bin/btop")
    
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

# Create instances to register them in the Software.registry
essentials = Essentials()
bat = Bat()
ctop = Ctop()
fastfetch = Fastfetch()
btop = Btop()
vnc = Vnc()
test_only = TestOnly()