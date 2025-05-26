"""
Software Installation CLI with Rich Visualization
"""

import argparse

from .installer import SoftwareInstaller
from .models import InstallationStep

__all__ = ['SoftwareInstaller', 'InstallationStep']

def get_version():
    """Get the version of envira"""
    try:
        from envira import __version__
        return __version__
    except ImportError:
        return "unknown"

def main():
    """Main CLI entry point"""
    parser = argparse.ArgumentParser(
        prog="envira",
        description="Software environment management tool",
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog="""
Examples:
  envira             # Run interactive installation
  envira --version   # Show version information
        """
    )
    
    parser.add_argument(
        "--version", 
        action="version", 
        version=f"envira {get_version()}"
    )
    
    # Parse arguments
    args = parser.parse_args()
    
    # Run the installer
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