#!/usr/bin/env python3
"""归序本地服务：安全的文件整理计划、事务执行与撤销。"""
from __future__ import annotations

import argparse
import json
import hashlib
from contextlib import contextmanager
import mimetypes
import os
import secrets
import shutil
import sqlite3
import subprocess
import sys
import threading
import time
import webbrowser
from datetime import datetime, timezone
from http import HTTPStatus
from http.server import SimpleHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from typing import Any
from urllib.parse import urlparse

HOST, PORT = "127.0.0.1", 8765
PLAN_TTL_SECONDS, MAX_FILES = 15 * 60, 10_000
APP_DIR = Path(__file__).resolve().parent
DATA_DIR = Path(os.environ.get("GUIXU_DATA_DIR", Path.home() / ".guixu")).expanduser()
DATABASE = DATA_DIR / "organizer.sqlite3"
LOCK = threading.RLock()
PLANS: dict[str, dict[str, Any]] = {}

GROUPS = {
    "图片": {"jpg", "jpeg", "png", "gif", "webp", "svg", "heic", "avif", "bmp", "tiff"},
    "文档": {"pdf", "doc", "docx", "xls", "xlsx", "ppt", "pptx", "txt", "md", "rtf", "pages", "numbers"},
    "音视频": {"mp3", "wav", "m4a", "flac", "aac", "mp4", "mov", "avi", "mkv", "webm"},
    "压缩包": {"zip", "rar", "7z", "tar", "gz", "bz2", "xz"},
    "代码": {"js", "ts", "html", "css", "json", "py", "java", "c", "cpp", "h", "go", "rs", "jsx", "tsx"},
}
RESERVED = set(GROUPS) | {"其他文件", ".guixu"}
SAFE_STATIC = {"/", "/index.html", "/style.css", "/enhancements.css", "/app.js", "/favicon.svg"}


class OrganizerError(Exception):
    pass


def pick_folder() -> dict[str, Any]:
    """通过操作系统目录选择器获取路径；取消操作不是错误。"""
    if sys.platform != "darwin":
        raise OrganizerError("当前版本暂只支持 macOS 系统文件夹选择器")
    script = """
try
    set selectedFolder to choose folder with prompt "选择需要整理的文件夹"
    return POSIX path of selectedFolder
on error number -128
    return ""
end try
"""
    try:
        result = subprocess.run(["osascript", "-e", script], capture_output=True, text=True, timeout=300, check=False)
    except (OSError, subprocess.TimeoutExpired) as exc:
        raise OrganizerError("无法打开系统文件夹选择器") from exc
    if result.returncode != 0:
        raise OrganizerError(result.stderr.strip() or "系统文件夹选择器运行失败")
    selected = result.stdout.strip()
    if not selected:
        return {"cancelled": True, "path": None}
    return {"cancelled": False, "path": str(canonical_directory(selected))}


def now() -> str:
    return datetime.now(timezone.utc).isoformat(timespec="seconds")


@contextmanager
def database():
    DATA_DIR.mkdir(mode=0o700, parents=True, exist_ok=True)
    conn = sqlite3.connect(DATABASE)
    conn.row_factory = sqlite3.Row
    conn.execute("PRAGMA foreign_keys = ON")
    try:
        yield conn
        conn.commit()
    except Exception:
        conn.rollback()
        raise
    finally:
        conn.close()


def init_database() -> None:
    with database() as conn:
        conn.executescript("""
        CREATE TABLE IF NOT EXISTS operations (
            id TEXT PRIMARY KEY, created_at TEXT NOT NULL, root_path TEXT NOT NULL,
            rule TEXT NOT NULL, status TEXT NOT NULL, completed_at TEXT, error TEXT
        );
        CREATE TABLE IF NOT EXISTS operation_items (
            operation_id TEXT NOT NULL REFERENCES operations(id) ON DELETE CASCADE,
            ordinal INTEGER NOT NULL, source_path TEXT NOT NULL, target_path TEXT NOT NULL,
            source_size INTEGER NOT NULL, source_mtime_ns INTEGER NOT NULL,
            status TEXT NOT NULL DEFAULT 'pending', error TEXT,
            PRIMARY KEY(operation_id, ordinal)
        );
        CREATE INDEX IF NOT EXISTS operation_items_operation ON operation_items(operation_id, status);
        CREATE TABLE IF NOT EXISTS rules (
            id TEXT PRIMARY KEY, name TEXT NOT NULL, enabled INTEGER NOT NULL DEFAULT 1,
            priority INTEGER NOT NULL DEFAULT 100, conditions_json TEXT NOT NULL,
            action_json TEXT NOT NULL, mode TEXT NOT NULL DEFAULT 'preview',
            created_at TEXT NOT NULL, updated_at TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS rules_priority ON rules(enabled, priority DESC, created_at ASC);
        CREATE TABLE IF NOT EXISTS file_cache (
            path TEXT PRIMARY KEY, size INTEGER NOT NULL, mtime_ns INTEGER NOT NULL,
            partial_hash TEXT, content_hash TEXT, last_seen TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS file_cache_snapshot ON file_cache(size, mtime_ns);
        CREATE TABLE IF NOT EXISTS automations (
            id TEXT PRIMARY KEY, name TEXT NOT NULL, root_path TEXT NOT NULL,
            kind TEXT NOT NULL, interval_seconds INTEGER NOT NULL, mode TEXT NOT NULL,
            recursive INTEGER NOT NULL DEFAULT 0, ignore_hidden INTEGER NOT NULL DEFAULT 1,
            enabled INTEGER NOT NULL DEFAULT 1, next_run REAL NOT NULL,
            last_run TEXT, last_status TEXT, created_at TEXT NOT NULL, updated_at TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS automation_runs (
            id TEXT PRIMARY KEY, automation_id TEXT NOT NULL REFERENCES automations(id) ON DELETE CASCADE,
            created_at TEXT NOT NULL, status TEXT NOT NULL, matched INTEGER NOT NULL DEFAULT 0,
            moved INTEGER NOT NULL DEFAULT 0, plan_id TEXT, detail TEXT
        );
        CREATE INDEX IF NOT EXISTS automation_runs_parent ON automation_runs(automation_id, created_at DESC);
        CREATE TABLE IF NOT EXISTS watch_state (
            automation_id TEXT NOT NULL REFERENCES automations(id) ON DELETE CASCADE,
            path TEXT NOT NULL, size INTEGER NOT NULL, mtime_ns INTEGER NOT NULL,
            stable_count INTEGER NOT NULL DEFAULT 0, handled INTEGER NOT NULL DEFAULT 0,
            PRIMARY KEY(automation_id, path)
        );
        """)


