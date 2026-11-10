"""Protect affected-product routing and the mandatory aggregate result."""
import unittest
from ci_scope import select_products, aggregate_result, PRODUCTS, JOBS


class ScopeTests(unittest.TestCase):
    def test_workspace_harbor_inputs_run_python_and_preserve_local_coverage(self):
        for path in (
                "scripts/harbor/workspace_plan.py", "scripts/harbor/workspace_benchmark.py",
                "scripts/harbor/test_workspace_plan.py", "scripts/harbor/workspace-pilot-tasks.json",
                "scripts/harbor/workspace_monitor/app.js", "scripts/harbor/workspace_monitor.compose.yml",
                "scripts/harbor/workspace-benchmark.md", "scripts/harbor/workspace-monitor.md",
                "scripts/harbor/benchmark-transport.json", "scripts/harbor/full-tasks.json",
                "scripts\\harbor\\test_workspace_provisioner.py"):
            with self.subTest(path=path):
                selected = select_products([path])
                self.assertEqual({key for key, value in selected.items() if value},
                                 {"coreRust", "localNode", "workspacePython"})

    def test_legacy_harbor_runner_keeps_its_existing_product_selection(self):
        for path in ("scripts/harbor/runner.py", "scripts/harbor/test_runner.py", "scripts/harbor/run.ps1"):
            with self.subTest(path=path):
                selected = select_products([path])
                self.assertEqual({key for key, value in selected.items() if value}, {"coreRust", "localNode"})

    def test_shared_core_locks_and_unknown_inputs_expand_to_all_products(self):
        for path in ("packages/core/src/lib.rs", "Cargo.lock", "pnpm-lock.yaml", "pnpm-workspace.yaml", "uv.lock", "new-build-input"):
            self.assertTrue(all(select_products([path]).values()), path)

    def test_web_only_changes_keep_local_and_core_gates_out_of_scope(self):
        result = select_products(["packages/workspace/web/src/routes/App.jsx"])
        self.assertTrue(result["web"] and result["docker"])
        self.assertFalse(result["coreRust"] or result["localNode"] or result["workspaceRust"])

    def test_model_http_checks_both_rust_consumers_and_hosted_integration(self):
        selected = select_products(["packages/model-http/src/lib.rs"])
        self.assertEqual({key for key, value in selected.items() if value},
                         {"coreRust", "workspaceRust", "workspacePython", "docker"})

    def test_api_test_only_changes_use_the_python_gate(self):
        for path in ("packages/workspace/api/app_core/test_native_model_grant.py", "packages/workspace/api/app_core/tests.py"):
            selected = select_products([path])
            self.assertEqual({key for key, value in selected.items() if value}, {"workspacePython"})

    def test_api_and_desktop_changes_include_their_contract_consumers(self):
        api = select_products(["packages/workspace/api/app_core/http/messages.py"])
        self.assertTrue(api["workspacePython"] and api["workspaceRust"] and api["docker"])
        ui = select_products(["packages/desktop/src/renderer/components/chat/messageScroll.ts"])
        self.assertEqual({key for key, value in ui.items() if value}, {"localNode"})
        services = select_products(["packages/desktop/src/services/schedules.rs"])
        self.assertEqual({key for key, value in services.items() if value}, {"coreRust", "localNode"})

    def test_visualization_changes_check_both_consumers_and_web_image(self):
        selected = select_products(["packages/visuals/src/Renderer.tsx"])
        self.assertEqual({key for key, value in selected.items() if value},
                         {"localNode", "web", "docker"})

    def test_shared_ui_checks_both_consumers_and_web_image(self):
        for path in ("packages/ui/src/theme-store.ts",
                     "packages/ui/src/components/dialog.tsx"):
            selected = select_products([path])
            self.assertEqual({key for key, value in selected.items() if value},
                             {"localNode", "web", "docker"})

    def test_documentation_can_return_a_result_without_product_builds(self):
        self.assertFalse(any(select_products(["docs/workspace/Product.md"]).values()))

    def test_protocol_documentation_runs_its_rust_contract_checks(self):
        for path in ("docs/reference/RuntimeProtocol.md", "docs/reference/SessionEvents.md"):
            with self.subTest(path=path):
                selected = select_products([path])
                self.assertTrue(selected["coreRust"])
                self.assertEqual({key for key, value in selected.items() if value}, {"coreRust"})
                needs = {"scope": {"result": "success", "outputs": {
                    key: str(value).lower() for key, value in selected.items()}}}
                needs.update({job: {"result": "skipped"} for job in JOBS.values()})
                needs["rust-format"] = {"result": "success"}
                self.assertFalse(aggregate_result(needs))
                needs["core-rust"]["result"] = "success"
                self.assertTrue(aggregate_result(needs))

    def test_selected_skipped_failed_cancelled_and_missing_jobs_fail_the_aggregate(self):
        selected = dict.fromkeys(PRODUCTS, False)
        selected["web"] = True
        needs = {"scope": {"result": "success", "outputs": {key: str(value).lower() for key, value in selected.items()}}}
        needs.update({job: {"result": "success" if selected[product] else "skipped"} for product, job in JOBS.items()})
        needs["rust-format"] = {"result": "skipped"}
        self.assertTrue(aggregate_result(needs))
        for result in ("skipped", "failure", "cancelled"):
            altered = {**needs, "web": {"result": result}}
            self.assertFalse(aggregate_result(altered))
        self.assertFalse(aggregate_result({key: value for key, value in needs.items() if key != "web"}))
        self.assertFalse(aggregate_result({**needs, "scope": {"result": "failure"}}))

    def test_rust_format_is_required_when_either_rust_product_is_selected(self):
        for product in ("coreRust", "workspaceRust"):
            with self.subTest(product=product):
                selected = dict.fromkeys(PRODUCTS, "false")
                selected[product] = "true"
                needs = {"scope": {"result": "success", "outputs": selected}}
                needs.update({job: {"result": "success" if key == product else "skipped"}
                              for key, job in JOBS.items()})
                self.assertFalse(aggregate_result(needs))
                for result in ("skipped", "failure", "cancelled"):
                    self.assertFalse(aggregate_result({**needs, "rust-format": {"result": result}}))
                self.assertTrue(aggregate_result({**needs, "rust-format": {"result": "success"}}))


if __name__ == "__main__":
    unittest.main()
