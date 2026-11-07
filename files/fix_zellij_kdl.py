#!/usr/bin/env python3
from __future__ import annotations

import argparse
import os
import re
import subprocess
import sys
import tempfile
from dataclasses import dataclass
from pathlib import Path
from typing import Callable, Optional


DEFAULT_CONFIG_PATH = Path.home() / ".config" / "zellij" / "config.kdl"
DEFAULT_TIMEOUT_SECONDS = 30
QUOTED_RE = re.compile(r'"((?:[^"\\]|\\.)*)"')


class ConfigEditError(RuntimeError):
    pass


@dataclass(frozen=True)
class Block:
    keyword: str
    keyword_pos: int
    open_brace: int
    close_brace: int
    depth: int


def run_command(
    args: list[str],
    timeout: int = DEFAULT_TIMEOUT_SECONDS,
    env: Optional[dict[str, str]] = None,
) -> subprocess.CompletedProcess[str]:
    try:
        return subprocess.run(
            args,
            capture_output=True,
            text=True,
            timeout=timeout,
            env=env,
            check=False,
        )
    except FileNotFoundError as exc:
        raise ConfigEditError(f"Command not found: {args[0]}") from exc
    except subprocess.TimeoutExpired as exc:
        raise ConfigEditError(f"Command timed out: {' '.join(args)}") from exc


def atomic_write(path: Path, content: str) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    fd, tmp_name = tempfile.mkstemp(
        prefix=f".{path.name}.", suffix=".tmp", dir=str(path.parent)
    )
    tmp_path = Path(tmp_name)
    try:
        with open(fd, "w", encoding="utf-8", newline="") as handle:
            handle.write(content)
            handle.flush()
            os.fsync(handle.fileno())
        tmp_path.replace(path)
    except Exception:
        if tmp_path.exists():
            tmp_path.unlink()
        raise


def ensure_config_exists(config_path: Path) -> bool:
    if config_path.exists():
        return False
    config_path.parent.mkdir(parents=True, exist_ok=True)
    result = run_command(["zellij", "setup", "--dump-config"], timeout=60)
    if result.returncode != 0:
        stderr = result.stderr.strip()
        raise ConfigEditError(
            "Failed to generate config with `zellij setup --dump-config`"
            + (f"\n{stderr}" if stderr else "")
        )
    if not result.stdout:
        raise ConfigEditError("`zellij setup --dump-config` returned empty output")
    atomic_write(config_path, result.stdout)
    return True


def scan_code_and_depth(text: str) -> tuple[list[bool], list[int]]:
    code_mask = [False] * len(text)
    depth_before = [0] * len(text)

    depth = 0
    in_string = False
    in_line_comment = False
    in_block_comment = False
    escaped = False

    i = 0
    while i < len(text):
        ch = text[i]
        depth_before[i] = depth

        if in_line_comment:
            if ch == "\n":
                in_line_comment = False
                code_mask[i] = True
            i += 1
            continue

        if in_block_comment:
            if ch == "*" and i + 1 < len(text) and text[i + 1] == "/":
                i += 2
                continue
            i += 1
            continue

        if in_string:
            if escaped:
                escaped = False
            elif ch == "\\":
                escaped = True
            elif ch == '"':
                in_string = False
            i += 1
            continue

        if ch == "/" and i + 1 < len(text) and text[i + 1] == "/":
            in_line_comment = True
            i += 1
            continue

        if ch == "/" and i + 1 < len(text) and text[i + 1] == "*":
            in_block_comment = True
            i += 1
            continue

        if ch == '"':
            in_string = True
            i += 1
            continue

        code_mask[i] = True
        if ch == "{":
            depth += 1
        elif ch == "}":
            depth = max(0, depth - 1)

        i += 1

    return code_mask, depth_before


def iter_line_ranges(text: str):
    start = 0
    while True:
        end = text.find("\n", start)
        if end == -1:
            yield start, len(text)
            break
        yield start, end
        start = end + 1


def line_start_index(text: str, index: int) -> int:
    return text.rfind("\n", 0, index) + 1


def line_indent(text: str, index: int) -> str:
    start = line_start_index(text, index)
    end = start
    while end < len(text) and text[end] in (" ", "\t"):
        end += 1
    return text[start:end]