def contained(path: Path, root: Path) -> bool:
    try:
        path.resolve().relative_to(root)
        return True
    except ValueError:
        return False


def canonical_directory(value: str) -> Path:
    if not value or not isinstance(value, str):
        raise OrganizerError("请提供要整理的文件夹路径")
    try:
        path = Path(value).expanduser().resolve(strict=True)
    except OSError as exc:
        raise OrganizerError("无法访问该文件夹") from exc
    if not path.is_dir():
        raise OrganizerError("该路径不是文件夹")
    # 防止一次输入误把整个家目录、系统根或应用源码当成整理目标。
    denied = {Path("/").resolve(), Path.home().resolve(), APP_DIR}
    if path in denied:
        raise OrganizerError("为避免误操作，不能直接整理系统根目录、家目录或应用目录")
    protected = [Path(value).resolve() for value in ("/System", "/private", "/usr", "/bin", "/sbin", "/etc") if Path(value).exists()]
    if any(path == root or root in path.parents for root in protected):
        raise OrganizerError("该系统目录受保护，不能整理")
    return path


def hidden(path: Path) -> bool:
    return any(part.startswith(".") for part in path.parts)


def snapshot(path: Path) -> tuple[int, int]:
    stat = path.stat()
    return stat.st_size, stat.st_mtime_ns


def classify(path: Path, rule: str) -> str:
    if rule == "date":
        return datetime.fromtimestamp(path.stat().st_mtime).strftime("%Y-%m")
    if rule == "name":
        initial = path.name.lstrip().upper()[:1]
        return initial if "A" <= initial <= "Z" else "其他"
    suffix = path.suffix[1:].lower()
    for group, suffixes in GROUPS.items():
        if suffix in suffixes:
            return group
    mime, _ = mimetypes.guess_type(path.name)
    if mime and mime.startswith("image/"):
        return "图片"
    if mime and mime.startswith(("audio/", "video/")):
        return "音视频"
    return "其他文件"


def sanitize_rule(payload: dict[str, Any]) -> dict[str, Any]:
    """验证并标准化可保存的规则，不接受任意路径或可执行内容。"""
    name = str(payload.get("name", "")).strip()
    if not 1 <= len(name) <= 80:
        raise OrganizerError("规则名称需为 1–80 个字符")
    conditions = payload.get("conditions", {})
    action = payload.get("action", {})
    if not isinstance(conditions, dict) or not isinstance(action, dict):
        raise OrganizerError("规则条件或动作格式错误")
    extensions = conditions.get("extensions", [])
    if not isinstance(extensions, list) or any(not isinstance(item, str) for item in extensions):
        raise OrganizerError("扩展名条件格式错误")
    normalized_extensions = sorted({item.lower().lstrip(".") for item in extensions if item.strip()})
    if len(normalized_extensions) > 100 or any(not item.replace("+", "").replace("-", "").isalnum() for item in normalized_extensions):
        raise OrganizerError("扩展名条件不合法")
    name_contains = str(conditions.get("name_contains", "")).strip()
    if len(name_contains) > 120:
        raise OrganizerError("文件名条件过长")
    numbers = {}
    for key in ("min_size", "max_size", "older_than_days", "newer_than_days"):
        value = conditions.get(key)
        if value in (None, ""):
            continue
        try:
            number = int(value)
        except (TypeError, ValueError) as exc:
            raise OrganizerError(f"{key} 必须为非负整数") from exc
        if number < 0:
            raise OrganizerError(f"{key} 必须为非负整数")
        numbers[key] = number
    if numbers.get("min_size", 0) > numbers.get("max_size", float("inf")):
        raise OrganizerError("最小文件大小不能大于最大文件大小")
    destination = str(action.get("destination", "")).strip().strip("/")
    if not destination or len(destination) > 240:
        raise OrganizerError("请设置规则目标目录")
    segments = Path(destination).parts
    permitted_tokens = {"{category}", "{YYYY}", "{MM}", "{ext}", "{initial}"}
    if any(part in {".", "..", ""} or part.startswith("~") for part in segments):
        raise OrganizerError("目标目录不能包含相对路径")
    for token in (part for part in segments if "{" in part or "}" in part):
        if token not in permitted_tokens:
            raise OrganizerError("目标目录包含不支持的变量")
    mode = payload.get("mode", "preview")
    if mode not in {"preview", "auto"}:
        raise OrganizerError("规则运行模式不支持")
    return {"name": name, "enabled": bool(payload.get("enabled", True)),
            "priority": max(0, min(int(payload.get("priority", 100)), 9999)),
            "conditions": {"extensions": normalized_extensions, "name_contains": name_contains, **numbers},
            "action": {"destination": destination}, "mode": mode}


