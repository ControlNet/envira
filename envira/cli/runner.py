"""
Installation execution and streaming for the CLI
"""

import os
import subprocess
import threading
import time
from typing import Optional, List, Callable

from rich.console import Console
from rich.table import Table
from rich.panel import Panel
from rich.live import Live
from rich.layout import Layout
from rich.text import Text
from rich.align import Align
from rich import box

from ..util.result import Result, Success, Failure, Skip
from .models import InstallationStep
from .utils import get_real_user_info


def install_software_with_streaming(step: InstallationStep, is_sudo: bool, 
                                   streaming_enabled: bool, callback: Optional[Callable] = None) -> Result:
    """Install a single software package with streaming output"""
    # Create a wrapper class to capture the original software methods
    original_run = subprocess.run
    
    def streaming_run(*args, **kwargs):
        # If capture_output is True, we want to stream instead
        if kwargs.get('capture_output', False) and streaming_enabled:
            try:
                kwargs.pop('capture_output')
                # Remove parameters that Popen doesn't accept
                check_param = kwargs.pop('check', False)
                kwargs['stdout'] = subprocess.PIPE
                kwargs['stderr'] = subprocess.PIPE
                kwargs['text'] = True
                
                process = subprocess.Popen(*args, **kwargs)
            except Exception as e:
                # If process creation fails, fall back to original behavior
                return original_run(*args, **kwargs)
            
            # Capture both stdout and stderr
            captured_output = []
            captured_errors = []
            
            def read_stdout():
                try:
                    while True:
                        line = process.stdout.readline()
                        if not line:  # EOF
                            break
                        line = line.strip()
                        if line:
                            captured_output.append(line)
                            step.log_output.append(line)
                            if callback:
                                callback(line)
                except Exception:
                    pass
                finally:
                    try:
                        process.stdout.close()
                    except:
                        pass
            
            def read_stderr():
                try:
                    while True:
                        line = process.stderr.readline()
                        if not line:  # EOF
                            break
                        line = line.strip()
                        if line:
                            captured_errors.append(line)
                            step.log_output.append(f"ERROR: {line}")
                            if callback:
                                callback(f"ERROR: {line}")
                except Exception:
                    pass
                finally:
                    try:
                        process.stderr.close()
                    except:
                        pass
            
            # Start threads to read stdout and stderr
            stdout_thread = threading.Thread(target=read_stdout, daemon=True)
            stderr_thread = threading.Thread(target=read_stderr, daemon=True)
            
            stdout_thread.start()
            stderr_thread.start()
            
            # Wait for process to complete
            try:
                returncode = process.wait()
            except Exception as e:
                # If process.wait() fails, kill the process and re-raise
                try:
                    process.kill()
                    process.wait()
                except:
                    pass
                raise e
            
            # Give threads a moment to finish reading any remaining output
            stdout_thread.join(timeout=2.0)
            stderr_thread.join(timeout=2.0)
            
            if returncode != 0 and check_param:
                # Create detailed error message
                cmd_str = ' '.join(args[0]) if isinstance(args[0], list) else str(args[0])
                error_details = []
                if captured_errors:
                    error_details.extend(captured_errors[-3:])  # Last 3 error lines
                if captured_output and not captured_errors:
                    error_details.extend(captured_output[-2:])  # Last 2 output lines if no stderr
                
                if error_details:
                    error_msg = f"Command '{cmd_str}' failed: {'; '.join(error_details)}"
                else:
                    error_msg = f"Command '{cmd_str}' failed with exit code {returncode}"
                
                # Create exception with detailed message
                exc = subprocess.CalledProcessError(returncode, args[0])
                exc.stderr = '\n'.join(captured_errors) if captured_errors else None
                exc.stdout = '\n'.join(captured_output) if captured_output else None
                # Override the string representation
                exc.__str__ = lambda: error_msg
                raise exc
            
            # Create a mock result object that behaves like subprocess.run result
            class MockResult:
                def __init__(self, returncode, stdout, stderr):
                    self.returncode = returncode
                    self.stdout = '\n'.join(stdout) if stdout else ""
                    self.stderr = '\n'.join(stderr) if stderr else ""
            
            return MockResult(returncode, captured_output, captured_errors)
        else:
            return original_run(*args, **kwargs)
    
    # Temporarily replace subprocess.run
    subprocess.run = streaming_run
    
    try:
        if step.scope == "sudo":
            result = step.software.install_sudo()
        else:
            # User scope installation - check if we need to switch user context
            if is_sudo:
                # Running as sudo but doing user installation - we should warn or switch context
                real_user, real_home = get_real_user_info()
                step.log_output.append(f"User-scope installation while running as sudo - target user: {real_user}")
                step.log_output.append(f"Target home directory: {real_home}")
                
                # Set environment variables for the user installation
                original_env = os.environ.copy()
                try:
                    os.environ['HOME'] = real_home
                    os.environ['USER'] = real_user
                    result = step.software.install_user()
                finally:
                    # Restore original environment
                    os.environ.clear()
                    os.environ.update(original_env)
            else:
                result = step.software.install_user()
        return result
    except Exception as e:
        return Failure(str(e))
    finally:
        # Restore original subprocess.run
        subprocess.run = original_run