def find_matching_brace(
    text: str, open_brace: int, code_mask: list[bool]
) -> Optional[int]:
    if open_brace < 0 or open_brace >= len(text) or text[open_brace] != "{":
        return None
    depth = 1
    i = open_brace + 1
    while i < len(text):
        if code_mask[i]:
            ch = text[i]
            if ch == "{":
                depth += 1
            elif ch == "}":
                depth -= 1
                if depth == 0:
                    return i
        i += 1
    return None


def find_next_open_brace_same_depth(
    text: str,
    code_mask: list[bool],
    depth_before: list[int],
    start: int,
    end: int,
    expected_depth: int,
) -> Optional[int]:
    i = start
    while i < end:
        if code_mask[i]:
            ch = text[i]
            if ch == "{" and depth_before[i] == expected_depth:
                return i
            if ch == ";" and depth_before[i] == expected_depth:
                return None
        i += 1
    return None


def find_blocks(
    text: str,
    code_mask: list[bool],
    depth_before: list[int],
    keyword: str,
    start: int,
    end: int,
    target_depth: Optional[int] = None,
    header_predicate: Optional[Callable[[str], bool]] = None,
) -> list[Block]:
    blocks: list[Block] = []
    pattern = re.compile(rf"\b{re.escape(keyword)}\b")
    for match in pattern.finditer(text, start, end):
        pos = match.start()
        if not code_mask[pos]:
            continue
        if target_depth is not None and depth_before[pos] != target_depth:
            continue

        open_brace = find_next_open_brace_same_depth(
            text,
            code_mask,
            depth_before,
            match.end(),
            end,
            depth_before[pos],
        )
        if open_brace is None:
            continue

        close_brace = find_matching_brace(text, open_brace, code_mask)
        if close_brace is None or close_brace >= end:
            continue

        header = text[pos:open_brace]
        if header_predicate is not None and not header_predicate(header):
            continue

        blocks.append(
            Block(
                keyword=keyword,
                keyword_pos=pos,
                open_brace=open_brace,
                close_brace=close_brace,
                depth=depth_before[pos],
            )
        )
    return blocks


def find_first_block(
    text: str,
    code_mask: list[bool],
    depth_before: list[int],
    keyword: str,
    start: int,
    end: int,
    target_depth: Optional[int] = None,
    header_predicate: Optional[Callable[[str], bool]] = None,
) -> Optional[Block]:
    blocks = find_blocks(
        text,
        code_mask,
        depth_before,
        keyword,
        start,
        end,
        target_depth=target_depth,
        header_predicate=header_predicate,
    )
    return blocks[0] if blocks else None


def extract_quoted_strings(text: str) -> list[str]:
    return [m.group(1) for m in QUOTED_RE.finditer(text)]


def block_has_args_normal_locked(header: str) -> bool:
    args = {value.lower() for value in extract_quoted_strings(header)}
    return "normal" in args and "locked" in args


def infer_child_indent(text: str, parent: Block) -> str:
    parent_indent = line_indent(text, parent.keyword_pos)
    body = text[parent.open_brace + 1 : parent.close_brace]
    for line in body.splitlines():
        stripped = line.strip()
        if not stripped or stripped.startswith("//"):
            continue
        leading = line[: len(line) - len(line.lstrip(" \t"))]
        if len(leading) > len(parent_indent):
            return leading
    return parent_indent + "    "


def infer_indent_unit(parent_indent: str, child_indent: str) -> str:
    if child_indent.startswith(parent_indent) and len(child_indent) > len(
        parent_indent
    ):
        return child_indent[len(parent_indent) :]
    return "    "


def insert_before_close_brace(text: str, close_brace: int, snippet: str) -> str:
    prefix = "" if close_brace == 0 or text[close_brace - 1] == "\n" else "\n"
    return text[:close_brace] + prefix + snippet + text[close_brace:]


def find_line_comment_start(line: str) -> int:
    in_string = False
    escaped = False
    i = 0
    while i < len(line):
        ch = line[i]
        if in_string:
            if escaped:
                escaped = False
            elif ch == "\\":
                escaped = True
            elif ch == '"':
                in_string = False
            i += 1
            continue
        if ch == '"':
            in_string = True
            i += 1
            continue
        if ch == "/" and i + 1 < len(line) and line[i + 1] == "/":
            return i
        i += 1
    return -1