def rule_matches(path: Path, conditions: dict[str, Any], current_time: float) -> bool:
    suffix = path.suffix[1:].lower()
    if conditions.get("extensions") and suffix not in conditions["extensions"]:
        return False
    if conditions.get("name_contains") and conditions["name_contains"].casefold() not in path.name.casefold():
        return False
    size, mtime_ns = snapshot(path)
    if size < conditions.get("min_size", 0) or size > conditions.get("max_size", float("inf")):
        return False
    age_days = (current_time - mtime_ns / 1_000_000_000) / 86_400
    if age_days < conditions.get("older_than_days", 0):
        return False
    if "newer_than_days" in conditions and age_days > conditions["newer_than_days"]:
        return False
    return True


def render_destination(template: str, path: Path, category: str) -> str:
    moment = datetime.fromtimestamp(path.stat().st_mtime)
    initial = path.name.lstrip().upper()[:1]
    values = {"{category}": category, "{YYYY}": moment.strftime("%Y"), "{MM}": moment.strftime("%m"),
              "{ext}": path.suffix[1:].lower() or "无扩展名", "{initial}": initial if "A" <= initial <= "Z" else "其他"}
    result = template
    for token, value in values.items():
        result = result.replace(token, value)
    if any(part in {"", ".", ".."} for part in Path(result).parts):
        raise OrganizerError("规则生成了不安全的目标目录")
    return result


def list_rules() -> list[dict[str, Any]]:
    with database() as conn:
        rows = conn.execute("SELECT * FROM rules ORDER BY priority DESC, created_at ASC").fetchall()
    return [{"id": row["id"], "name": row["name"], "enabled": bool(row["enabled"]), "priority": row["priority"],
             "conditions": json.loads(row["conditions_json"]), "action": json.loads(row["action_json"]),
             "mode": row["mode"], "created_at": row["created_at"], "updated_at": row["updated_at"]} for row in rows]


def save_rule(payload: dict[str, Any], rule_id: str | None = None) -> dict[str, Any]:
    rule = sanitize_rule(payload)
    timestamp = now()
    rule_id = rule_id or secrets.token_urlsafe(10)
    with database() as conn:
        exists = conn.execute("SELECT 1 FROM rules WHERE id=?", (rule_id,)).fetchone()
        if exists:
            conn.execute("""UPDATE rules SET name=?,enabled=?,priority=?,conditions_json=?,action_json=?,mode=?,updated_at=? WHERE id=?""",
                         (rule["name"], int(rule["enabled"]), rule["priority"], json.dumps(rule["conditions"], ensure_ascii=False),
                          json.dumps(rule["action"], ensure_ascii=False), rule["mode"], timestamp, rule_id))
        else:
            conn.execute("""INSERT INTO rules(id,name,enabled,priority,conditions_json,action_json,mode,created_at,updated_at)
                          VALUES(?,?,?,?,?,?,?,?,?)""",
                         (rule_id, rule["name"], int(rule["enabled"]), rule["priority"], json.dumps(rule["conditions"], ensure_ascii=False),
                          json.dumps(rule["action"], ensure_ascii=False), rule["mode"], timestamp, timestamp))
    return next(item for item in list_rules() if item["id"] == rule_id)


def delete_rule(rule_id: str) -> None:
    with database() as conn:
        if conn.execute("DELETE FROM rules WHERE id=?", (rule_id,)).rowcount == 0:
            raise OrganizerError("找不到该规则")


def evaluate_rule(path: Path, rules: list[dict[str, Any]], current_time: float) -> tuple[str, str] | None:
    for rule in rules:
        if rule["enabled"] and rule_matches(path, rule["conditions"], current_time):
            category = classify(path, "type")
            return render_destination(rule["action"]["destination"], path, category), rule["name"]
    return None


def digest(path: Path, *, partial: bool) -> str:
    """读取有限小块或完整内容；完整哈希仅用于同大小、同预哈希候选。"""
    hasher = hashlib.blake2b(digest_size=32)
    with path.open("rb") as handle:
        if partial:
            first = handle.read(64 * 1024)
            hasher.update(first)
            if path.stat().st_size > len(first):
                handle.seek(max(0, path.stat().st_size - 64 * 1024))
                hasher.update(handle.read(64 * 1024))
        else:
            while block := handle.read(1024 * 1024):
                hasher.update(block)
    return hasher.hexdigest()


def cached_hash(path: Path, *, partial: bool) -> str:
    size, mtime_ns = snapshot(path)
    column = "partial_hash" if partial else "content_hash"
    with database() as conn:
        row = conn.execute(f"SELECT {column} FROM file_cache WHERE path=? AND size=? AND mtime_ns=?", (str(path), size, mtime_ns)).fetchone()
    if row and row[column]:
        return row[column]
    value = digest(path, partial=partial)
    with database() as conn:
        current = conn.execute("SELECT path FROM file_cache WHERE path=?", (str(path),)).fetchone()
        if current:
            conn.execute(f"UPDATE file_cache SET size=?,mtime_ns=?,{column}=?,last_seen=? WHERE path=?", (size, mtime_ns, value, now(), str(path)))
        else:
            conn.execute(f"INSERT INTO file_cache(path,size,mtime_ns,{column},last_seen) VALUES(?,?,?,?,?)", (str(path), size, mtime_ns, value, now()))
    return value


MAGIC_PREFIXES = {
    b"%PDF-": {"pdf"}, b"\x89PNG\r\n\x1a\n": {"png"}, b"\xff\xd8\xff": {"jpg", "jpeg"},
    b"PK\x03\x04": {"zip", "docx", "xlsx", "pptx", "jar"}, b"Rar!\x1a\x07": {"rar"}, b"7z\xbc\xaf'\x1c": {"7z"},
}


