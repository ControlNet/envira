"""
User interface components for the CLI
"""

import sys
import termios
import tty
from typing import List, Set

from rich.console import Console
from rich.table import Table
from rich.panel import Panel
from rich.text import Text
from rich.align import Align
from rich import box

from ..software import SOFTWARE_REGISTRY, Software
from .utils import (
    get_installation_symbol, 
    get_planned_installation_scope, 
    is_software_selectable,
    detect_privilege_level
)


def get_key() -> str:
    """Get a single keypress from stdin, handling special keys like arrows"""
    fd = sys.stdin.fileno()
    old_settings = termios.tcgetattr(fd)
    try:
        tty.setraw(sys.stdin.fileno())
        key = sys.stdin.read(1)
        
        if ord(key) == 3:  # Ctrl+C
            raise KeyboardInterrupt
        elif ord(key) == 13:  # Enter
            return 'enter'
        elif ord(key) == 32:  # Space
            return 'space'
        elif ord(key) == 27:  # Escape sequence
            # Read the next two characters immediately
            try:
                # For arrow keys, we expect [A, [B, [C, [D
                char1 = sys.stdin.read(1)  # Should be '['
                char2 = sys.stdin.read(1)  # Should be 'A', 'B', 'C', or 'D'
                full_key = key + char1 + char2
            except:
                # If we can't read the full sequence, just return escape
                full_key = key
            
            if full_key == '\x1b[A':
                return 'up'
            elif full_key == '\x1b[B':
                return 'down'
            elif full_key == '\x1b[C':
                return 'right'
            elif full_key == '\x1b[D':
                return 'left'
            else:
                return 'escape'
        else:
            return key
    finally:
        termios.tcsetattr(fd, termios.TCSADRAIN, old_settings)


def get_selection_status_symbol(software: Software, is_selected: bool, scope: str, is_sudo: bool) -> str:
    """Get status symbol for interactive selection table, accounting for planned actions"""
    # Get current status for this scope
    if scope == "sudo":
        installed = software.is_installed_sudo()
    else:  # scope == "user"
        installed = software.is_installed_user()
    
    if installed is None:
        return "[dim]N/A[/dim]"
    elif is_selected:
        # Determine which scope will actually be affected
        planned_scope = get_planned_installation_scope(software, is_sudo)
        
        if planned_scope == scope:
            # This scope will be affected
            if installed:
                return "[yellow]→ Upgrade[/yellow]"
            else:
                return "[green]→ Install[/green]"
        else:
            # This scope won't be affected (either already satisfied or not the preferred scope)
            return get_installation_symbol(installed)
    else:
        # Not selected, show current status
        return get_installation_symbol(installed)


def show_software_table(console: Console):
    """Display the software installation status table"""
    table = Table(title="Software Installation Status", box=box.ROUNDED)
    table.add_column("Software", style="cyan", no_wrap=True)
    table.add_column("Installed (sudo)", justify="center")
    table.add_column("Installed (user)", justify="center")
    table.add_column("Dependencies", style="dim")
    
    for name, software in SOFTWARE_REGISTRY.items():
        sudo_status = software.is_installed_sudo()
        user_status = software.is_installed_user()
        
        deps = ", ".join(software.dependencies) if software.dependencies else "None"
        
        table.add_row(
            name,
            get_installation_symbol(sudo_status),
            get_installation_symbol(user_status),
            deps
        )
    
    console.print()
    console.print(table)
    console.print()


def auto_select_dependencies(software_name: str, selected: Set[str], is_sudo: bool) -> List[str]:
    """Auto-select dependencies that aren't installed and return list of auto-selected items"""
    auto_selected = []
    software = SOFTWARE_REGISTRY[software_name]
    
    for dep in software.dependencies:
        if dep in SOFTWARE_REGISTRY:
            dep_software = SOFTWARE_REGISTRY[dep]
            
            # Check if dependency is satisfied in the relevant scope
            if is_sudo:
                # Running as sudo: dependency is satisfied if installed in sudo scope
                dep_satisfied = dep_software.is_installed_sudo() is True
            else:
                # Running as user: dependency is satisfied if installed in user scope
                dep_satisfied = dep_software.is_installed_user() is True
            
            # If not satisfied and not already selected, auto-select it
            if not dep_satisfied and dep not in selected and is_software_selectable(dep_software, is_sudo):
                selected.add(dep)
                auto_selected.append(dep)
                # Recursively check dependencies of dependencies
                nested_auto = auto_select_dependencies(dep, selected, is_sudo)
                auto_selected.extend(nested_auto)
    
    return auto_selected