def replace_default_mode_line(line: str) -> str:
    comment_start = find_line_comment_start(line)
    if comment_start == -1:
        code_part = line
        comment_part = ""
    else:
        code_part = line[:comment_start]
        comment_part = line[comment_start:]

    replaced, count = re.subn(
        r'(\bdefault_mode\s+")([^"\\]*(?:\\.[^"\\]*)*)(")',
        r"\1locked\3",
        code_part,
        count=1,
    )
    if count == 0:
        replaced, count = re.subn(
            r"\bdefault_mode\b[^\n]*",
            'default_mode "locked"',
            code_part,
            count=1,
        )
    if count == 0:
        indent = code_part[: len(code_part) - len(code_part.lstrip(" \t"))]
        replaced = f'{indent}default_mode "locked"'

    return replaced + comment_part


def first_code_line_start(text: str) -> int:
    code_mask, _ = scan_code_and_depth(text)
    for index, is_code in enumerate(code_mask):
        if is_code:
            return line_start_index(text, index)
    return len(text)


def ensure_default_mode_locked(text: str) -> str:
    code_mask, depth_before = scan_code_and_depth(text)
    target_lines: list[tuple[int, int]] = []
    for start, end in iter_line_ranges(text):
        line = text[start:end]
        match = re.search(r"\bdefault_mode\b", line)
        if not match:
            continue
        pos = start + match.start()
        if code_mask[pos] and depth_before[pos] == 0:
            target_lines.append((start, end))

    if target_lines:
        for start, end in reversed(target_lines):
            new_line = replace_default_mode_line(text[start:end])
            text = text[:start] + new_line + text[end:]
        return text

    insert_at = first_code_line_start(text)
    return text[:insert_at] + 'default_mode "locked"\n' + text[insert_at:]


def get_keybinds_block(text: str) -> Optional[Block]:
    code_mask, depth_before = scan_code_and_depth(text)
    return find_first_block(
        text,
        code_mask,
        depth_before,
        "keybinds",
        0,
        len(text),
        target_depth=0,
    )


def ensure_keybinds_block(text: str) -> str:
    if get_keybinds_block(text) is not None:
        return text

    if text and not text.endswith("\n"):
        text += "\n"
    snippet = (
        "keybinds {\n"
        "    locked {\n"
        '        bind "Ctrl b" { SwitchToMode "Tmux"; }\n'
        "    }\n"
        "    tmux {\n"
        '        bind "Ctrl b" { Write 2; SwitchToMode "Locked"; }\n'
        "    }\n"
        '    shared_except "normal" "locked" {\n'
        '        bind "Enter" "Esc" { SwitchToMode "Locked"; }\n'
        "    }\n"
        "}\n"
    )
    return text + snippet


def find_mode_block(text: str, keybinds: Block, mode_keyword: str) -> Optional[Block]:
    code_mask, depth_before = scan_code_and_depth(text)
    return find_first_block(
        text,
        code_mask,
        depth_before,
        mode_keyword,
        keybinds.open_brace + 1,
        keybinds.close_brace,
        target_depth=keybinds.depth + 1,
    )


def find_shared_except_normal_locked_block(
    text: str, keybinds: Block
) -> Optional[Block]:
    code_mask, depth_before = scan_code_and_depth(text)
    return find_first_block(
        text,
        code_mask,
        depth_before,
        "shared_except",
        keybinds.open_brace + 1,
        keybinds.close_brace,
        target_depth=keybinds.depth + 1,
        header_predicate=block_has_args_normal_locked,
    )


def find_bind_blocks(text: str, parent: Block) -> list[Block]:
    code_mask, depth_before = scan_code_and_depth(text)
    return find_blocks(
        text,
        code_mask,
        depth_before,
        "bind",
        parent.open_brace + 1,
        parent.close_brace,
        target_depth=parent.depth + 1,
    )


def bind_has_key_and_mode(text: str, bind_block: Block, key: str, mode: str) -> bool:
    header = text[bind_block.keyword_pos : bind_block.open_brace]
    keys = set(extract_quoted_strings(header))
    if key not in keys:
        return False
    actions = text[bind_block.open_brace + 1 : bind_block.close_brace]
    return (
        re.search(
            rf'\bSwitchToMode\s+"{re.escape(mode)}"', actions, flags=re.IGNORECASE
        )
        is not None
    )