def run_installation(steps: List[InstallationStep], console: Console, is_sudo: bool):
    """Run the installation process with docker-like visualization"""
    if not steps:
        console.print("[yellow]No software to install[/yellow]")
        return
    
    streaming_enabled = True  # Global flag for streaming
    
    layout = Layout()
    layout.split_column(
        Layout(name="header", size=3),
        Layout(name="progress", size=len(steps) + 2),
        Layout(name="current", size=10),
        Layout(name="logs")
    )
    
    # Header
    layout["header"].update(
        Panel(
            Align.center(f"[bold cyan]Installing {len(steps)} packages[/bold cyan]"),
            box=box.DOUBLE
        )
    )
    
    # Progress table
    progress_table = Table(box=box.SIMPLE)
    progress_table.add_column("Step", width=4)
    progress_table.add_column("Software", style="cyan")
    progress_table.add_column("Scope", width=6)
    progress_table.add_column("Status", width=12)
    
    # Current step display
    current_text = Text()
    
    # Logs display
    logs_text = Text()
    
    with Live(layout, console=console, refresh_per_second=10) as live:
        def update_display():
            # Update progress table
            progress_table = Table(box=box.SIMPLE)
            progress_table.add_column("Step", width=4)
            progress_table.add_column("Software", style="cyan")
            progress_table.add_column("Scope", width=6)
            progress_table.add_column("Status", width=55)  # Expanded for error messages
            
            current_step_index = -1
            for j, s in enumerate(steps):
                if s.status == "running":
                    current_step_index = j
                
                # Create status text with error details for failed steps
                if s.status == "failure" and s.result and isinstance(s.result, Failure):
                    # Truncate long error messages for the table
                    error_msg = str(s.result.error)
                    if len(error_msg) > 50:
                        error_msg = error_msg[:47] + "..."
                    status_text = f"[red]✗ {error_msg}[/red]"
                else:
                    status_text = {
                        "pending": "[dim]Pending[/dim]",
                        "running": "[yellow]Running...[/yellow]",
                        "success": "[green]✓ Success[/green]",
                        "failure": "[red]✗ Failed[/red]",
                        "skipped": "[blue]~ Skipped[/blue]"
                    }.get(s.status, s.status)
                
                style = "bold" if j == current_step_index else ""
                progress_table.add_row(
                    f"{j+1:2d}",
                    s.software.name,
                    s.scope,
                    status_text,
                    style=style
                )
            
            layout["progress"].update(Panel(progress_table, title="Installation Progress"))
            
            # Update current step
            if current_step_index >= 0:
                current_step = steps[current_step_index]
                current_text = Text()
                current_text.append(f"Installing: ", style="bold")
                current_text.append(f"{current_step.software.name}", style="cyan bold")
                current_text.append(f" ({current_step.scope} scope)", style="dim")
                layout["current"].update(Panel(current_text, title="Current Step"))
            
            # Update logs - show recent logs from current and completed steps
            logs_text = Text()
            total_logs = 0
            for log_step in steps:
                if log_step.log_output:
                    total_logs += len(log_step.log_output)
                    logs_text.append(f"{log_step.software.name} ({log_step.scope}):\n", style="cyan bold")
                    # Show last 15 lines to prevent overflow
                    recent_logs = log_step.log_output[-15:] if len(log_step.log_output) > 15 else log_step.log_output
                    for log_line in recent_logs:
                        logs_text.append(f"  {log_line}\n")
                    logs_text.append("\n")
            
            # Add debug info about log count and streaming status
            if total_logs == 0:
                logs_text.append("[dim]Waiting for installation output...[/dim]\n")
                logs_text.append(f"[dim]Streaming enabled: {streaming_enabled}[/dim]\n")
            
            # Show total log count in title
            log_title = f"Installation Logs ({total_logs} lines)" if total_logs > 0 else "Installation Logs"
            layout["logs"].update(Panel(logs_text, title=log_title, box=box.SIMPLE))
        
        def log_callback(line):
            """Callback for streaming log updates"""
            # Rate limit updates to avoid overwhelming Rich
            log_callback.last_update = getattr(log_callback, 'last_update', 0)
            current_time = time.time()
            if current_time - log_callback.last_update > 0.1:  # Update at most every 100ms
                update_display()
                log_callback.last_update = current_time
        
        for i, step in enumerate(steps):
            step.status = "running"
            step.log_output.append(f"Starting installation of {step.software.name}...")
            update_display()
            
            # Perform installation with streaming
            step.result = install_software_with_streaming(step, is_sudo, streaming_enabled, callback=log_callback)
            
            # Add completion message
            step.log_output.append(f"Installation of {step.software.name} completed.")
            
            # Update status based on result
            if isinstance(step.result, Success):
                step.status = "success"
                step.log_output.append(f"✓ {step.result.value}")
            elif isinstance(step.result, Failure):
                step.status = "failure"
                step.log_output.append(f"✗ Error: {step.result.error}")
                # If streaming fails, try once more without streaming
                if streaming_enabled:
                    step.log_output.append("Streaming failed, retrying without streaming...")
                    streaming_enabled = False  # Disable streaming for remaining installations
                    # Try again without streaming
                    step.result = install_software_with_streaming(step, is_sudo, streaming_enabled)
                    if isinstance(step.result, Success):
                        step.status = "success"
                        step.log_output.append(f"✓ {step.result.value}")
                    elif isinstance(step.result, Failure):
                        step.status = "failure"
                        step.log_output.append(f"✗ Error: {step.result.error}")
            elif isinstance(step.result, Skip):
                step.status = "skipped"
                step.log_output.append("~ Skipped")
            
            update_display()
            
            # Brief pause between installations
            time.sleep(0.5)
    
    # Show final summary
    show_installation_summary(steps, console)