def detected_extensions(path: Path) -> set[str]:
    try:
        with path.open("rb") as handle:
            header = handle.read(16)
    except OSError:
        return set()
    return next((extensions for prefix, extensions in MAGIC_PREFIXES.items() if header.startswith(prefix)), set())


def analyze(payload: dict[str, Any]) -> dict[str, Any]:
    root = canonical_directory(payload.get("path", ""))
    files = collect(root, bool(payload.get("recursive", False)), bool(payload.get("ignore_hidden", True)))
    by_size: dict[int, list[Path]] = {}
    empty, suspicious = [], []
    for file in files:
        size, _ = snapshot(file)
        if size == 0:
            empty.append(str(file.relative_to(root)))
        else:
            by_size.setdefault(size, []).append(file)
        actual = detected_extensions(file)
        extension = file.suffix[1:].lower()
        if actual and extension not in actual:
            suspicious.append({"path": str(file.relative_to(root)), "extension": extension or "无", "detected": sorted(actual)})
    duplicates = []
    for size, same_size in by_size.items():
        if len(same_size) < 2:
            continue
        partial_groups: dict[str, list[Path]] = {}
        for file in same_size:
            partial_groups.setdefault(cached_hash(file, partial=True), []).append(file)
        for candidates in partial_groups.values():
            if len(candidates) < 2:
                continue
            full_groups: dict[str, list[Path]] = {}
            for file in candidates:
                full_groups.setdefault(cached_hash(file, partial=False), []).append(file)
            for content_hash, identical in full_groups.items():
                if len(identical) > 1:
                    duplicates.append({"id": content_hash[:12], "size": size, "reclaimable": size * (len(identical) - 1),
                                       "files": [str(file.relative_to(root)) for file in identical]})
    largest = sorted(files, key=lambda file: snapshot(file)[0], reverse=True)[:20]
    return {"root": str(root), "scanned": len(files), "duplicates": duplicates, "empty": empty,
            "suspicious": suspicious, "largest": [{"path": str(file.relative_to(root)), "size": snapshot(file)[0]} for file in largest]}


def collect(root: Path, recursive: bool, ignore_hidden: bool) -> list[Path]:
    """迭代扫描并剪枝；不跟随符号链接，也不进入分类输出目录。"""
    result, pending = [], [root]
    while pending:
        directory = pending.pop()
        try:
            children = list(directory.iterdir())
        except OSError as exc:
            raise OrganizerError(f"无法读取目录：{directory.name}") from exc
        for item in children:
            relative = item.relative_to(root)
            if (ignore_hidden and hidden(relative)) or item.is_symlink():
                continue
            if item.is_dir():
                if recursive and relative.parts[0] not in RESERVED:
                    pending.append(item)
                continue
            if not item.is_file():
                continue
            result.append(item)
            if len(result) > MAX_FILES:
                raise OrganizerError(f"文件数量超过安全上限（{MAX_FILES}），请缩小范围")
    return result


def normal_key(path: Path) -> str:
    # 为 macOS/Windows 的大小写不敏感目标卷保守地预留名称。
    return os.path.normcase(str(path)).casefold()


def safe_destination(root: Path, category: str, name: str, reserved: set[str]) -> Path:
    relative = Path(category)
    if not category or relative.is_absolute() or any(part in {".", ".."} for part in relative.parts):
        raise OrganizerError("无效的分类目录")
    base = root / relative
    candidate = base / name
    stem, suffix = Path(name).stem, Path(name).suffix
    number = 1
    while candidate.exists() or normal_key(candidate) in reserved:
        candidate = base / f"{stem} ({number}){suffix}"
        number += 1
    reserved.add(normal_key(candidate))
    return candidate


def cleanup_expired_plans() -> None:
    cutoff = time.time() - PLAN_TTL_SECONDS
    with LOCK:
        for plan_id in [key for key, plan in PLANS.items() if plan["created"] < cutoff]:
            PLANS.pop(plan_id, None)


def make_plan(payload: dict[str, Any], only_paths: set[str] | None = None) -> dict[str, Any]:
    root = canonical_directory(payload.get("path", ""))
    rule = payload.get("rule", "type")
    if rule not in {"type", "date", "name", "custom"}:
        raise OrganizerError("不支持的整理规则")
    files = collect(root, bool(payload.get("recursive", False)), bool(payload.get("ignore_hidden", True)))
    if only_paths is not None:
        files = [file for file in files if str(file) in only_paths]
    reserved: set[str] = set()
    moves = []
    saved_rules = list_rules() if rule == "custom" else []
    current_time = time.time()
    for source in sorted(files, key=lambda item: (item.name.casefold(), str(item))):
        matched_rule = None
        if rule == "custom":
            evaluation = evaluate_rule(source, saved_rules, current_time)
            if not evaluation:
                continue
            destination, matched_rule = evaluation
        else:
            destination = classify(source, rule)
        target = safe_destination(root, destination, source.name, reserved)
        size, mtime_ns = snapshot(source)
        moves.append({"source": source, "target": target, "name": source.name, "category": target.parent.name,
                      "destination": destination, "matched_rule": matched_rule,
                      "size": size, "modified": mtime_ns / 1_000_000_000, "snapshot": (size, mtime_ns)})
    plan_id = secrets.token_urlsafe(24)
    with LOCK:
        cleanup_expired_plans()
        PLANS[plan_id] = {"created": time.time(), "root": root, "rule": rule, "moves": moves}
    public_moves = [{key: value for key, value in move.items() if key not in {"source", "target", "snapshot"}} for move in moves]
    return {"plan_id": plan_id, "root": str(root), "file_count": len(files), "moves": public_moves}


def operation_rows(operation_id: str) -> list[sqlite3.Row]:
    with database() as conn:
        return conn.execute("SELECT * FROM operation_items WHERE operation_id=? ORDER BY ordinal", (operation_id,)).fetchall()