def bind_has_keys_enter_esc_and_locked(text: str, bind_block: Block) -> bool:
    header = text[bind_block.keyword_pos : bind_block.open_brace]
    keys = set(extract_quoted_strings(header))
    if not {"Enter", "Esc"}.issubset(keys):
        return False
    actions = text[bind_block.open_brace + 1 : bind_block.close_brace]
    return (
        re.search(r'\bSwitchToMode\s+"Locked"', actions, flags=re.IGNORECASE)
        is not None
    )


def bind_has_key(text: str, bind_block: Block, key: str) -> bool:
    header = text[bind_block.keyword_pos : bind_block.open_brace]
    keys = set(extract_quoted_strings(header))
    return key in keys


def find_switch_mode_value_ranges(
    segment: str, mode: Optional[str] = None
) -> list[tuple[int, int]]:
    ranges: list[tuple[int, int]] = []
    i = 0
    in_string = False
    in_line_comment = False
    in_block_comment = False
    escaped = False

    while i < len(segment):
        ch = segment[i]

        if in_line_comment:
            if ch == "\n":
                in_line_comment = False
            i += 1
            continue

        if in_block_comment:
            if ch == "*" and i + 1 < len(segment) and segment[i + 1] == "/":
                i += 2
                continue
            i += 1
            continue

        if in_string:
            if escaped:
                escaped = False
            elif ch == "\\":
                escaped = True
            elif ch == '"':
                in_string = False
            i += 1
            continue

        if ch == "/" and i + 1 < len(segment) and segment[i + 1] == "/":
            in_line_comment = True
            i += 2
            continue

        if ch == "/" and i + 1 < len(segment) and segment[i + 1] == "*":
            in_block_comment = True
            i += 2
            continue

        if ch == '"':
            in_string = True
            i += 1
            continue

        if segment.startswith("SwitchToMode", i):
            j = i + len("SwitchToMode")
            while j < len(segment) and segment[j].isspace():
                j += 1
            if j < len(segment) and segment[j] == '"':
                k = j + 1
                esc = False
                while k < len(segment):
                    c = segment[k]
                    if esc:
                        esc = False
                    elif c == "\\":
                        esc = True
                    elif c == '"':
                        break
                    k += 1
                if k < len(segment):
                    value = segment[j + 1 : k]
                    if mode is None or value.lower() == mode.lower():
                        ranges.append((j + 1, k))
                    i = k + 1
                    continue

        i += 1

    return ranges


def replace_switch_mode_in_block_body(
    text: str, block: Block, from_mode: str, to_mode: str
) -> tuple[str, int]:
    start = block.open_brace + 1
    end = block.close_brace
    body = text[start:end]
    ranges = find_switch_mode_value_ranges(body, mode=from_mode)
    if not ranges:
        return text, 0
    updated_body = body
    for value_start, value_end in reversed(ranges):
        updated_body = updated_body[:value_start] + to_mode + updated_body[value_end:]
    updated = text[:start] + updated_body + text[end:]
    return updated, len(ranges)


def count_switch_mode_in_block_body(text: str, block: Block, mode: str) -> int:
    body = text[block.open_brace + 1 : block.close_brace]
    return len(find_switch_mode_value_ranges(body, mode=mode))


def ensure_locked_ctrl_b_to_tmux(text: str) -> str:
    keybinds = get_keybinds_block(text)
    if keybinds is None:
        raise ConfigEditError("keybinds block missing after ensure_keybinds_block")

    locked = find_mode_block(text, keybinds, "locked")
    if locked is None:
        mode_indent = infer_child_indent(text, keybinds)
        unit = infer_indent_unit(line_indent(text, keybinds.keyword_pos), mode_indent)
        bind_indent = mode_indent + unit
        snippet = (
            f"{mode_indent}locked {{\n"
            f'{bind_indent}bind "Ctrl b" {{ SwitchToMode "Tmux"; }}\n'
            f"{mode_indent}}}\n"
        )
        return insert_before_close_brace(text, keybinds.close_brace, snippet)

    binds = find_bind_blocks(text, locked)
    if any(
        bind_has_key_and_mode(text, bind_block, "Ctrl b", "Tmux")
        for bind_block in binds
    ):
        return text

    bind_indent = infer_child_indent(text, locked)
    snippet = f'{bind_indent}bind "Ctrl b" {{ SwitchToMode "Tmux"; }}\n'
    return insert_before_close_brace(text, locked.close_brace, snippet)


