import json
from unittest import TestCase

from app_core.sso_config import parse_providers


class OAuthConfigurationTests(TestCase):
    def configuration(self, **changes):
        return {"providerId": "example", "displayName": "Example", "clientId": "client",
            "clientSecret": "synthetic-private-secret", "authorizeUrl": "https://identity.example/authorize",
            "tokenUrl": "https://identity.example/token", "userinfoUrl": "https://identity.example/userinfo",
            "subjectField": "user.id", **changes}

    def parse(self, **changes):
        return parse_providers(json.dumps([self.configuration(**changes)]))[0]

    def test_disabled_configuration_is_empty(self):
        self.assertEqual(parse_providers("[]"), ())

    def test_invalid_configuration_never_reports_the_secret(self):
        for changes in [{"unexpected": True}, {"providerId": ""}, {"clientSecret": ""},
                {"scope": "openid profile"}, {"subjectField": "user..id"},
                {"tokenEndpointAuthMethod": "none"}, {"userinfoUrl": "https://user:password@identity.example/u"}]:
            with self.subTest(changes=changes):
                with self.assertRaisesRegex(RuntimeError, "^Invalid OAuth provider configuration$"):
                    self.parse(**changes)

    def test_http_requires_explicit_test_mode(self):
        config = json.dumps([self.configuration(tokenUrl="http://127.0.0.1/token")])
        with self.assertRaises(RuntimeError):
            parse_providers(config)
        self.assertEqual(len(parse_providers(config, allow_insecure_http=True)), 1)

    def test_duplicate_provider_ids_fail(self):
        with self.assertRaises(RuntimeError):
            parse_providers(json.dumps([self.configuration(), self.configuration()]))

    def test_secret_rotation_preserves_identity_but_registration_changes_do_not(self):
        original = self.parse().fingerprint
        self.assertEqual(original, self.parse(clientSecret="rotated-test-secret", displayName="New label").fingerprint)
        self.assertNotEqual(original, self.parse(clientId="different-client").fingerprint)
        self.assertNotEqual(original, self.parse(subjectField="another.id").fingerprint)
