try:
    from ._version import __version__
except ImportError:
    # fallback for development
    __version__ = "unknown"

__all__ = ["__version__"]