def ensure_tmux_block_locked_returns(text: str) -> str:
    keybinds = get_keybinds_block(text)
    if keybinds is None:
        raise ConfigEditError("keybinds block missing after ensure_keybinds_block")

    tmux = find_mode_block(text, keybinds, "tmux")
    if tmux is None:
        mode_indent = infer_child_indent(text, keybinds)
        unit = infer_indent_unit(line_indent(text, keybinds.keyword_pos), mode_indent)
        bind_indent = mode_indent + unit
        snippet = (
            f"{mode_indent}tmux {{\n"
            f'{bind_indent}bind "Ctrl b" {{ Write 2; SwitchToMode "Locked"; }}\n'
            f"{mode_indent}}}\n"
        )
        text = insert_before_close_brace(text, keybinds.close_brace, snippet)
        keybinds = get_keybinds_block(text)
        if keybinds is None:
            raise ConfigEditError("Failed to create tmux block")
        tmux = find_mode_block(text, keybinds, "tmux")
        if tmux is None:
            raise ConfigEditError("Failed to locate tmux block after creation")

    text, _ = replace_switch_mode_in_block_body(
        text, tmux, from_mode="Normal", to_mode="Locked"
    )

    keybinds = get_keybinds_block(text)
    if keybinds is None:
        raise ConfigEditError("keybinds block missing after tmux edits")
    tmux = find_mode_block(text, keybinds, "tmux")
    if tmux is None:
        raise ConfigEditError("tmux block missing after tmux edits")

    binds = find_bind_blocks(text, tmux)
    if any(bind_has_key(text, bind_block, "Ctrl b") for bind_block in binds):
        return text

    bind_indent = infer_child_indent(text, tmux)
    snippet = f'{bind_indent}bind "Ctrl b" {{ Write 2; SwitchToMode "Locked"; }}\n'
    return insert_before_close_brace(text, tmux.close_brace, snippet)


def ensure_shared_except_normal_locked_returns(text: str) -> str:
    keybinds = get_keybinds_block(text)
    if keybinds is None:
        raise ConfigEditError("keybinds block missing after ensure_keybinds_block")

    shared = find_shared_except_normal_locked_block(text, keybinds)
    if shared is None:
        mode_indent = infer_child_indent(text, keybinds)
        unit = infer_indent_unit(line_indent(text, keybinds.keyword_pos), mode_indent)
        bind_indent = mode_indent + unit
        snippet = (
            f'{mode_indent}shared_except "normal" "locked" {{\n'
            f'{bind_indent}bind "Enter" "Esc" {{ SwitchToMode "Locked"; }}\n'
            f"{mode_indent}}}\n"
        )
        return insert_before_close_brace(text, keybinds.close_brace, snippet)

    text, _ = replace_switch_mode_in_block_body(
        text, shared, from_mode="Normal", to_mode="Locked"
    )

    keybinds = get_keybinds_block(text)
    if keybinds is None:
        raise ConfigEditError("keybinds block missing after shared edits")
    shared = find_shared_except_normal_locked_block(text, keybinds)
    if shared is None:
        raise ConfigEditError(
            'shared_except "normal" "locked" block missing after shared edits'
        )

    binds = find_bind_blocks(text, shared)
    if any(
        bind_has_keys_enter_esc_and_locked(text, bind_block) for bind_block in binds
    ):
        return text

    bind_indent = infer_child_indent(text, shared)
    snippet = f'{bind_indent}bind "Enter" "Esc" {{ SwitchToMode "Locked"; }}\n'
    return insert_before_close_brace(text, shared.close_brace, snippet)


def has_top_level_default_mode_locked(text: str) -> bool:
    code_mask, depth_before = scan_code_and_depth(text)
    for start, end in iter_line_ranges(text):
        line = text[start:end]
        match = re.search(r"\bdefault_mode\b", line)
        if not match:
            continue
        pos = start + match.start()
        if not (code_mask[pos] and depth_before[pos] == 0):
            continue
        comment_start = find_line_comment_start(line)
        code_part = line if comment_start == -1 else line[:comment_start]
        if re.search(r'\bdefault_mode\s+"locked"', code_part):
            return True
    return False


