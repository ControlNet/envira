"""
Software Installation CLI with Rich Visualization
"""

from .installer import SoftwareInstaller
from .models import InstallationStep

__all__ = ['SoftwareInstaller', 'InstallationStep']

def main():
    """Main CLI entry point"""
    try:
        installer = SoftwareInstaller()
        installer.run()
    except KeyboardInterrupt:
        print("\nInstallation cancelled by user")
        return 1
    except Exception as e:
        print(f"Error: {e}")
        return 1
    return 0 