def show_installation_summary(steps: List[InstallationStep], console: Console):
    """Show installation summary"""
    console.print()
    
    success_count = sum(1 for step in steps if step.status == "success")
    failure_count = sum(1 for step in steps if step.status == "failure")
    skipped_count = sum(1 for step in steps if step.status == "skipped")
    
    summary_table = Table(title="Installation Summary", box=box.ROUNDED)
    summary_table.add_column("Result", style="bold")
    summary_table.add_column("Count", justify="center")
    summary_table.add_column("Software", style="dim")
    
    if success_count > 0:
        successful_software = [s.software.name for s in steps if s.status == "success"]
        summary_table.add_row(
            "[green]Successful[/green]",
            str(success_count),
            ", ".join(successful_software)
        )
    
    if failure_count > 0:
        failed_software = [s.software.name for s in steps if s.status == "failure"]
        summary_table.add_row(
            "[red]Failed[/red]",
            str(failure_count),
            ", ".join(failed_software)
        )
    
    if skipped_count > 0:
        skipped_software = [s.software.name for s in steps if s.status == "skipped"]
        summary_table.add_row(
            "[blue]Skipped[/blue]",
            str(skipped_count),
            ", ".join(skipped_software)
        )
    
    console.print(summary_table)
    console.print()
    
    if failure_count > 0:
        console.print("[red]Some installations failed. Check the logs above for details.[/red]")
    elif success_count > 0:
        console.print("[green]All installations completed successfully![/green]")
    else:
        console.print("[blue]No installations were performed.[/blue]") 