def record_operation(plan: dict[str, Any]) -> str:
    operation_id = secrets.token_urlsafe(12)
    with database() as conn:
        conn.execute("INSERT INTO operations(id,created_at,root_path,rule,status) VALUES(?,?,?,?,?)",
                     (operation_id, now(), str(plan["root"]), plan["rule"], "in_progress"))
        conn.executemany("""INSERT INTO operation_items(operation_id,ordinal,source_path,target_path,source_size,source_mtime_ns,status)
                          VALUES(?,?,?,?,?,?, 'pending')""",
                         [(operation_id, index, str(move["source"]), str(move["target"]), *move["snapshot"])
                          for index, move in enumerate(plan["moves"])])
    return operation_id


def update_item(operation_id: str, ordinal: int, status: str, error: str | None = None) -> None:
    with database() as conn:
        conn.execute("UPDATE operation_items SET status=?, error=? WHERE operation_id=? AND ordinal=?",
                     (status, error, operation_id, ordinal))


def move_without_overwrite(source: Path, target: Path) -> None:
    """同盘用硬链接 + unlink 保证不覆盖；跨盘用临时副本并校验。"""
    target.parent.mkdir(parents=True, exist_ok=True)
    if target.exists():
        raise FileExistsError("目标路径已存在，计划失效")
    try:
        os.link(source, target)  # O_EXCL 语义：目标若出现，绝不会覆盖。
        source.unlink()
        return
    except OSError as exc:
        if target.exists():
            raise FileExistsError("目标路径已存在，计划失效") from exc
        # 常见于跨卷；先复制到同目录临时文件，确认完整后再替换临时名。
        temporary = target.with_name(f".{target.name}.guixu-{secrets.token_hex(6)}.tmp")
        try:
            shutil.copy2(source, temporary)
            if snapshot(source)[0] != snapshot(temporary)[0]:
                raise OSError("复制校验失败")
            with temporary.open("rb") as handle:
                os.fsync(handle.fileno())
            if target.exists():
                raise FileExistsError("目标路径已存在，计划失效")
            os.rename(temporary, target)
            source.unlink()
        except Exception:
            temporary.unlink(missing_ok=True)
            raise


def execute_plan(plan_id: str) -> dict[str, Any]:
    with LOCK:
        plan = PLANS.pop(plan_id, None)
    if not plan or time.time() - plan["created"] > PLAN_TTL_SECONDS:
        raise OrganizerError("预览已过期，请重新扫描")
    root = plan["root"]
    # 所有源文件都必须仍处于授权根目录且快照完全一致，才开始写入事务日志。
    for move in plan["moves"]:
        source, target = move["source"], move["target"]
        if not contained(source, root) or not contained(target.parent, root) or not source.exists() or snapshot(source) != move["snapshot"]:
            raise OrganizerError(f"文件已变化：{source.name}，请重新扫描")
        if target.exists():
            raise OrganizerError(f"目标已被占用：{target.name}，请重新扫描")
    operation_id = record_operation(plan)
    failed = []
    for ordinal, move in enumerate(plan["moves"]):
        try:
            move_without_overwrite(move["source"], move["target"])
            update_item(operation_id, ordinal, "completed")
        except OSError as exc:
            update_item(operation_id, ordinal, "failed", str(exc))
            failed.append({"name": move["name"], "reason": str(exc)})
    completed = len(plan["moves"]) - len(failed)
    status = "completed" if not failed else ("failed" if not completed else "partial")
    with database() as conn:
        conn.execute("UPDATE operations SET status=?, completed_at=? WHERE id=?", (status, now(), operation_id))
        preview_run = conn.execute("SELECT automation_id FROM automation_runs WHERE plan_id=? AND status='preview_ready'",
                                   (plan_id,)).fetchone()
        if preview_run:
            detail = json.dumps(failed, ensure_ascii=False) if failed else None
            conn.execute("UPDATE automation_runs SET status=?, moved=?, plan_id=NULL, detail=? WHERE plan_id=?",
                         (status, completed, detail, plan_id))
            conn.execute("UPDATE automations SET last_status=? WHERE id=?", (status, preview_run["automation_id"]))
    return {"transaction_id": operation_id, "moved": completed, "failed": failed}


def undo(operation_id: str) -> dict[str, Any]:
    with database() as conn:
        operation = conn.execute("SELECT status FROM operations WHERE id=?", (operation_id,)).fetchone()
    if not operation:
        raise OrganizerError("找不到该操作记录")
    if operation["status"] == "undone":
        raise OrganizerError("该操作已撤销")
    reverted, failed = 0, []
    for item in reversed(operation_rows(operation_id)):
        if item["status"] != "completed":
            continue
        source, target = Path(item["source_path"]), Path(item["target_path"])
        try:
            if source.exists() or not target.exists():
                raise OSError("文件已被外部修改，无法安全撤销")
            source.parent.mkdir(parents=True, exist_ok=True)
            move_without_overwrite(target, source)
            update_item(operation_id, item["ordinal"], "reverted")
            reverted += 1
        except OSError as exc:
            failed.append({"name": target.name, "reason": str(exc)})
    with database() as conn:
        status = "undone" if not failed else "undo_partial"
        conn.execute("UPDATE operations SET status=?, completed_at=? WHERE id=?", (status, now(), operation_id))
    return {"reverted": reverted, "failed": failed}


