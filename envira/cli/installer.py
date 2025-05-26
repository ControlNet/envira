"""
Main Software Installer orchestrator class
"""

from typing import Set, List

from rich.console import Console
from rich.prompt import Confirm
from rich.panel import Panel
from rich.align import Align
from rich import box

from .models import InstallationStep
from .utils import is_running_as_sudo, detect_privilege_level
from .ui import show_software_table, interactive_software_selection
from .steps import prepare_installation_steps
from .runner import run_installation


class SoftwareInstaller:
    """Main orchestrator for software installation"""
    
    def __init__(self):
        self.console = Console()
        self.is_sudo = is_running_as_sudo()
        self.selected_software: Set[str] = set()
        self.installation_steps: List[InstallationStep] = []
    
    def run(self):
        """Main CLI function"""
        self.console.print()
        self.console.print(Panel(
            Align.center("[bold cyan]Software Installation Manager[/bold cyan]"),
            box=box.DOUBLE
        ))
        self.console.print()
        
        # Show privilege level
        privilege_info = detect_privilege_level()
        self.console.print(f"[dim]Running as: {privilege_info}[/dim]")
        self.console.print()
        
        # Show software table
        show_software_table(self.console)
        
        # Ask if user wants to install
        if not Confirm.ask("Do you want to install software?"):
            self.console.print("[yellow]Installation cancelled[/yellow]")
            return
        
        # Interactive software selection
        self.console.print()
        selected_software = interactive_software_selection(self.console, self.is_sudo)
        
        if not selected_software:
            self.console.print("[yellow]No software selected[/yellow]")
            return
        
        # Prepare installation steps
        steps = prepare_installation_steps(selected_software, self.is_sudo)
        
        if not steps:
            self.console.print("[yellow]All selected software is already installed[/yellow]")
            return
        
        # Confirm installation
        self.console.print()
        self.console.print(f"[cyan]Will install {len(steps)} packages[/cyan]")
        if not Confirm.ask("Proceed with installation?"):
            self.console.print("[yellow]Installation cancelled[/yellow]")
            return
        
        # Run installation
        self.console.print()
        run_installation(steps, self.console, self.is_sudo) 