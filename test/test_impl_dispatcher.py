import unittest
import os
import sys
from unittest.mock import patch

# Add the parent directory to sys.path so we can import envira
sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

from envira.util.impl_dispatcher import on, impl, _registry, _current_os_id, _ImplRegistry, Implementation
from envira.util.result import Success, Failure


class TestImplDispatcher(unittest.TestCase):
    """Comprehensive tests for the implementation dispatcher."""
    
    def setUp(self):
        """Reset the registry before each test to ensure isolation."""
        # Store original registry state
        self._original_registry = _registry._registry.copy()
        self._original_wrappers = _registry._wrappers.copy()
        
        # Clear registry for test
        _registry._registry.clear()
        _registry._wrappers.clear()
    
    def tearDown(self):
        """Restore the registry after each test."""
        _registry._registry.clear()
        _registry._registry.update(self._original_registry)
        _registry._wrappers.clear()
        _registry._wrappers.update(self._original_wrappers)
    
    def test_current_os_detection(self):
        """Test OS detection functionality."""
        # Test environment override
        with patch.dict(os.environ, {"OVERRIDE_OS": "test_os"}):
            self.assertEqual(_current_os_id(), "test_os")
        
        # Test normal detection (will vary by system, but should not crash)
        os.environ.pop("OVERRIDE_OS", None)
        current_os = _current_os_id()
        self.assertIsInstance(current_os, str)
        self.assertGreater(len(current_os), 0)
    
    def test_basic_os_dispatcher(self):
        """Test basic OS-specific dispatching."""
        with patch.dict(os.environ, {"OVERRIDE_OS": "ubuntu"}):
            @on.ubuntu
            def test_func():
                return Success("ubuntu_result")
            
            @on.arch
            def test_func():
                return Success("arch_result")
            
            result = test_func()
            self.assertIsInstance(result, Success)
            self.assertEqual(result.value, "ubuntu_result")
    
    def test_implementation_type_dispatcher(self):
        """Test preferred/fallback implementation dispatching."""
        with patch.dict(os.environ, {"OVERRIDE_OS": "ubuntu"}):
            @on.ubuntu
            @impl.preferred
            def test_func():
                raise Exception("preferred failed")
            
            @on.ubuntu
            @impl.fallback
            def test_func():
                return Success("fallback_result")
            
            result = test_func()
            self.assertIsInstance(result, Success)
            self.assertEqual(result.value, "fallback_result")
    
    def test_multiple_os_same_function(self):
        """Test multiple OS decorators on the same function."""
        with patch.dict(os.environ, {"OVERRIDE_OS": "ubuntu"}):
            @on.ubuntu
            @on.linuxmint
            @impl.preferred
            def test_func():
                return Success("ubuntu_linuxmint_result")
            
            result = test_func()
            self.assertIsInstance(result, Success)
            self.assertEqual(result.value, "ubuntu_linuxmint_result")
        
        # Test with linuxmint
        with patch.dict(os.environ, {"OVERRIDE_OS": "linuxmint"}):
            result = test_func()
            self.assertIsInstance(result, Success)
            self.assertEqual(result.value, "ubuntu_linuxmint_result")
    
    def test_fallback_sequence(self):
        """Test that preferred implementations are tried before fallback."""
        execution_order = []
        
        with patch.dict(os.environ, {"OVERRIDE_OS": "ubuntu"}):
            @on.ubuntu
            @impl.preferred
            def test_func():
                execution_order.append("preferred1")
                raise Exception("preferred1 failed")
            
            @on.ubuntu
            @impl.preferred
            def test_func():
                execution_order.append("preferred2")
                raise Exception("preferred2 failed")
            
            @on.ubuntu
            @impl.fallback
            def test_func():
                execution_order.append("fallback1")
                return Success("fallback1_success")
            
            @on.ubuntu
            @impl.fallback
            def test_func():
                execution_order.append("fallback2")
                return Success("fallback2_success")
            
            result = test_func()
            
            # Should try all preferred first, then fallbacks
            self.assertEqual(execution_order, ["preferred1", "preferred2", "fallback1"])
            self.assertIsInstance(result, Success)
            self.assertEqual(result.value, "fallback1_success")
    
    def test_other_os_fallback(self):
        """Test that 'other' OS implementations work as fallback."""
        with patch.dict(os.environ, {"OVERRIDE_OS": "unknown_os"}):
            @on.ubuntu
            @impl.preferred
            def test_func():
                return Success("ubuntu_result")
            
            @on.other
            @impl.preferred
            def test_func():
                return Success("other_result")
            
            result = test_func()
            self.assertIsInstance(result, Success)
            self.assertEqual(result.value, "other_result")
    
    def test_no_implementation_found(self):
        """Test behavior when no implementation matches the current OS."""
        with patch.dict(os.environ, {"OVERRIDE_OS": "nonexistent_os"}):
            @on.ubuntu
            def test_func():
                return Success("ubuntu_result")
            
            result = test_func()
            self.assertIsInstance(result, Failure)
            self.assertIn("No implementations", str(result.error))
    
    def test_all_implementations_fail(self):
        """Test behavior when all implementations fail."""
        with patch.dict(os.environ, {"OVERRIDE_OS": "ubuntu"}):
            @on.ubuntu
            @impl.preferred
            def test_func():
                raise Exception("preferred failed")
            
            @on.ubuntu
            @impl.fallback
            def test_func():
                raise Exception("fallback failed")
            
            result = test_func()
            self.assertIsInstance(result, Failure)
            self.assertIn("All implementations", str(result.error))
    
    def test_software_failure_result(self):
        """Test that Failure results are handled correctly."""
        with patch.dict(os.environ, {"OVERRIDE_OS": "ubuntu"}):
            @on.ubuntu
            @impl.preferred
            def test_func():
                return Failure(Exception("preferred failed"))
            
            @on.ubuntu
            @impl.fallback
            def test_func():
                return Success("fallback success")
            
            result = test_func()
            self.assertIsInstance(result, Success)
            self.assertEqual(result.value, "fallback success")
    
    def test_non_software_result_wrapping(self):
        """Test that non-Software return values are wrapped in Success."""
        with patch.dict(os.environ, {"OVERRIDE_OS": "ubuntu"}):
            @on.ubuntu
            def test_func():
                return "plain_string_result"
            
            result = test_func()
            self.assertIsInstance(result, Success)
            self.assertEqual(result.value, "plain_string_result")
    
    def test_registry_isolation(self):
        """Test that different function names don't interfere with each other."""
        with patch.dict(os.environ, {"OVERRIDE_OS": "ubuntu"}):
            @on.ubuntu
            def func1():
                return Success("func1_result")
            
            @on.ubuntu
            def func2():
                return Success("func2_result")
            
            result1 = func1()
            result2 = func2()
            
            self.assertEqual(result1.value, "func1_result")
            self.assertEqual(result2.value, "func2_result")
    
    def test_implementation_dataclass(self):
        """Test the Implementation dataclass functionality."""
        def dummy_func():
            pass
        
        impl = Implementation(
            func=dummy_func,
            os_list=["ubuntu", "linuxmint"],
            impl_type="preferred"
        )
        
        self.assertTrue(impl.matches_os("ubuntu"))
        self.assertTrue(impl.matches_os("linuxmint"))
        self.assertFalse(impl.matches_os("arch"))
        
        # Test 'other' matching
        impl_other = Implementation(
            func=dummy_func,
            os_list=["other"],
            impl_type="fallback"
        )
        
        self.assertTrue(impl_other.matches_os("any_os"))
        self.assertTrue(impl_other.matches_os("ubuntu"))
    
    def test_complex_scenario(self):
        """Test a complex scenario with multiple OS and implementation types."""
        execution_log = []
        
        with patch.dict(os.environ, {"OVERRIDE_OS": "ubuntu"}):
            # Ubuntu preferred - should succeed
            @on.ubuntu
            @on.linuxmint
            @impl.preferred
            def install_docker():
                execution_log.append("ubuntu_linuxmint_preferred")
                return Success("apt install")
            
            # Ubuntu-only fallback - should not be called
            @on.ubuntu
            @impl.fallback
            def install_docker():
                execution_log.append("ubuntu_fallback")
                return Success("snap install")
            
            # Arch preferred - should not be called
            @on.arch
            @impl.preferred
            def install_docker():
                execution_log.append("arch_preferred")
                return Success("pacman install")
            
            # Other fallback - should not be called
            @on.other
            @impl.fallback
            def install_docker():
                execution_log.append("other_fallback")
                return Success("compile from source")
            
            result = install_docker()
            
            # Only the ubuntu/linuxmint preferred should execute
            self.assertEqual(execution_log, ["ubuntu_linuxmint_preferred"])
            self.assertIsInstance(result, Success)
            self.assertEqual(result.value, "apt install")
    
    def test_decorator_stacking_preservation(self):
        """Test that implementation type is preserved across multiple OS decorators."""
        with patch.dict(os.environ, {"OVERRIDE_OS": "ubuntu"}):
            @on.ubuntu
            @on.linuxmint
            @impl.fallback
            def test_func():
                return Success("success")
            
            # Check that both OS entries have the correct implementation type
            # Find the full function name in the registry
            func_name = None
            for name in _registry._registry.keys():
                if name.endswith('test_func'):
                    func_name = name
                    break
            
            self.assertIsNotNone(func_name, "Could not find test_func in registry")
            implementations = _registry._registry.get(func_name, [])
            ubuntu_impls = [impl for impl in implementations if 'ubuntu' in impl.os_list]
            linuxmint_impls = [impl for impl in implementations if 'linuxmint' in impl.os_list]
            
            self.assertEqual(len(ubuntu_impls), 1)
            self.assertEqual(len(linuxmint_impls), 1)
            self.assertEqual(ubuntu_impls[0].impl_type, "fallback")
            self.assertEqual(linuxmint_impls[0].impl_type, "fallback")
    
    def test_comprehensive_docker_example(self):
        """Test the comprehensive Docker installation example from comprehensive_example_2.py."""
        execution_log = []
        
        # Test with Ubuntu
        with patch.dict(os.environ, {"OVERRIDE_OS": "ubuntu"}):
            @on.ubuntu
            @on.linuxmint
            @impl.preferred
            def install_docker():
                """Preferred method for Ubuntu family: use apt"""
                execution_log.append("ubuntu_apt")
                raise Exception("apt repository not available")

            @on.ubuntu
            @impl.fallback
            def install_docker():
                """Fallback 1 for Ubuntu family: use snap"""
                execution_log.append("ubuntu_snap")
                raise Exception("snap not installed")

            @on.linuxmint
            @impl.fallback
            def install_docker():
                """Fallback 1 for LinuxMint family: use flatpak"""
                execution_log.append("linuxmint_flatpak")
                raise Exception("flatpak not installed")

            @on.ubuntu
            @on.linuxmint
            @impl.fallback
            def install_docker():
                """Fallback 2 for Ubuntu family: manual download"""
                execution_log.append("manual_download")
                return Success("Docker installed via manual download on Ubuntu family")

            @on.other
            @impl.preferred
            def install_docker():
                """Preferred method for other systems: try generic package manager"""
                execution_log.append("other_preferred")
                return Failure(Exception("No package manager found"))
            
            result = install_docker()
            
            # On Ubuntu, should try: apt (preferred) -> snap (ubuntu fallback) -> manual download (shared fallback)
            self.assertEqual(execution_log, ["ubuntu_apt", "ubuntu_snap", "manual_download"])
            self.assertIsInstance(result, Success)
            self.assertEqual(result.value, "Docker installed via manual download on Ubuntu family")
        
        # Reset execution log and test with LinuxMint
        execution_log.clear()
        
        with patch.dict(os.environ, {"OVERRIDE_OS": "linuxmint"}):
            result = install_docker()
            
            # On LinuxMint, should try: apt (preferred) -> flatpak (linuxmint fallback) -> manual download (shared fallback)
            self.assertEqual(execution_log, ["ubuntu_apt", "linuxmint_flatpak", "manual_download"])
            self.assertIsInstance(result, Success)
            self.assertEqual(result.value, "Docker installed via manual download on Ubuntu family")
        
        # Reset execution log and test with unsupported OS
        execution_log.clear()
        
        with patch.dict(os.environ, {"OVERRIDE_OS": "freebsd"}):
            result = install_docker()
            
            # On unsupported OS, should try "other" implementation
            self.assertEqual(execution_log, ["other_preferred"])
            self.assertIsInstance(result, Failure)
            # The error could be either the original exception or the "all implementations failed" message
            error_str = str(result.error)
            self.assertTrue(
                "No package manager found" in error_str or 
                "All implementations" in error_str,
                f"Expected error about package manager or all implementations, got: {error_str}"
            )