def recover_incomplete() -> None:
    """启动时只标记可判定结果，绝不擅自移动文件。"""
    with database() as conn:
        operations = conn.execute("SELECT id FROM operations WHERE status='in_progress'").fetchall()
    for operation in operations:
        items = operation_rows(operation["id"])
        unresolved = False
        for item in items:
            source, target = Path(item["source_path"]), Path(item["target_path"])
            if target.exists() and not source.exists():
                update_item(operation["id"], item["ordinal"], "completed")
            elif source.exists() and not target.exists():
                update_item(operation["id"], item["ordinal"], "pending")
                unresolved = True
            else:
                update_item(operation["id"], item["ordinal"], "recovery_needed", "状态无法自动判断")
                unresolved = True
        with database() as conn:
            conn.execute("UPDATE operations SET status=?, completed_at=? WHERE id=?",
                         ("recovery_needed" if unresolved else "completed", now(), operation["id"]))


def history() -> list[dict[str, Any]]:
    with database() as conn:
        operations = conn.execute("SELECT * FROM operations ORDER BY created_at DESC LIMIT 30").fetchall()
    result = []
    for operation in operations:
        items = operation_rows(operation["id"])
        result.append({"id": operation["id"], "at": operation["created_at"], "status": operation["status"],
                       "root": operation["root_path"], "moved": [{"from": row["source_path"], "to": row["target_path"]}
                       for row in items if row["status"] in {"completed", "reverted"}]})
    return result


def automation_dict(row: sqlite3.Row) -> dict[str, Any]:
    return {"id": row["id"], "name": row["name"], "path": row["root_path"], "kind": row["kind"],
            "interval_seconds": row["interval_seconds"], "mode": row["mode"], "recursive": bool(row["recursive"]),
            "ignore_hidden": bool(row["ignore_hidden"]), "enabled": bool(row["enabled"]), "next_run": row["next_run"],
            "last_run": row["last_run"], "last_status": row["last_status"]}


def list_automations() -> list[dict[str, Any]]:
    with database() as conn:
        rows = conn.execute("SELECT * FROM automations ORDER BY created_at ASC").fetchall()
    return [automation_dict(row) for row in rows]


def get_automation(automation_id: str) -> dict[str, Any]:
    with database() as conn:
        row = conn.execute("SELECT * FROM automations WHERE id=?", (automation_id,)).fetchone()
    if not row:
        raise OrganizerError("找不到该自动化任务")
    return automation_dict(row)


def save_automation(payload: dict[str, Any], automation_id: str | None = None) -> dict[str, Any]:
    name = str(payload.get("name", "")).strip()
    if not 1 <= len(name) <= 80:
        raise OrganizerError("任务名称需为 1–80 个字符")
    root = canonical_directory(payload.get("path", ""))
    kind = payload.get("kind", "watch")
    mode = payload.get("mode", "preview")
    if kind not in {"watch", "schedule"} or mode not in {"preview", "auto"}:
        raise OrganizerError("自动化类型或模式不支持")
    try:
        interval = int(payload.get("interval_seconds", 60))
    except (TypeError, ValueError) as exc:
        raise OrganizerError("检查间隔必须为整数") from exc
    minimum = 10 if kind == "watch" else 60
    if not minimum <= interval <= 2_592_000:
        raise OrganizerError(f"检查间隔需在 {minimum}–2592000 秒之间")
    timestamp, automation_id = now(), automation_id or secrets.token_urlsafe(10)
    with database() as conn:
        previous = conn.execute("SELECT root_path,kind FROM automations WHERE id=?", (automation_id,)).fetchone()
        values = (name, str(root), kind, interval, mode, int(bool(payload.get("recursive", False))),
                  int(bool(payload.get("ignore_hidden", True))), int(bool(payload.get("enabled", True))),
                  time.time() + interval, timestamp)
        if previous:
            conn.execute("""UPDATE automations SET name=?,root_path=?,kind=?,interval_seconds=?,mode=?,recursive=?,ignore_hidden=?,enabled=?,next_run=?,updated_at=? WHERE id=?""", (*values, automation_id))
            if previous["root_path"] != str(root) or previous["kind"] != kind:
                conn.execute("DELETE FROM watch_state WHERE automation_id=?", (automation_id,))
        else:
            conn.execute("""INSERT INTO automations(id,name,root_path,kind,interval_seconds,mode,recursive,ignore_hidden,enabled,next_run,created_at,updated_at)
                          VALUES(?,?,?,?,?,?,?,?,?,?,?,?)""", (automation_id, *values[:-1], timestamp, timestamp))
    return get_automation(automation_id)


def delete_automation(automation_id: str) -> None:
    with database() as conn:
        if conn.execute("DELETE FROM automations WHERE id=?", (automation_id,)).rowcount == 0:
            raise OrganizerError("找不到该自动化任务")


def watch_candidates(automation: dict[str, Any]) -> tuple[list[Path], bool]:
    root = canonical_directory(automation["path"])
    files = collect(root, automation["recursive"], automation["ignore_hidden"])
    current = {str(file): snapshot(file) for file in files}
    with database() as conn:
        previous_rows = conn.execute("SELECT * FROM watch_state WHERE automation_id=?", (automation["id"],)).fetchall()
        previous = {row["path"]: row for row in previous_rows}
        if not previous_rows and automation["last_run"] is None:
            conn.executemany("INSERT INTO watch_state(automation_id,path,size,mtime_ns,stable_count,handled) VALUES(?,?,?,?,0,1)",
                             [(automation["id"], path, snap[0], snap[1]) for path, snap in current.items()])
            return [], True
        candidates = []
        for path, snap in current.items():
            row = previous.get(path)
            if row is None:
                conn.execute("INSERT INTO watch_state(automation_id,path,size,mtime_ns,stable_count,handled) VALUES(?,?,?,?,0,0)",
                             (automation["id"], path, snap[0], snap[1]))
            elif (row["size"], row["mtime_ns"]) != snap:
                conn.execute("UPDATE watch_state SET size=?,mtime_ns=?,stable_count=0,handled=0 WHERE automation_id=? AND path=?",
                             (snap[0], snap[1], automation["id"], path))
            elif not row["handled"]:
                stable = row["stable_count"] + 1
                handled = int(stable >= 1)
                conn.execute("UPDATE watch_state SET stable_count=?,handled=? WHERE automation_id=? AND path=?",
                             (stable, handled, automation["id"], path))
                if handled:
                    candidates.append(Path(path))
        for stale in set(previous) - set(current):
            conn.execute("DELETE FROM watch_state WHERE automation_id=? AND path=?", (automation["id"], stale))
    return candidates, False


