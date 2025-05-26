"""
Step preparation and dependency resolution for installation
"""

from typing import List

from ..software import SOFTWARE_REGISTRY
from .models import InstallationStep


def prepare_installation_steps(selected_software: List[str], is_sudo: bool) -> List[InstallationStep]:
    """Prepare installation steps with dependency resolution"""
    steps = []
    resolved = set()
    
    def add_software_steps(software_name: str):
        if software_name in resolved:
            return
        
        software = SOFTWARE_REGISTRY[software_name]
        
        # Add dependencies first (only if they need installation)
        for dep in software.dependencies:
            if dep in SOFTWARE_REGISTRY:
                dep_software = SOFTWARE_REGISTRY[dep]
                # Check if dependency needs installation in the relevant scope
                if is_sudo:
                    dep_needs_installation = dep_software.is_installed_sudo() is not True
                else:
                    dep_needs_installation = dep_software.is_installed_user() is not True
                
                # Only add steps for dependencies that need installation
                if dep_needs_installation:
                    add_software_steps(dep)
        
        user_status = software.is_installed_user()
        # Add installation/upgrade steps for this software
        if is_sudo:
            # Running as sudo: prefer sudo scope if available
            sudo_status = software.is_installed_sudo()
            
            if sudo_status is not None:
                steps.append(InstallationStep(software, "sudo", "pending"))
            elif user_status is not None:
                # Only use user scope if sudo scope is not available
                steps.append(InstallationStep(software, "user", "pending"))
        else:
            # Running as user: only user scope
            if user_status is not None:
                steps.append(InstallationStep(software, "user", "pending"))
        
        resolved.add(software_name)
    
    for software_name in selected_software:
        add_software_steps(software_name)
    
    return steps 