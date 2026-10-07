"""Run inside a disposable API container, before and after container replacement."""
import io
import json
import os
from pathlib import Path
import sys
from unittest.mock import patch
import zipfile

sys.path.insert(0, "/app/packages/api")
os.environ.setdefault("DJANGO_SETTINGS_MODULE", "api.settings")
import django
django.setup()

from django.conf import settings
from django.core.files.storage import default_storage
from django.core.files.uploadedfile import SimpleUploadedFile
from app_core.plugin_catalog import load_plugin_catalog
from app_core.plugin_install_source import UploadedZip
from app_core.plugin_lifecycle import install_plugin_from_source, remove_plugin


for path in ("/proc/1/status", "/proc/self/status"):
    status = dict(line.split(":", 1) for line in Path(path).read_text().splitlines())
    for field in ("CapEff", "CapPrm", "CapBnd"):
        assert int(status[field], 16) == 0, f"{path}: {field} must be empty"
    assert status["NoNewPrivs"].strip() == "1", f"{path}: privilege gain must be blocked"

name = "deployment-smoke"
upload_path = "deployment-smoke/upload.bin"
manifest = {"name": name, "version": "1.0.0", "paths": {}, "interface": {
    "displayName": "Deployment smoke", "shortDescription": "Synthetic deployment test",
    "capabilities": ["Synthetic test"],
}}

if sys.argv[1] == "write":
    assert not default_storage.exists(upload_path), "requires fresh disposable storage"
    assert not load_plugin_catalog()["packages"], "requires empty disposable plugin catalog"
    stored = default_storage.save(upload_path, SimpleUploadedFile("upload.bin", b"synthetic-upload"))
    assert stored == upload_path
    archive = io.BytesIO()
    with zipfile.ZipFile(archive, "w") as carrier:
        carrier.writestr(".centaeris-plugin/plugin.json", json.dumps(manifest))
    package = {"name": name, "version": "1.0.0", "packageDigest": "sha256:" + "a" * 64,
               "skills": [], "cli": [], "mcpServers": [], "hooks": []}
    # Isolate only the unavailable Runtime inspection request. ZIP extraction, locks,
    # atomic rename/fsync, catalog validation and mounted-volume writes are real.
    with patch("app_core.plugin_lifecycle.request_plugin_inspection", return_value=package):
        install_plugin_from_source(UploadedZip(SimpleUploadedFile("plugin.zip", archive.getvalue())))
elif sys.argv[1] == "read":
    with default_storage.open(upload_path, "rb") as uploaded:
        assert uploaded.read() == b"synthetic-upload"
    assert load_plugin_catalog()["packages"][0]["name"] == name
    assert json.loads((Path(settings.PLUGIN_CATALOG_ROOT) / name / ".centaeris-plugin/plugin.json").read_text()) == manifest
    remove_plugin(name)
    default_storage.delete(upload_path)
    assert not load_plugin_catalog()["packages"]
    assert not default_storage.exists(upload_path)
else:
    raise ValueError("mode must be write or read")
print("API capability, storage and Plugin deployment probe passed:", sys.argv[1])