def record_automation_run(automation_id: str, status: str, matched: int, moved: int = 0,
                          plan_id: str | None = None, detail: str | None = None) -> dict[str, Any]:
    run_id, timestamp = secrets.token_urlsafe(10), now()
    with database() as conn:
        conn.execute("INSERT INTO automation_runs(id,automation_id,created_at,status,matched,moved,plan_id,detail) VALUES(?,?,?,?,?,?,?,?)",
                     (run_id, automation_id, timestamp, status, matched, moved, plan_id, detail))
        conn.execute("UPDATE automations SET last_run=?,last_status=? WHERE id=?", (timestamp, status, automation_id))
    return {"id": run_id, "automation_id": automation_id, "at": timestamp, "status": status,
            "matched": matched, "moved": moved, "plan_id": plan_id, "detail": detail}


def run_automation(automation_id: str) -> dict[str, Any]:
    automation = get_automation(automation_id)
    if not automation["enabled"]:
        raise OrganizerError("该自动化任务已停用")
    only_paths = None
    if automation["kind"] == "watch":
        candidates, baseline = watch_candidates(automation)
        if baseline:
            return record_automation_run(automation_id, "baseline", 0, detail="已建立初始快照，不处理现有文件")
        if not candidates:
            return record_automation_run(automation_id, "idle", 0)
        only_paths = {str(path) for path in candidates}
    plan = make_plan({"path": automation["path"], "rule": "custom", "recursive": automation["recursive"],
                      "ignore_hidden": automation["ignore_hidden"]}, only_paths=only_paths)
    matched = len(plan["moves"])
    if not matched:
        return record_automation_run(automation_id, "idle", 0)
    if automation["mode"] == "preview":
        return record_automation_run(automation_id, "preview_ready", matched, plan_id=plan["plan_id"])
    result = execute_plan(plan["plan_id"])
    status = "completed" if not result["failed"] else "partial"
    return record_automation_run(automation_id, status, matched, result["moved"], detail=json.dumps(result["failed"], ensure_ascii=False))


def automation_runs() -> list[dict[str, Any]]:
    with database() as conn:
        rows = conn.execute("SELECT * FROM automation_runs ORDER BY created_at DESC, rowid DESC LIMIT 50").fetchall()
    return [{"id": row["id"], "automation_id": row["automation_id"], "at": row["created_at"], "status": row["status"],
             "matched": row["matched"], "moved": row["moved"], "plan_id": row["plan_id"], "detail": row["detail"]} for row in rows]


def run_due_automations(current_time: float | None = None) -> int:
    current_time = current_time or time.time()
    with database() as conn:
        rows = conn.execute("SELECT id,interval_seconds FROM automations WHERE enabled=1 AND next_run<=?", (current_time,)).fetchall()
        for row in rows:
            # 先推进下次时间，避免长任务被下一个轮询线程重复领取。
            conn.execute("UPDATE automations SET next_run=? WHERE id=?", (current_time + row["interval_seconds"], row["id"]))
    for row in rows:
        try:
            run_automation(row["id"])
        except Exception as exc:
            record_automation_run(row["id"], "error", 0, detail=str(exc))
    return len(rows)


def automation_loop() -> None:
    while True:
        try:
            run_due_automations()
        except Exception as exc:
            print(f"[automation] {exc}")
        time.sleep(2)


