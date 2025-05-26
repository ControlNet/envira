"""
Utility functions for the CLI
"""

import os
import pwd
import getpass
from typing import Optional, Tuple

from ..software import Software


def is_running_as_sudo() -> bool:
    """Check if running as sudo"""
    return os.geteuid() == 0


def detect_privilege_level() -> str:
    """Detect if running as sudo or user"""
    if is_running_as_sudo():
        return "sudo (can access both sudo and user scope)"
    else:
        return "user (user scope only)"


def get_real_user_info() -> Tuple[str, str]:
    """Get the real user's username and home directory when running as sudo"""
    if not is_running_as_sudo():
        # Not running as sudo, return current user info
        username = getpass.getuser()
        home_dir = os.path.expanduser("~")
        return username, home_dir
    
    # Running as sudo, try to get the original user
    sudo_user = os.environ.get('SUDO_USER')
    if sudo_user:
        try:
            # Get the real user's info from password database
            user_info = pwd.getpwnam(sudo_user)
            return sudo_user, user_info.pw_dir
        except KeyError:
            pass
    
    # Fallback: try to determine from USER environment variable
    original_user = os.environ.get('USER', 'root')
    if original_user != 'root':
        try:
            user_info = pwd.getpwnam(original_user)
            return original_user, user_info.pw_dir
        except KeyError:
            pass
    
    # Final fallback: return root info
    return 'root', '/root'


def get_installation_symbol(installed: Optional[bool]) -> str:
    """Get symbol for installation status"""
    if installed is True:
        return "[green]✓[/green]"
    elif installed is False:
        return "[red]✗[/red]"
    else:  # None - not applicable
        return "[dim]N/A[/dim]"


def get_planned_installation_scope(software: Software, is_sudo: bool) -> Optional[str]:
    """Determine which scope will be used for installation/upgrade"""
    user_status = software.is_installed_user()
    
    if is_sudo:
        # Running as sudo: can install/upgrade in both sudo and user scopes
        sudo_status = software.is_installed_sudo()
        
        # Prefer sudo scope if available
        if sudo_status is not None:
            return "sudo"
        elif user_status is not None:
            # Only use user scope if sudo scope is not available
            return "user"
    else:
        # Running as user: only install/upgrade in user scope
        if user_status is not None:
            return "user"
    
    return None  # No scope available


def is_software_selectable(software: Software, is_sudo: bool) -> bool:
    """Check if software is selectable in current context"""
    sudo_installed = software.is_installed_sudo()
    user_installed = software.is_installed_user()
    
    if is_sudo:
        # Running as sudo: selectable if either scope is available
        return sudo_installed is not None or user_installed is not None
    else:
        # Running as user: only selectable if user scope is available
        return user_installed is not None 