def validate_result(text: str) -> list[str]:
    errors: list[str] = []
    if not has_top_level_default_mode_locked(text):
        errors.append('Missing top-level: default_mode "locked"')

    keybinds = get_keybinds_block(text)
    if keybinds is None:
        errors.append("Missing keybinds block")
        return errors

    locked = find_mode_block(text, keybinds, "locked")
    if locked is None:
        errors.append("Missing locked block in keybinds")
    else:
        locked_binds = find_bind_blocks(text, locked)
        if not any(
            bind_has_key_and_mode(text, bind_block, "Ctrl b", "Tmux")
            for bind_block in locked_binds
        ):
            errors.append(
                'locked block missing: bind "Ctrl b" { SwitchToMode "Tmux"; }'
            )

    tmux = find_mode_block(text, keybinds, "tmux")
    if tmux is None:
        errors.append("Missing tmux block in keybinds")
    else:
        if count_switch_mode_in_block_body(text, tmux, "Normal") > 0:
            errors.append('tmux block still contains SwitchToMode "Normal"')

    shared = find_shared_except_normal_locked_block(text, keybinds)
    if shared is None:
        errors.append('Missing shared_except "normal" "locked" block')
    else:
        shared_binds = find_bind_blocks(text, shared)
        if not any(
            bind_has_keys_enter_esc_and_locked(text, bind_block)
            for bind_block in shared_binds
        ):
            errors.append(
                'shared_except "normal" "locked" missing Enter/Esc -> Locked binding'
            )

    return errors


def transform_config(text: str) -> str:
    updated = text
    updated = ensure_default_mode_locked(updated)
    updated = ensure_keybinds_block(updated)
    updated = ensure_locked_ctrl_b_to_tmux(updated)
    updated = ensure_tmux_block_locked_returns(updated)
    updated = ensure_shared_except_normal_locked_returns(updated)

    errors = validate_result(updated)
    if errors:
        joined = "\n- " + "\n- ".join(errors)
        raise ConfigEditError(f"Post-edit validation failed:{joined}")
    return updated


def run_zellij_check(config_path: Path) -> None:
    env = dict(os.environ)
    env["ZELLIJ_CONFIG_FILE"] = str(config_path)
    result = run_command(["zellij", "setup", "--check"], timeout=60, env=env)
    if result.returncode != 0:
        stderr = result.stderr.strip()
        stdout = result.stdout.strip()
        detail_parts = [part for part in (stdout, stderr) if part]
        detail = "\n".join(detail_parts)
        raise ConfigEditError(
            "`zellij setup --check` failed" + (f"\n{detail}" if detail else "")
        )


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Safely patch Zellij config.kdl for locked/tmux workflow.",
    )
    parser.add_argument(
        "--config",
        default=str(DEFAULT_CONFIG_PATH),
        help="Path to config.kdl (default: ~/.config/zellij/config.kdl)",
    )
    parser.add_argument(
        "--skip-zellij-check",
        action="store_true",
        help="Skip `zellij setup --check`",
    )
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    config_path = Path(args.config).expanduser().resolve()

    generated = ensure_config_exists(config_path)
    original_text = config_path.read_text(encoding="utf-8")

    updated_text = transform_config(original_text)
    changed = updated_text != original_text

    if changed:
        atomic_write(config_path, updated_text)

    try:
        if not args.skip_zellij_check:
            run_zellij_check(config_path)
    except ConfigEditError:
        if changed:
            atomic_write(config_path, original_text)
        raise

    print(f"[OK] Config path: {config_path}")
    if generated:
        print("[OK] Generated missing config via `zellij setup --dump-config`")
    if changed:
        print("[OK] Applied locked/tmux keybind updates")
    else:
        print("[OK] Config already satisfied required state (idempotent)")
    if args.skip_zellij_check:
        print("[SKIP] zellij setup --check")
    else:
        print("[OK] zellij setup --check")
    print("[NOTE] default_mode changes apply to newly started/attached Zellij sessions")
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except ConfigEditError as exc:
        print(f"[ERROR] {exc}", file=sys.stderr)
        raise SystemExit(1)