class Handler(SimpleHTTPRequestHandler):
    """只提供白名单前端资源；不再把项目目录作为可浏览文件夹。"""
    def do_GET(self) -> None:
        path = urlparse(self.path).path
        if path == "/api/history":
            return self.respond(HTTPStatus.OK, {"items": history()})
        if path == "/api/health":
            return self.respond(HTTPStatus.OK, {"status": "ok", "version": 1})
        if path == "/api/rules":
            return self.respond(HTTPStatus.OK, {"items": list_rules()})
        if path == "/api/automations":
            return self.respond(HTTPStatus.OK, {"items": list_automations(), "runs": automation_runs()})
        if path not in SAFE_STATIC:
            return self.respond(HTTPStatus.NOT_FOUND, {"error": "资源不存在"})
        self.path = "/index.html" if path == "/" else path
        return super().do_GET()

    def do_POST(self) -> None:
        try:
            self.require_same_origin()
            length = int(self.headers.get("Content-Length", "0"))
            if not 0 <= length <= 1_000_000:
                raise OrganizerError("请求过大")
            data = json.loads(self.rfile.read(length) or b"{}")
            path = urlparse(self.path).path
            if path == "/api/pick-folder": result = pick_folder()
            elif path == "/api/plan": result = make_plan(data)
            elif path == "/api/execute": result = execute_plan(data.get("plan_id", ""))
            elif path == "/api/undo": result = undo(data.get("transaction_id", ""))
            elif path == "/api/rules": result = save_rule(data)
            elif path == "/api/analyze": result = analyze(data)
            elif path == "/api/automations": result = save_automation(data)
            elif path.startswith("/api/automations/") and path.endswith("/run"):
                result = run_automation(path.removeprefix("/api/automations/").removesuffix("/run"))
            else: return self.respond(HTTPStatus.NOT_FOUND, {"error": "接口不存在"})
            self.respond(HTTPStatus.OK, result)
        except OrganizerError as exc:
            self.respond(HTTPStatus.BAD_REQUEST, {"error": str(exc)})
        except (json.JSONDecodeError, ValueError):
            self.respond(HTTPStatus.BAD_REQUEST, {"error": "请求格式错误"})
        except Exception:
            self.respond(HTTPStatus.INTERNAL_SERVER_ERROR, {"error": "服务发生内部错误，请查看终端日志"})
            raise

    def do_PUT(self) -> None:
        try:
            self.require_same_origin()
            path = urlparse(self.path).path
            rule_prefix, automation_prefix = "/api/rules/", "/api/automations/"
            if not (path.startswith(rule_prefix) or path.startswith(automation_prefix)):
                return self.respond(HTTPStatus.NOT_FOUND, {"error": "接口不存在"})
            length = int(self.headers.get("Content-Length", "0"))
            data = json.loads(self.rfile.read(length) or b"{}")
            if path.startswith(rule_prefix):
                result = save_rule(data, path[len(rule_prefix):])
            else:
                result = save_automation(data, path[len(automation_prefix):])
            self.respond(HTTPStatus.OK, result)
        except OrganizerError as exc:
            self.respond(HTTPStatus.BAD_REQUEST, {"error": str(exc)})
        except (json.JSONDecodeError, ValueError):
            self.respond(HTTPStatus.BAD_REQUEST, {"error": "请求格式错误"})

    def do_DELETE(self) -> None:
        try:
            self.require_same_origin()
            path = urlparse(self.path).path
            rule_prefix, automation_prefix = "/api/rules/", "/api/automations/"
            if not (path.startswith(rule_prefix) or path.startswith(automation_prefix)):
                return self.respond(HTTPStatus.NOT_FOUND, {"error": "接口不存在"})
            if path.startswith(rule_prefix):
                delete_rule(path[len(rule_prefix):])
            else:
                delete_automation(path[len(automation_prefix):])
            self.respond(HTTPStatus.OK, {"deleted": True})
        except OrganizerError as exc:
            self.respond(HTTPStatus.BAD_REQUEST, {"error": str(exc)})

    def require_same_origin(self) -> None:
        origin = self.headers.get("Origin")
        host = self.headers.get("Host")
        if origin and urlparse(origin).netloc != host:
            raise OrganizerError("拒绝来自其他网站的本地操作请求")

    def respond(self, status: HTTPStatus, body: dict[str, Any]) -> None:
        raw = json.dumps(body, ensure_ascii=False).encode()
        self.send_response(status)
        self.send_header("Content-Type", "application/json; charset=utf-8")
        self.send_header("Content-Length", str(len(raw)))
        self.end_headers()
        self.wfile.write(raw)

    def log_message(self, fmt: str, *args: Any) -> None:
        print(f"[{self.log_date_time_string()}] {fmt % args}")


def serve(port: int = PORT, open_browser: bool = False) -> None:
    os.chdir(APP_DIR)
    init_database()
    recover_incomplete()
    threading.Thread(target=automation_loop, name="guixu-automation", daemon=True).start()
    url = f"http://{HOST}:{port}"
    print(f"归序已启动：{url}")
    if open_browser:
        threading.Timer(0.4, lambda: webbrowser.open(url)).start()
    try:
        ThreadingHTTPServer((HOST, port), Handler).serve_forever()
    except KeyboardInterrupt:
        print("\n归序已停止")


def cli() -> None:
    parser = argparse.ArgumentParser(prog="guixu", description="归序本地文件整理工具")
    subparsers = parser.add_subparsers(dest="command")
    serve_parser = subparsers.add_parser("serve", help="启动本地界面")
    serve_parser.add_argument("--port", type=int, default=PORT)
    serve_parser.add_argument("--open", action="store_true", help="自动打开浏览器")
    for name in ("plan", "analyze"):
        command = subparsers.add_parser(name, help="生成整理预览" if name == "plan" else "分析文件夹")
        command.add_argument("path")
        command.add_argument("--recursive", action="store_true")
        if name == "plan":
            command.add_argument("--rule", choices=["type", "date", "name", "custom"], default="type")
    apply_parser = subparsers.add_parser("apply", help="执行整理计划（需要 --yes）")
    apply_parser.add_argument("path")
    apply_parser.add_argument("--rule", choices=["type", "date", "name", "custom"], default="type")
    apply_parser.add_argument("--recursive", action="store_true")
    apply_parser.add_argument("--yes", action="store_true")
    undo_parser = subparsers.add_parser("undo", help="撤销一次操作")
    undo_parser.add_argument("transaction_id")
    subparsers.add_parser("rules", help="列出规则")
    subparsers.add_parser("automations", help="列出自动化任务")
    args = parser.parse_args()
    if args.command in {None, "serve"}:
        serve(getattr(args, "port", PORT), getattr(args, "open", False))
        return
    init_database()
    try:
        if args.command == "plan": result = make_plan({"path": args.path, "rule": args.rule, "recursive": args.recursive})
        elif args.command == "analyze": result = analyze({"path": args.path, "recursive": args.recursive})
        elif args.command == "apply":
            if not args.yes:
                raise OrganizerError("执行文件移动必须显式添加 --yes")
            plan = make_plan({"path": args.path, "rule": args.rule, "recursive": args.recursive})
            result = execute_plan(plan["plan_id"])
        elif args.command == "undo": result = undo(args.transaction_id)
        elif args.command == "rules": result = {"items": list_rules()}
        else: result = {"items": list_automations()}
        print(json.dumps(result, ensure_ascii=False, indent=2, default=str))
    except OrganizerError as exc:
        print(f"错误：{exc}", file=sys.stderr)
        raise SystemExit(2) from exc


if __name__ == "__main__":
    cli()
