#!/usr/bin/env python3
"""
Main entry point for the envira package.

This allows the package to be run with:
    python -m envira
"""

import sys
from .cli import main

if __name__ == "__main__":
    sys.exit(main()) 