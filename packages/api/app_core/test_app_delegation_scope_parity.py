"""The settings picker and hosted authority expose the same scope registry."""
import json
from pathlib import Path
import re
from unittest import TestCase

from .app_delegation_contract import NATIVE_SCOPES, SCOPES


class AppDelegationScopeParityTests(TestCase):
    def web_scopes(self, name):
        root = Path(__file__).resolve().parents[3]
        source = (root / "packages/web/src/routes/AppDelegations.jsx").read_text(encoding="utf-8")
        declaration = re.search(rf"const {name} = (?:new Set\()?(\[[^\]]*\])\)?;", source)
        self.assertIsNotNone(declaration, f"Missing settings scope registry: {name}")
        values = json.loads(declaration.group(1))
        self.assertEqual(len(values), len(set(values)), "Scope picker must not duplicate permissions")
        return frozenset(values)

    def test_published_assistant_picker_matches_server_scopes(self):
        self.assertEqual(self.web_scopes("SCOPES"), SCOPES)

    def test_native_agent_picker_matches_server_scopes(self):
        self.assertEqual(self.web_scopes("NATIVE_SCOPES"), NATIVE_SCOPES)