def interactive_software_selection(console: Console, is_sudo: bool) -> List[str]:
    """Interactive software selection with arrow keys and space"""
    software_list = list(SOFTWARE_REGISTRY.keys())
    selected = set()
    current_index = 0
    status_message = ""
    
    # Start with the first selectable item
    for i, software_name in enumerate(software_list):
        software = SOFTWARE_REGISTRY[software_name]
        if is_software_selectable(software, is_sudo):
            current_index = i
            break
    
    while True:
        console.clear()
        console.print("[bold cyan]Select Software to Install/Upgrade[/bold cyan]")
        console.print("[dim]Navigation: ↑/↓ arrows | Toggle: Space | Confirm: Enter | Exit: Ctrl+C or Q[/dim]")
        console.print(f"[dim]Running as: {detect_privilege_level()}[/dim]")
        console.print("[dim]⊘ = Not applicable in current context[/dim]")
        console.print()
        
        # Create selection table with 5 columns (cursor, checkbox, software, statuses, dependencies)
        table = Table(box=box.SIMPLE)
        table.add_column("", width=3)  # Cursor
        table.add_column("", width=3)  # Checkbox
        table.add_column("Software", style="cyan")
        table.add_column("Sudo", justify="center", width=15)
        table.add_column("User", justify="center", width=15)
        table.add_column("Dependencies", style="dim", width=20)
        
        for i, software_name in enumerate(software_list):
            software = SOFTWARE_REGISTRY[software_name]
            is_selectable = is_software_selectable(software, is_sudo)
            
            prefix = ">" if i == current_index else " "
            if is_selectable:
                checkbox = "[green]☑[/green]" if software_name in selected else "☐"
            else:
                checkbox = "[dim]⊘[/dim]"  # Not selectable indicator
            
            # Use consistent status symbol generation
            is_selected = software_name in selected
            sudo_status = get_selection_status_symbol(software, is_selected, "sudo", is_sudo)
            user_status = get_selection_status_symbol(software, is_selected, "user", is_sudo)
            
            # Format dependencies
            if software.dependencies:
                deps_status = []
                for dep in software.dependencies:
                    if dep in SOFTWARE_REGISTRY:
                        dep_software = SOFTWARE_REGISTRY[dep]
                        if is_sudo:
                            dep_installed = dep_software.is_installed_sudo()
                        else:
                            dep_installed = dep_software.is_installed_user()
                        
                        if dep_installed:
                            deps_status.append(f"[green]{dep}[/green]")
                        else:
                            deps_status.append(f"[red]{dep}[/red]")
                    else:
                        deps_status.append(f"[dim]{dep}[/dim]")
                dependencies_text = ", ".join(deps_status)
            else:
                dependencies_text = "[dim]None[/dim]"
            
            # Style: bold if current, dim if not selectable
            if i == current_index:
                style = "bold"
            elif not is_selectable:
                style = "dim"
            else:
                style = ""
            
            table.add_row(
                prefix,
                checkbox,
                software_name,
                sudo_status,
                user_status,
                dependencies_text,
                style=style
            )
        
        console.print(table)
        
        # Show selected software with planned actions
        if selected:
            actions = []
            for software_name in selected:
                software = SOFTWARE_REGISTRY[software_name]
                planned_scope = get_planned_installation_scope(software, is_sudo)
                
                if planned_scope:
                    # Get current status for the planned scope
                    if planned_scope == "sudo":
                        current_status = software.is_installed_sudo()
                    else:
                        current_status = software.is_installed_user()
                    
                    action = "upgrade" if current_status else "install"
                    actions.append(f"{software_name} ({planned_scope}: {action})")
                else:
                    # No scope available - shouldn't happen if software is selectable
                    actions.append(f"{software_name} (no action needed)")
            
            console.print(f"\n[green]Planned actions ({len(selected)}):[/green]")
            for action in actions[:3]:  # Show first 3
                console.print(f"  [green]•[/green] {action}")
            if len(actions) > 3:
                console.print(f"  [dim]... and {len(actions) - 3} more[/dim]")
        else:
            console.print(f"\n[dim]No software selected[/dim]")
        
        # Show current cursor position and status message
        console.print(f"\n[dim]Current: {software_list[current_index]}[/dim]")
        if status_message:
            console.print(status_message)
        
        try:
            key = get_key()
            
            # Clear status message on any key press
            if status_message:
                status_message = ""
            
            if key == 'enter':
                break
            elif key == 'space':
                # Toggle selection (only if selectable)
                software_name = software_list[current_index]
                software = SOFTWARE_REGISTRY[software_name]
                if is_software_selectable(software, is_sudo):
                    if software_name in selected:
                        selected.remove(software_name)
                    else:
                        selected.add(software_name)
                        # Auto-select dependencies if needed
                        auto_selected = auto_select_dependencies(software_name, selected, is_sudo)
                        if auto_selected:
                            status_message = f"[yellow]Auto-selected dependencies: {', '.join(auto_selected)}[/yellow]"
            elif key == 'up':
                # Move up, skipping non-selectable items
                for _ in range(len(software_list)):
                    current_index = (current_index - 1) % len(software_list)
                    software = SOFTWARE_REGISTRY[software_list[current_index]]
                    if is_software_selectable(software, is_sudo):
                        break
            elif key == 'down':
                # Move down, skipping non-selectable items
                for _ in range(len(software_list)):
                    current_index = (current_index + 1) % len(software_list)
                    software = SOFTWARE_REGISTRY[software_list[current_index]]
                    if is_software_selectable(software, is_sudo):
                        break
            elif key.lower() == 'q':
                # Quick exit option
                break
                
        except KeyboardInterrupt:
            console.print("\n[red]Installation cancelled[/red]")
            sys.exit(0)
        except Exception as e:
            # Fallback for terminals that don't support raw input
            console.print(f"\n[yellow]Keyboard input not supported, using fallback mode[/yellow]")
            console.print(f"[dim]Error: {e}[/dim]")
            try:
                choice = input(f"\nEnter software number (0-{len(software_list)-1}) or 'done': ")
                if choice.lower() == 'done':
                    break
                elif choice.isdigit():
                    idx = int(choice)
                    if 0 <= idx < len(software_list):
                        software_name = software_list[idx]
                        if software_name in selected:
                            selected.remove(software_name)
                            console.print(f"[yellow]Deselected: {software_name}[/yellow]")
                        else:
                            selected.add(software_name)
                            console.print(f"[green]Selected: {software_name}[/green]")
            except KeyboardInterrupt:
                console.print("\n[red]Installation cancelled[/red]")
                sys.exit(0)
    
    return list(selected) 