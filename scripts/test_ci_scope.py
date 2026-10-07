"""Protect affected-product routing and the mandatory aggregate result."""
import unittest
from ci_scope import select_products, aggregate_result, PRODUCTS, JOBS


class ScopeTests(unittest.TestCase):
    def test_shared_core_locks_and_unknown_inputs_expand_to_all_products(self):
        for path in ("packages/core/src/lib.rs", "Cargo.lock", "package-lock.json", "uv.lock", "new-build-input"):
            self.assertTrue(all(select_products([path]).values()), path)

    def test_web_only_changes_keep_local_and_core_gates_out_of_scope(self):
        result = select_products(["packages/web/src/routes/App.jsx"])
        self.assertTrue(result["web"] and result["docker"])
        self.assertFalse(result["coreRust"] or result["localNode"] or result["workspaceRust"])

    def test_api_and_ui_changes_include_their_contract_consumers(self):
        api = select_products(["packages/api/app_core/http/messages.py"])
        self.assertTrue(api["workspacePython"] and api["workspaceRust"] and api["docker"])
        ui = select_products(["packages/ui/src/components/chat/messageScroll.ts"])
        self.assertTrue(ui["localNode"] and ui["web"])

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
                self.assertFalse(aggregate_result(needs))
                needs["core-rust"]["result"] = "success"
                self.assertTrue(aggregate_result(needs))

    def test_selected_skipped_failed_cancelled_and_missing_jobs_fail_the_aggregate(self):
        selected = dict.fromkeys(PRODUCTS, False)
        selected["web"] = True
        needs = {"scope": {"result": "success", "outputs": {key: str(value).lower() for key, value in selected.items()}}}
        needs.update({job: {"result": "success" if selected[product] else "skipped"} for product, job in JOBS.items()})
        self.assertTrue(aggregate_result(needs))
        for result in ("skipped", "failure", "cancelled"):
            altered = {**needs, "web": {"result": result}}
            self.assertFalse(aggregate_result(altered))
        self.assertFalse(aggregate_result({key: value for key, value in needs.items() if key != "web"}))
        self.assertFalse(aggregate_result({**needs, "scope": {"result": "failure"}}))


if __name__ == "__main__":
    unittest.main()
