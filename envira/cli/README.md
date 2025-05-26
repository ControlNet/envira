# Software Installation CLI - Modular Architecture

This directory contains the modular implementation of the Software Installation CLI, refactored from the original monolithic `cli.py` file.

## Structure

```
envira/cli/
├── __init__.py         # Main entry point and exports
├── models.py           # Data models (InstallationStep)
├── utils.py            # Utility functions (privilege detection, symbols, user info)
├── ui.py               # User interface components (tables, interactive selection)
├── steps.py            # Step preparation and dependency resolution
├── runner.py           # Installation execution and streaming
├── installer.py        # Main orchestrator class
└── README.md           # This file
```

## Usage

### Recommended (using package main)
```bash
python -m envira
```

### Legacy (backward compatibility)
```bash
python cli.py
```

## Module Responsibilities

### `models.py`
- `InstallationStep`: Data class representing a single installation step

### `utils.py`
- Privilege detection (`is_running_as_sudo`, `detect_privilege_level`)
- User context management (`get_real_user_info`)
- Display symbols (`get_installation_symbol`)
- Scope planning (`get_planned_installation_scope`, `is_software_selectable`)

### `ui.py`
- Keyboard input handling (`get_key`)
- Software table display (`show_software_table`)
- Interactive selection (`interactive_software_selection`)
- Dependency auto-selection (`auto_select_dependencies`)
- Status symbol generation (`get_selection_status_symbol`)

### `steps.py`
- Installation step preparation (`prepare_installation_steps`)
- Dependency resolution with proper scope checking

### `runner.py`
- Software installation with streaming (`install_software_with_streaming`)
- Installation orchestration (`run_installation`)
- Progress visualization and logging
- Installation summary (`show_installation_summary`)

### `installer.py`
- Main orchestrator class (`SoftwareInstaller`)
- Coordinates all the above modules
- Main CLI flow and user interaction

## Key Features

- **Modular Design**: Each module has a clear responsibility
- **Dependency Resolution**: Smart dependency handling that respects installation scopes
- **Upgrade Support**: Can upgrade already-installed software
- **Streaming Output**: Real-time installation progress and logs
- **User Context Switching**: Proper handling of sudo/user installations
- **Rich UI**: Beautiful terminal interface with tables and progress indicators

## Migration from Legacy CLI

The new modular structure maintains full backward compatibility while providing:
- Better code organization and maintainability
- Easier testing and debugging
- Clear separation of concerns
- Reusable components 