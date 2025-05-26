# Import all software classes for easy access
from .base import Software
from . import essentials

# Initialize the software registry
SOFTWARE_REGISTRY = Software.registry

__all__ = [
    "Software", "SOFTWARE_REGISTRY"
]