class TestRegistryInternals(unittest.TestCase):
    """Test internal registry functionality."""
    
    def setUp(self):
        """Create a fresh registry for testing."""
        self.registry = _ImplRegistry()
    
    def test_registry_registration(self):
        """Test direct registry registration."""
        def test_func():
            return "test"
        
        wrapper = self.registry.register(test_func, ["ubuntu"], "preferred")
        
        self.assertIn("test_impl_dispatcher.TestRegistryInternals.test_registry_registration.<locals>.test_func", self.registry._registry)
        implementations = self.registry._registry["test_impl_dispatcher.TestRegistryInternals.test_registry_registration.<locals>.test_func"]
        self.assertEqual(len(implementations), 1)
        self.assertEqual(implementations[0].os_list, ["ubuntu"])
        self.assertEqual(implementations[0].impl_type, "preferred")
    
    def test_wrapper_caching(self):
        """Test that wrappers are cached and reused."""
        def test_func():
            return "test"
        
        wrapper1 = self.registry.register(test_func, ["ubuntu"], "preferred")
        wrapper2 = self.registry.register(test_func, ["arch"], "fallback")
        
        # Should return the same wrapper function
        self.assertIs(wrapper1, wrapper2)
        
        # But should have registered both implementations
        implementations = self.registry._registry["test_impl_dispatcher.TestRegistryInternals.test_wrapper_caching.<locals>.test_func"]
        self.assertEqual(len(implementations), 2)


if __name__ == '__main__':
    # Run with verbose output
    unittest.main(verbosity=2) 