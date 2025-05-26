#!/usr/bin/env python3
"""
Test script to verify the new modular CLI structure works correctly using unittest
"""

import unittest
import os
import sys

# Add the parent directory to sys.path so we can import envira
sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))


class TestModularCLI(unittest.TestCase):
    """Test suite for the modular CLI structure."""
    
    def test_imports(self):
        """Test that all modules can be imported correctly"""
        try:
            # Test main package import
            from envira.cli import main, SoftwareInstaller, InstallationStep
            
            # Test individual module imports
            from envira.cli.models import InstallationStep
            from envira.cli.utils import is_running_as_sudo, detect_privilege_level
            from envira.cli.ui import show_software_table
            from envira.cli.steps import prepare_installation_steps
            from envira.cli.runner import run_installation
            from envira.cli.installer import SoftwareInstaller
            
            # Test that the installer can be created
            installer = SoftwareInstaller()
            self.assertIsInstance(installer, SoftwareInstaller)
            
            # Test that privilege detection works
            privilege = detect_privilege_level()
            self.assertIsInstance(privilege, str)
            self.assertIn(privilege, ["root", "sudo", "user (user scope only)"])
            
        except Exception as e:
            self.fail(f"Import test failed: {e}")
    
    def test_structure(self):
        """Test that the modular structure is properly organized"""
        # Get the project root directory
        project_root = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
        cli_dir = os.path.join(project_root, "envira", "cli")
        
        expected_files = [
            "__init__.py",
            "models.py", 
            "utils.py",
            "ui.py",
            "steps.py",
            "runner.py",
            "installer.py",
            "README.md"
        ]
        
        missing_files = []
        for file in expected_files:
            file_path = os.path.join(cli_dir, file)
            if not os.path.exists(file_path):
                missing_files.append(file)
        
        self.assertEqual(missing_files, [], f"Missing files: {missing_files}")
    
    def test_main_entry_point(self):
        """Test that the main entry point exists"""
        project_root = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
        main_file = os.path.join(project_root, "envira", "__main__.py")
        
        self.assertTrue(os.path.exists(main_file), "__main__.py entry point is missing")
    
    def test_software_registry_access(self):
        """Test that the software registry can be accessed"""
        try:
            from envira.software import SOFTWARE_REGISTRY
            self.assertIsInstance(SOFTWARE_REGISTRY, dict)
            self.assertGreater(len(SOFTWARE_REGISTRY), 0, "Software registry should not be empty")
        except Exception as e:
            self.fail(f"Software registry access failed: {e}")
    
    def test_installation_step_model(self):
        """Test that InstallationStep model works correctly"""
        try:
            from envira.cli.models import InstallationStep
            from envira.software import SOFTWARE_REGISTRY
            
            # Get the first software from registry for testing
            software_name = next(iter(SOFTWARE_REGISTRY.keys()))
            software = SOFTWARE_REGISTRY[software_name]
            
            # Create an installation step
            step = InstallationStep(
                software=software,
                scope="user",
                status="pending"
            )
            
            self.assertEqual(step.software, software)
            self.assertEqual(step.scope, "user")
            self.assertEqual(step.status, "pending")
            self.assertIsNone(step.result)
            self.assertEqual(step.log_output, [])
            
        except Exception as e:
            self.fail(f"InstallationStep model test failed: {e}")
    
    def test_privilege_detection_functions(self):
        """Test privilege detection utility functions"""
        try:
            from envira.cli.utils import is_running_as_sudo, detect_privilege_level, get_real_user_info
            
            # Test sudo detection
            is_sudo = is_running_as_sudo()
            self.assertIsInstance(is_sudo, bool)
            
            # Test privilege level detection
            privilege = detect_privilege_level()
            self.assertIsInstance(privilege, str)
            
            # Test real user info (should not fail even if not running as sudo)
            user, home = get_real_user_info()
            self.assertIsInstance(user, str)
            self.assertIsInstance(home, str)
            self.assertGreater(len(user), 0)
            self.assertGreater(len(home), 0)
            
        except Exception as e:
            self.fail(f"Privilege detection test failed: {e}")
    
    def test_step_preparation(self):
        """Test that installation step preparation works"""
        try:
            from envira.cli.steps import prepare_installation_steps
            from envira.software import SOFTWARE_REGISTRY
            
            # Get a software that doesn't have complex dependencies
            test_software = None
            for name, software in SOFTWARE_REGISTRY.items():
                if not software.dependencies:  # Find software without dependencies
                    test_software = name
                    break
            
            if test_software:
                # Test step preparation
                steps = prepare_installation_steps([test_software], is_sudo=False)
                self.assertIsInstance(steps, list)
                if steps:  # Only check if we have steps
                    self.assertEqual(steps[0].software.name, test_software)
                    self.assertEqual(steps[0].scope, "user")
                    self.assertEqual(steps[0].status, "pending")
            
        except Exception as e:
            self.fail(f"Step preparation test failed: {e}")


class TestCLIEntryPoints(unittest.TestCase):
    """Test CLI entry points and backward compatibility."""
    
    def test_legacy_cli_exists(self):
        """Test that the legacy CLI file exists"""
        project_root = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
        legacy_cli = os.path.join(project_root, "cli.py")
        
        self.assertTrue(os.path.exists(legacy_cli), "Legacy cli.py should exist for backward compatibility")
    
    def test_package_main_exists(self):
        """Test that the package can be run as a module"""
        project_root = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
        main_file = os.path.join(project_root, "envira", "__main__.py")
        
        self.assertTrue(os.path.exists(main_file), "Package __main__.py should exist")
        
        # Test that it contains the correct import
        with open(main_file, 'r') as f:
            content = f.read()
            self.assertIn("from .cli import main", content)
            self.assertIn('python -m envira', content)


if __name__ == '__main__':
    # Run with verbose output
    unittest.main(verbosity=2) 