from dataclasses import dataclass
from typing import Any


@dataclass
class Success:
    value: Any

@dataclass
class Failure:
    error: Any

@dataclass
class Skip:
    pass

type Result = Success | Failure | Skip
