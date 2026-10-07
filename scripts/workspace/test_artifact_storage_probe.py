"""Probe guards run in the ordinary, service-free Python gate."""
import unittest
from unittest.mock import Mock, patch
import artifact_storage_probe as probe
from artifact_storage_probe import (
    IMAGE, make_compose, require_local_endpoint, classify_error,
    Missing, Conflict, Denied, Unavailable,
)


class ArtifactStorageProbeGuards(unittest.TestCase):
    def test_existing_object_with_different_length_is_a_conflict(self):
        store = probe.CandidateStore(Mock(), "bucket")
        store.call = Mock(side_effect=[Conflict(), {"ContentLength": 99}])
        with self.assertRaises(Conflict):
            store.put("key", b"short")

    def test_restart_discovers_new_dynamic_port_before_readiness(self):
        compose = Mock(return_value="127.0.0.1:23456")
        store = Mock()
        with patch.object(probe, "sdk", return_value="new-client") as factory, patch.object(probe, "wait_ready") as ready:
            probe.reconnect_after_restart(store, compose, "key", "secret")
        factory.assert_called_once_with("http://127.0.0.1:23456", "key", "secret")
        self.assertEqual(store.client, "new-client")
        ready.assert_called_once_with("new-client")
        self.assertEqual(compose.call_args_list[0].args, ("start", "rustfs"))

    def test_compose_is_owned_bounded_and_loopback_only(self):
        config = make_compose("centaeris-rustfs-probe-abc", "synthetic-key", "synthetic-secret")
        service = config["services"]["rustfs"]
        self.assertEqual(service["image"], IMAGE)
        self.assertIn("@sha256:", IMAGE)
        self.assertEqual(service["ports"], ["127.0.0.1::9000"])
        self.assertEqual(service["mem_limit"], "1g")
        self.assertNotIn("privileged", service)
        self.assertNotIn("container_name", service)
        self.assertNotIn("external", config["volumes"]["data"])
        with self.assertRaises(ValueError):
            make_compose("centaeris-workspace", "key", "secret")

    def test_probe_cannot_target_a_remote_or_credential_bearing_endpoint(self):
        require_local_endpoint("http://127.0.0.1:12345")
        for value in ("https://example.com", "http://localhost:9000", "http://user:pass@127.0.0.1:9000", "http://127.0.0.1:9000/path"):
            with self.subTest(value=value), self.assertRaises(ValueError):
                require_local_endpoint(value)

    def test_denial_outage_and_conflict_never_become_missing(self):
        for status, code, expected in (
            (404, "NoSuchKey", Missing), (403, "AccessDenied", Denied),
            (404, "404", Unavailable), (404, "NoSuchBucket", Unavailable),
            (412, "PreconditionFailed", Conflict), (409, "ConditionalRequestConflict", Conflict),
            (503, "SlowDown", Unavailable), (500, "InternalError", Unavailable),
        ):
            self.assertIs(classify_error(status, code), expected)


if __name__ == "__main__":
    unittest.main()
