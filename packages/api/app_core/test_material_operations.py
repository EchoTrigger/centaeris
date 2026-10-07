from django.conf import settings
from django.test import SimpleTestCase


class RetiredMaterialRoutesTests(SimpleTestCase):
    def test_retired_routes_are_not_compatibility_aliases(self):
        for path in ("knowledge/read", "knowledge/search", "knowledge/commit", "materials/reconcile"):
            with self.subTest(path=path):
                response = self.client.post(
                    "/internal/" + path, data="{}", content_type="application/json",
                    HTTP_X_INTERNAL_TOKEN=settings.INTERNAL_API_TOKEN,
                )
                self.assertEqual(response.status_code, 404)
