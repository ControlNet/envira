"""
Data models for the CLI
"""

from dataclasses import dataclass
from typing import List, Optional

from ..software import Software
from ..util.result import Result


@dataclass
class InstallationStep:
    software: Software
    scope: str  # "sudo" or "user"
    status: str  # "pending", "running", "success", "failure", "skipped"
    result: Optional[Result] = None
    log_output: List[str] = None
    
    def __post_init__(self):
        if self.log_output is None:
            self.log_output = [] 