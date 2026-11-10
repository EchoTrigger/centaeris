"""Exercise public error responses through Django and Ninja method routing."""
from django.test import SimpleTestCase, override_settings
from django.urls import path
from ninja import NinjaAPI

from app_core.http.errors import install_error_handlers
from app_core.http.schema import StrictSchema


class Payload(StrictSchema):
    name: str


api = NinjaAPI(urls_namespace="error_contract", docs_url=None, openapi_url=None)
install_error_handlers(api)


@api.get("/sources")
def list_sources(request):
    return {"ok": True}


@api.post("/sources")
def create_source(request, payload: Payload):
    return {"ok": True}


@api.get("/assets")
def list_session_assets(request):
    return {"ok": True}


@api.post("/assets")
def attach_session_asset(request, payload: Payload):
    return {"ok": True}


@api.delete("/assets")
def detach_session_asset(request, payload: Payload):
    return {"ok": True}


@api.post("/model")
def test_model(request, payload: Payload):
    return {"ok": True}


@api.post("/unmapped")
def unmapped_operation(request, payload: Payload):
    return {"ok": True}


urlpatterns = [path("", api.urls)]


@override_settings(ROOT_URLCONF=__name__)
class HttpErrorContractTests(SimpleTestCase):
    def test_method_routing_preserves_operation_errors(self):
        for method, target, status, code in [
            ("POST", "/sources", 400, "invalid_source"),
            ("POST", "/assets", 403, "asset_not_accessible"),
            ("DELETE", "/assets", 404, "asset_link_not_found"),
            ("POST", "/model", 400, "invalid_json"),
        ]:
            for body in ["{", '{"name":7}', '{"name":"valid","unknown":true}']:
                with self.subTest(method=method, target=target, body=body):
                    response = self.client.generic(method, target, body, "application/json")
                    self.assertEqual(response.status_code, status)
                    self.assertEqual(response.json(), {"error": code})

    def test_unmapped_errors_keep_distinct_defaults_and_field_details(self):
        malformed = self.client.post("/unmapped", "{", content_type="application/json")
        invalid = self.client.post("/unmapped", '{"name":7}', content_type="application/json")
        self.assertEqual(malformed.status_code, 400)
        self.assertEqual(malformed.json(), {"error": "invalid_json"})
        self.assertEqual(invalid.status_code, 400)
        self.assertEqual(invalid.json(), {"error": "request_invalid", "fields": ["payload.name"]})

    def test_valid_requests_reach_the_selected_operation(self):
        for method, target in [("POST", "/sources"), ("POST", "/assets"), ("DELETE", "/assets")]:
            response = self.client.generic(method, target, '{"name":"valid"}', "application/json")
            self.assertEqual(response.status_code, 200)
            self.assertEqual(response.json(), {"ok": True})
