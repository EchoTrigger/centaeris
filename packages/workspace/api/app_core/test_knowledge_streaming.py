import hashlib

from django.test import SimpleTestCase

from .material_commit import _validate_manifest
from .material_contract import KnowledgeError


class KnowledgeStreamingTests(SimpleTestCase):
    def test_more_than_one_thousand_pages_keep_utf8_and_line_identity_checks(self):
        canonical = bytearray("# 附件\n\n".encode())
        pages = []
        for number in range(1, 1002):
            text = "法律条文\n第二行😀" if number % 2 else ""
            body = text.encode()
            canonical.extend(f"## Page {number}\n\n".encode())
            start = len(canonical)
            start_line = canonical.count(b"\n") + 1
            canonical.extend(body)
            end = len(canonical)
            canonical.extend(b"\n\n")
            pages.append({
                "pageText": {
                    "schema": "knowledge.page_text.v1", "page": number,
                    "route": "nativeText" if text else "empty",
                    "widthMillipoints": 1000, "heightMillipoints": 1000,
                    "text": text, "textSha256": f"sha256:{hashlib.sha256(body).hexdigest()}",
                    "spans": [{"text": text, "bbox": [0, 0, 10000, 10000]}] if text else [],
                },
                "canonicalStartByte": start, "canonicalEndByte": end,
                "canonicalStartLine": start_line,
                "canonicalEndLine": start_line + body.count(b"\n"),
            })
        manifest = {"schema": "knowledge.derived_manifest.v1", "pageCount": len(pages), "pages": pages}
        _validate_manifest(manifest, bytes(canonical))
        pages[-1]["canonicalStartLine"] += 1
        with self.assertRaisesRegex(KnowledgeError, "knowledge_page_text_identity_invalid"):
            _validate_manifest(manifest, bytes(canonical))
        pages[-1]["canonicalStartLine"] -= 1
        pages[0]["pageText"]["page"] = True
        with self.assertRaisesRegex(KnowledgeError, "knowledge_page_text_identity_invalid"):
            _validate_manifest(manifest, bytes(canonical))

    def test_empty_manifest_still_fails(self):
        with self.assertRaisesRegex(KnowledgeError, "knowledge_manifest_page_count_invalid"):
            _validate_manifest({"schema": "knowledge.derived_manifest.v1", "pageCount": 0, "pages": []}, b"")
