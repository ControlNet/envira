from __future__ import annotations
from typing import Set
from abc import ABC, abstractmethod

from ..util.result import Result


class Software(ABC):
    registry: dict[str, Software] = {}

    def __init__(self, name: str, dependencies: Set[str] = set()):
        self.name = name
        self.dependencies = dependencies
        self.registry[name] = self

    def is_dependency_satisfied(self) -> bool:
        for dependency in self.dependencies:
            if dependency not in Software.registry:
                return False
            installed_in_sudo = Software.registry[dependency].is_installed_sudo() or False
            installed_in_user = Software.registry[dependency].is_installed_user() or False
            if not installed_in_sudo and not installed_in_user:
                return False
        return True

    @abstractmethod
    def install_sudo(self) -> Result:
        pass

    @abstractmethod
    def install_user(self) -> Result:
        pass

    @abstractmethod
    def upgrade_sudo(self) -> Result:
        pass

    @abstractmethod
    def upgrade_user(self) -> Result:
        pass

    @abstractmethod
    def is_installed_sudo(self) -> bool | None:
        # None means not applicable. For example, oh-my-zsh is for user only, so it should return None in sudo context.
        pass

    @abstractmethod
    def is_installed_user(self) -> bool | None:
        # None means not applicable. For example, the docker is for sudo only, so it should return None in user context.
        pass
