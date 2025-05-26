import platform
from typing import Callable, Literal, Dict, List
from dataclasses import dataclass
import distro
import os
from .result import Success, Failure, Skip, Result

__all__ = ["on", "impl"]


def _current_os_id() -> str:
    """Return a lowercase identifier such as 'ubuntu', 'arch', 'darwin', 'windows'."""
    if os.environ.get("OVERRIDE_OS", "") != "":
        return os.environ["OVERRIDE_OS"]
    
    sys_name = platform.system().lower()

    # Linux → get the distribution ID if possible
    if sys_name == "linux":
        return distro.id().lower() or "linux"
    return {"darwin": "macos", "windows": "windows"}.get(sys_name, "other")


@dataclass
class Implementation:
    """Represents a single implementation of a function."""
    func: Callable
    os_list: List[str]  # List of OS names this implementation supports
    impl_type: Literal["preferred", "fallback"]
    
    def matches_os(self, current_os: str) -> bool:
        """Check if this implementation matches the current OS."""
        return current_os in self.os_list or "other" in self.os_list


class _ImplRegistry:
    """Implementation registry for storing function implementations with metadata."""
    
    def __init__(self):
        # function_name -> list of implementations
        self._registry: Dict[str, List[Implementation]] = {}
        self._wrappers: Dict[str, Callable] = {}  # Cache of wrapped functions
    
    def register(self, func: Callable, os_list: List[str], impl_type: Literal["preferred", "fallback"]):
        """Register a function implementation with OS and type metadata."""
        # Create a fully qualified name to avoid conflicts between modules and classes
        if hasattr(func, '__module__') and hasattr(func, '__qualname__'):
            name = f"{func.__module__}.{func.__qualname__}"  # e.g. "controlnet_env.software.essentials.Essentials.install_sudo"
        elif hasattr(func, '__qualname__') and '.' in func.__qualname__:
            name = func.__qualname__  # e.g. "Essentials.install_sudo"
        else:
            name = func.__name__  # fallback for functions without qualname
        
        # Extract original function if already wrapped
        original_func = getattr(func, '_impl_dispatcher_original', func)
        
        # Create implementation entry
        impl = Implementation(
            func=original_func,
            os_list=os_list,
            impl_type=impl_type
        )
        
        # Add to registry
        if name not in self._registry:
            self._registry[name] = []
        self._registry[name].append(impl)
        
        # Create or return wrapper
        if name not in self._wrappers:
            wrapper = self._create_wrapper(name)
            self._wrappers[name] = wrapper
            # Set the wrapper in the calling frame's globals
            import inspect
            frame = inspect.currentframe()
            try:
                # Go up the call stack to find the frame where the decorator was called
                caller_frame = frame.f_back.f_back.f_back  # decorator -> _register_impl -> register
                if caller_frame and caller_frame.f_globals:
                    caller_frame.f_globals[name] = wrapper
            finally:
                del frame
        else:
            wrapper = self._wrappers[name]
        
        wrapper._impl_dispatcher_original = original_func
        return wrapper
    
    def _create_wrapper(self, func_name: str) -> Callable:
        """Create a wrapper function that dispatches to appropriate implementations."""
        
        def wrapper(*args, **kwargs) -> Result:
            current_os = _current_os_id()
            implementations = self._registry.get(func_name, [])
            
            # Filter implementations that match current OS
            # First try to find exact OS matches
            exact_matches = [impl for impl in implementations if current_os in impl.os_list]
            
            # If no exact matches, try "other" implementations
            if exact_matches:
                matching_impls = exact_matches
            else:
                other_matches = [impl for impl in implementations if "other" in impl.os_list]
                matching_impls = other_matches
            
            if not matching_impls:
                return Failure(Exception(f"No implementations of {func_name!r} for OS {current_os!r}"))
            
            # Try preferred implementations first
            preferred_impls = [impl for impl in matching_impls if impl.impl_type == "preferred"]
            fallback_impls = [impl for impl in matching_impls if impl.impl_type == "fallback"]
            errors = []
            
            # Try preferred implementations
            for impl in preferred_impls:
                try:
                    result = impl.func(*args, **kwargs)
                    match result:
                        case Success():
                            return result
                        case Failure(error):
                            errors.append(error)
                        case Skip():
                            return result
                        case _:
                            # For is_installed_* methods, return the result directly (bool | None)
                            if "is_installed" in func_name:
                                return result
                            else:
                                return Success(result)
                except Exception:
                    # Continue to next implementation
                    continue
            
            # Try fallback implementations
            for impl in fallback_impls:
                try:
                    result = impl.func(*args, **kwargs)
                    match result:
                        case Success():
                            return result
                        case Failure(error):
                            errors.append(error)
                        case Skip():
                            return result
                        case _:
                            # For is_installed_* methods, return the result directly (bool | None)
                            if "is_installed" in func_name:
                                return result
                            else:
                                return Success(result)
                except Exception:
                    # Continue to next implementation
                    continue
            
            # All implementations failed
            return Failure(Exception("\n".join(errors)))
        
        # Set wrapper metadata
        wrapper.__name__ = func_name
        wrapper.__doc__ = f"Dispatched function: {func_name}"
        return wrapper


# Global registry instance
_registry = _ImplRegistry()


class _OSDispatcher:
    """OS-specific dispatcher that works with the implementation registry."""
    
    def __getattr__(self, os_id: str):
        """Return a decorator for the specified OS."""
        os_key = os_id.lower()
        
        def decorator(func: Callable) -> Callable:
            # Get the implementation type if set
            impl_type = getattr(func, '_pending_impl_type', "preferred")  # default to preferred
            
            # Check if this function already has OS registrations pending
            pending_os_list = getattr(func, '_pending_os_list', [])
            pending_os_list.append(os_key)
            
            # Register with accumulated OS list
            wrapper = _registry.register(func, pending_os_list, impl_type)
            
            # Preserve both attributes on the wrapper for subsequent OS decorators
            if hasattr(func, '_pending_impl_type'):
                wrapper._pending_impl_type = func._pending_impl_type
            wrapper._pending_os_list = pending_os_list
            
            return wrapper
        
        return decorator


class _ImplDispatcher:
    """Implementation type dispatcher that works with the implementation registry."""
    
    def preferred(self, func: Callable) -> Callable:
        """Register as preferred implementation."""
        return self._mark_impl_type(func, "preferred")
    
    def fallback(self, func: Callable) -> Callable:
        """Register as fallback implementation."""
        return self._mark_impl_type(func, "fallback")
    
    def _mark_impl_type(self, func: Callable, impl_type: Literal["preferred", "fallback"]) -> Callable:
        """Mark the implementation type - actual registration happens in OS decorator."""
        # Store the implementation type for later registration
        func._pending_impl_type = impl_type
        return func


# Create dispatcher instances
on = _OSDispatcher()
impl = _ImplDispatcher() 