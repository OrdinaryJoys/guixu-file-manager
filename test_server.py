import tempfile
import unittest
from unittest import mock
from pathlib import Path

import server


class OrganizerCoreTests(unittest.TestCase):
    def setUp(self):
        # 系统临时目录在 macOS 常解析到 /private，属于服务主动拒绝的保护范围。
        self.temp = tempfile.TemporaryDirectory(dir=server.APP_DIR)
        self.root = Path(self.temp.name) / "downloads"
        self.root.mkdir()
        self.data = Path(self.temp.name) / "data"
        server.DATA_DIR = self.data
        server.DATABASE = self.data / "organizer.sqlite3"
        server.PLANS.clear()
        server.init_database()

    def tearDown(self):
        self.temp.cleanup()

    def plan(self, **extra):
        return server.make_plan({"path": str(self.root), "rule": "type", "recursive": False,
                                 "ignore_hidden": True, **extra})

    def test_native_folder_picker_returns_selected_path_and_handles_cancel(self):
        with mock.patch.object(server.sys, "platform", "darwin"), mock.patch.object(
            server.subprocess, "run", return_value=mock.Mock(returncode=0, stdout=f"{self.root}/\n", stderr="")
        ) as run:
            selected = server.pick_folder()
        self.assertEqual(selected, {"cancelled": False, "path": str(self.root)})
        self.assertEqual(run.call_args.args[0][:2], ["osascript", "-e"])

        with mock.patch.object(server.sys, "platform", "darwin"), mock.patch.object(
            server.subprocess, "run", return_value=mock.Mock(returncode=0, stdout="\n", stderr="")
        ):
            self.assertEqual(server.pick_folder(), {"cancelled": True, "path": None})

    def test_plan_execute_and_undo_preserves_all_files(self):
        (self.root / "report.pdf").write_text("one")
        (self.root / "photo.JPG").write_text("two")
        (self.root / "文档").mkdir()
        (self.root / "文档" / "report.pdf").write_text("existing")
        plan = self.plan()
        destinations = {item["name"]: item["category"] for item in plan["moves"]}
        self.assertEqual(destinations, {"report.pdf": "文档", "photo.JPG": "图片"})
        result = server.execute_plan(plan["plan_id"])
        self.assertEqual(result["moved"], 2)
        self.assertTrue((self.root / "文档" / "report (1).pdf").exists())
        self.assertTrue((self.root / "图片" / "photo.JPG").exists())
        undone = server.undo(result["transaction_id"])
        self.assertEqual(undone["reverted"], 2)
        self.assertTrue((self.root / "report.pdf").exists())
        self.assertTrue((self.root / "photo.JPG").exists())
        self.assertEqual(server.history()[0]["status"], "undone")

    def test_changed_file_invalidates_plan_before_any_move(self):
        first = self.root / "first.pdf"
        first.write_text("original")
        (self.root / "second.jpg").write_text("photo")
        plan = self.plan()
        first.write_text("changed-content")
        with self.assertRaisesRegex(server.OrganizerError, "文件已变化"):
            server.execute_plan(plan["plan_id"])
        self.assertTrue(first.exists())
        self.assertTrue((self.root / "second.jpg").exists())

    def test_recursive_scan_skips_hidden_symlinks_and_output_directories(self):
        (self.root / ".private").mkdir()
        (self.root / ".private" / "hidden.pdf").write_text("hidden")
        (self.root / "nested").mkdir()
        (self.root / "nested" / "deep.pdf").write_text("deep")
        (self.root / "文档").mkdir()
        (self.root / "文档" / "already.pdf").write_text("done")
        (self.root / "link.pdf").symlink_to(self.root / "nested" / "deep.pdf")
        plan = self.plan(recursive=True)
        self.assertEqual([item["name"] for item in plan["moves"]], ["deep.pdf"])

    def test_refuses_system_root_and_duplicate_undo(self):
        with self.assertRaises(server.OrganizerError):
            server.canonical_directory("/")
        (self.root / "one.pdf").write_text("x")
        result = server.execute_plan(self.plan()["plan_id"])
        server.undo(result["transaction_id"])
        with self.assertRaisesRegex(server.OrganizerError, "已撤销"):
            server.undo(result["transaction_id"])

    def test_recovery_marks_interrupted_move_without_moving_anything_else(self):
        source = self.root / "interrupted.pdf"
        source.write_text("important")
        plan_id = self.plan()["plan_id"]
        plan = server.PLANS[plan_id]
        operation_id = server.record_operation(plan)
        move = plan["moves"][0]
        # 模拟程序在完成文件移动后、更新 SQLite 项状态前被强制终止。
        server.move_without_overwrite(move["source"], move["target"])
        self.assertFalse(source.exists())
        self.assertTrue(move["target"].exists())
        server.recover_incomplete()
        self.assertEqual(server.history()[0]["status"], "completed")
        item = server.operation_rows(operation_id)[0]
        self.assertEqual(item["status"], "completed")

    def test_new_target_conflict_invalidates_plan_without_source_move(self):
        source = self.root / "report.pdf"
        source.write_text("source")
        plan = self.plan()
        (self.root / "文档").mkdir()
        (self.root / "文档" / "report.pdf").write_text("external")
        with self.assertRaisesRegex(server.OrganizerError, "目标已被占用"):
            server.execute_plan(plan["plan_id"])
        self.assertTrue(source.exists())
        self.assertEqual((self.root / "文档" / "report.pdf").read_text(), "external")

    def test_saved_rules_prioritize_and_render_safe_nested_destinations(self):
        low = server.save_rule({"name": "所有 PDF", "priority": 10,
                                "conditions": {"extensions": ["pdf"]},
                                "action": {"destination": "存档/{YYYY}/{MM}"}})
        high = server.save_rule({"name": "发票优先", "priority": 100,
                                 "conditions": {"extensions": [".pdf"], "name_contains": "发票"},
                                 "action": {"destination": "财务/{YYYY}/{MM}"}})
        (self.root / "六月发票.pdf").write_text("invoice")
        (self.root / "notes.pdf").write_text("notes")
        (self.root / "photo.jpg").write_text("photo")
        plan = server.make_plan({"path": str(self.root), "rule": "custom"})
        result = {item["name"]: item for item in plan["moves"]}
        self.assertEqual(set(result), {"六月发票.pdf", "notes.pdf"})
        self.assertTrue(result["六月发票.pdf"]["destination"].startswith("财务/"))
        self.assertTrue(result["notes.pdf"]["destination"].startswith("存档/"))
        self.assertEqual(result["六月发票.pdf"]["matched_rule"], high["name"])
        self.assertEqual(server.list_rules()[1]["id"], low["id"])

    def test_rule_rejects_path_escape_and_can_be_updated_deleted(self):
        with self.assertRaisesRegex(server.OrganizerError, "相对路径"):
            server.save_rule({"name": "危险", "conditions": {}, "action": {"destination": "../outside"}})
        rule = server.save_rule({"name": "图片", "conditions": {"extensions": ["jpg"]},
                                 "action": {"destination": "图片"}})
        updated = server.save_rule({"name": "照片", "enabled": False, "priority": 8,
                                    "conditions": {"extensions": ["jpeg"]}, "action": {"destination": "照片"}}, rule["id"])
        self.assertEqual(updated["name"], "照片")
        self.assertFalse(updated["enabled"])
        server.delete_rule(rule["id"])
        self.assertEqual(server.list_rules(), [])

    def test_analysis_finds_exact_duplicates_without_hashing_unique_files_twice(self):
        payload = b"same content" * 100
        (self.root / "copy-a.pdf").write_bytes(payload)
        (self.root / "copy-b.pdf").write_bytes(payload)
        (self.root / "unique.pdf").write_bytes(b"unique content")
        (self.root / "empty.txt").touch()
        (self.root / "wrong.jpg").write_bytes(b"%PDF-1.7 document")
        result = server.analyze({"path": str(self.root)})
        self.assertEqual(result["scanned"], 5)
        self.assertEqual(result["duplicates"][0]["files"], ["copy-a.pdf", "copy-b.pdf"])
        self.assertEqual(result["empty"], ["empty.txt"])
        self.assertEqual(result["suspicious"], [{"path": "wrong.jpg", "extension": "jpg", "detected": ["pdf"]}])
        # 第二次分析应直接复用 SQLite 中的完整哈希缓存。
        with mock.patch.object(server, "digest", wraps=server.digest) as hash_call:
            second = server.analyze({"path": str(self.root)})
        self.assertEqual(second["duplicates"][0]["id"], result["duplicates"][0]["id"])
        self.assertEqual(hash_call.call_count, 0)

    def test_watch_automation_baselines_existing_and_waits_for_new_file_stability(self):
        server.save_rule({"name": "照片", "conditions": {"extensions": ["jpg"]},
                          "action": {"destination": "照片"}})
        (self.root / "existing.jpg").write_text("old")
        automation = server.save_automation({"name": "下载监听", "path": str(self.root), "kind": "watch",
                                             "interval_seconds": 10, "mode": "preview"})
        self.assertEqual(server.run_automation(automation["id"])["status"], "baseline")
        (self.root / "new.jpg").write_text("new")
        self.assertEqual(server.run_automation(automation["id"])["status"], "idle")
        ready = server.run_automation(automation["id"])
        self.assertEqual((ready["status"], ready["matched"]), ("preview_ready", 1))
        applied = server.execute_plan(ready["plan_id"])
        self.assertEqual(applied["moved"], 1)
        recorded = server.automation_runs()[0]
        self.assertEqual((recorded["status"], recorded["moved"], recorded["plan_id"]), ("completed", 1, None))
        self.assertTrue((self.root / "existing.jpg").exists())
        self.assertTrue((self.root / "照片" / "new.jpg").exists())

    def test_scheduled_auto_mode_executes_only_after_explicit_configuration(self):
        server.save_rule({"name": "文档", "conditions": {"extensions": ["pdf"]},
                          "action": {"destination": "归档/{YYYY}"}})
        (self.root / "report.pdf").write_text("report")
        automation = server.save_automation({"name": "每日归档", "path": str(self.root), "kind": "schedule",
                                             "interval_seconds": 60, "mode": "auto"})
        result = server.run_automation(automation["id"])
        self.assertEqual((result["status"], result["moved"]), ("completed", 1))
        self.assertTrue(any((self.root / "归档").rglob("report.pdf")))
        server.delete_automation(automation["id"])
        self.assertEqual(server.list_automations(), [])

    def test_empty_watch_baseline_still_detects_first_later_file(self):
        server.save_rule({"name": "文本", "conditions": {"extensions": ["txt"]},
                          "action": {"destination": "文本"}})
        automation = server.save_automation({"name": "空目录监听", "path": str(self.root), "kind": "watch",
                                             "interval_seconds": 10, "mode": "preview"})
        self.assertEqual(server.run_automation(automation["id"])["status"], "baseline")
        (self.root / "first.txt").write_text("first")
        self.assertEqual(server.run_automation(automation["id"])["status"], "idle")
        ready = server.run_automation(automation["id"])
        self.assertEqual((ready["status"], ready["matched"]), ("preview_ready", 1))


if __name__ == "__main__":
    unittest.main(verbosity=2)
