import importlib.util
import io
import json
from pathlib import Path
import unittest
from unittest.mock import patch


SOURCE = Path(__file__).resolve().parents[1] / "mock-model/server.py"
SPEC = importlib.util.spec_from_file_location("real_route_mock", SOURCE)


class RealModelRouteTests(unittest.TestCase):
    def setUp(self):
        self.module = importlib.util.module_from_spec(SPEC)
        SPEC.loader.exec_module(self.module)
        self.handler = object.__new__(self.module.Handler)
        self.handler.path = "/real/v1/chat/completions"
        self.body = json.dumps({"model": "fixed", "messages": [], "stream": True}).encode()
        self.handler.headers = {"content-length": str(len(self.body)), "Authorization": "secret-client"}
        self.handler.rfile = io.BytesIO(self.body)
        self.handler.wfile = io.BytesIO()
        self.statuses = []
        self.headers = {}
        self.handler.send_response = self.statuses.append
        self.handler.send_header = self.headers.__setitem__
        self.handler.end_headers = lambda: None
        self.handler.send_error = lambda status, *args: self.statuses.append(status)

    def test_disabled_route_refuses_without_connecting(self):
        with patch.dict(self.module.os.environ, {}, clear=True):
            self.handler.do_POST()
        self.assertEqual(self.statuses, [503])

    def test_enabled_route_streams_bytes_and_does_not_forward_client_headers(self):
        chunks = [b"data: one\n\n", b"data: [DONE]\n\n", b""]
        response = unittest.mock.Mock(status=200)
        response.getheader.return_value = "text/event-stream"
        response.read1.side_effect = chunks
        connection = unittest.mock.MagicMock()
        connection.getresponse.return_value = response
        with patch.dict(self.module.os.environ, {"MOCK_REAL_MODEL_SOCKET": "/run/real/model.sock"}), \
                patch.object(self.module, "UnixHTTPConnection", return_value=connection) as factory:
            self.handler.do_POST()
        factory.assert_called_once_with("/run/real/model.sock")
        connection.request.assert_called_once_with("POST", "/v1/chat/completions", body=self.body,
                                                   headers={"Content-Type": "application/json"})
        self.assertEqual(self.statuses, [200])
        self.assertEqual(self.handler.wfile.getvalue(), b"data: one\n\ndata: [DONE]\n\n")
        self.assertEqual(self.headers["connection"], "close")
        connection.close.assert_called_once()

    def test_transport_errors_are_sanitized(self):
        with patch.dict(self.module.os.environ, {"MOCK_REAL_MODEL_SOCKET": "/run/real/model.sock"}), \
                patch.object(self.module, "UnixHTTPConnection", side_effect=OSError("secret-error-body")):
            self.handler.do_POST()
        self.assertEqual(self.statuses, [502])
        self.assertNotIn(b"secret", self.handler.wfile.getvalue())

    def test_route_is_exact(self):
        self.handler.path += "?redirect=somewhere"
        self.handler.do_POST()
        self.assertEqual(self.statuses, [404])


if __name__ == "__main__":
    unittest.main()
