#!/usr/bin/env python3
"""
Software Installation CLI - Legacy Entry Point

This file is kept for backward compatibility.
For the modular implementation, see: envira/cli/

Usage:
    python cli.py                    # Legacy entry point
    python -m envira                 # New modular entry point (recommended)
"""

import sys
import warnings

# Show deprecation warning
warnings.warn(
    "Using 'cli.py' directly is deprecated. Please use 'python -m envira' instead.",
    DeprecationWarning,
    stacklevel=2
)

# Import and run the new modular CLI
from envira.cli import main

if __name__ == "__main__":
    sys.exit(main()) 