import io
import json
import os
import tempfile
import tracemalloc
import unittest
from datetime import date
from contextlib import closing
from pathlib import Path
from unittest.mock import MagicMock, Mock, patch

import document_processor as processor


def text_pdf(*pages: str) -> bytes:
    objects = {
        1: b"<< /Type /Catalog /Pages 2 0 R >>",
        2: (
            f"<< /Type /Pages /Count {len(pages)} /Kids "
            f"[{' '.join(f'{5 + index * 2} 0 R' for index in range(len(pages)))}] >>"
        ).encode(),
        3: b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>",
    }
    for index, text in enumerate(pages):
        page_number = 5 + index * 2
        content_number = page_number + 1
        stream = f"BT /F1 12 Tf 72 720 Td ({text}) Tj ET".encode()
        objects[page_number] = (
            f"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] "
            f"/Resources << /Font << /F1 3 0 R >> >> /Contents {content_number} 0 R >>"
        ).encode()
        objects[content_number] = (
            f"<< /Length {len(stream)} >>\nstream\n".encode()
            + stream
            + b"\nendstream"
        )
    document = bytearray(b"%PDF-1.4\n%\xe2\xe3\xcf\xd3\n")
    offsets = {}
    for number, body in sorted(objects.items()):
        offsets[number] = len(document)
        document.extend(f"{number} 0 obj\n".encode() + body + b"\nendobj\n")
    xref = len(document)
    size = max(objects) + 1
    document.extend(f"xref\n0 {size}\n0000000000 65535 f \n".encode())
    for number in range(1, size):
        if number in offsets:
            document.extend(f"{offsets[number]:010d} 00000 n \n".encode())
        else:
            document.extend(b"0000000000 00000 f \n")
    document.extend(
        f"trailer\n<< /Size {size} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n".encode()
    )
    return bytes(document)


def scanned_pdf(page_count: int) -> bytes:
    from PIL import Image, ImageDraw

    pages = []
    for page in range(1, page_count + 1):
        image = Image.new("RGB", (612, 792), "white")
        draw = ImageDraw.Draw(image)
        draw.rectangle((60, 80, 552, 220), fill="black")
        draw.text((72, 260), f"scanned page {page}", fill="black")
        pages.append(image)
    output = io.BytesIO()
    pages[0].save(
        output,
        format="PDF",
        save_all=True,
        append_images=pages[1:],
        resolution=72,
    )
    for page in pages:
        page.close()
    return output.getvalue()


class DocumentProcessorTests(unittest.TestCase):
    def setUp(self):
        self.device = patch.dict(os.environ, {"CENTAERIS_PROCESSOR_DEVICE": "cpu"})
        self.device.start()
        processor.ocr_pipeline.cache_clear()

    def tearDown(self):
        processor.ocr_pipeline.cache_clear()
        self.device.stop()

    def test_processor_device_identity_is_exact_and_gpu_capability_fails_loudly(self):
        self.assertEqual(processor.processor_device(), "cpu")
        with patch.dict(os.environ, {"CENTAERIS_PROCESSOR_DEVICE": "banana"}):
            with self.assertRaisesRegex(processor.ProcessingError, "exactly cpu or gpu:0"):
                processor.processor_device()
        paddle = Mock()
        paddle.device.is_compiled_with_cuda.return_value = False
        paddle.device.cuda.device_count.return_value = 0
        with patch.dict(os.environ, {"CENTAERIS_PROCESSOR_DEVICE": "gpu:0"}), patch.dict(
            "sys.modules", {"paddle": paddle}
        ):
            with self.assertRaisesRegex(processor.ProcessingError, "one CUDA-visible device"):
                processor.specification()

    def test_model_digest_ignores_transport_cache_and_binds_inference_payload(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for name in processor.MODEL_PAYLOAD_FILES:
                (root / name).write_bytes(name.encode())
            expected = processor.hash_model_payload(root)
            cache = root / ".cache" / "huggingface"
            cache.mkdir(parents=True)
            (cache / "download.metadata").write_text("banana", encoding="utf-8")
            self.assertEqual(processor.hash_model_payload(root), expected)
            (root / "inference.json").write_bytes(b"changed")
            self.assertNotEqual(processor.hash_model_payload(root), expected)

    def test_native_quality_rejects_noise_and_accepts_real_text(self):
        self.assertFalse(processor.native_text_is_usable("1\n"))
        self.assertFalse(processor.native_text_is_usable("\ufffd" * 20))
        self.assertTrue(
            processor.native_text_is_usable("A complete policy sentence with useful text.")
        )

    def test_xlsx_preview_preserves_workbook_structure_and_safe_styles(self):
        from openpyxl import Workbook
        from openpyxl.styles import Alignment, Border, Font, PatternFill, Side

        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "budget.xlsx"
            workbook = Workbook()
            sheet = workbook.active
            sheet.title = "预算"
            sheet.freeze_panes = "B2"
            sheet.merge_cells("A1:C1")
            sheet["A1"] = "差旅预算"
            sheet["A1"].font = Font(bold=True, color="FFFFFFFF")
            sheet["A1"].fill = PatternFill("solid", fgColor="FF1F4E78")
            sheet["A1"].alignment = Alignment(horizontal="center", wrap_text=True)
            sheet["A1"].border = Border(bottom=Side(style="thin", color="FF000000"))
            sheet["A2"], sheet["B2"], sheet["C2"] = "日期", "项目", "金额"
            sheet["A3"], sheet["B3"], sheet["C3"] = date(2026, 9, 11), "住宿", 680
            sheet["A3"].number_format = "yyyy-mm-dd"
            sheet["C3"].number_format = '¥#,##0.00'
            sheet.column_dimensions["B"].width = 24
            sheet.row_dimensions[1].height = 28
            sheet.column_dimensions["C"].hidden = True
            sheet.row_dimensions[4].hidden = True
            workbook.create_sheet("明细")["A1"] = "第二张表"
            workbook.save(path)

            preview = processor.workbook_preview(path)

        self.assertEqual(preview["schema"], "knowledge.workbook_preview.v1")
        self.assertEqual([sheet["name"] for sheet in preview["sheets"]], ["预算", "明细"])
        budget = preview["sheets"][0]
        self.assertEqual(budget["frozenPane"], "B2")
        self.assertEqual(budget["merges"], [{"startRow": 1, "startColumn": 1, "endRow": 1, "endColumn": 3}])
        self.assertEqual(budget["columns"], [
            {"index": 2, "widthPx": 173, "hidden": False},
            {"index": 3, "widthPx": 96, "hidden": True},
        ])
        self.assertEqual(budget["rows"][0]["heightPx"], 37)
        self.assertTrue(budget["rows"][3]["hidden"])
        cells = {(row["index"], cell["column"]): cell for row in budget["rows"] for cell in row["cells"]}
        self.assertEqual(cells[(3, 1)]["displayValue"], "2026-09-11")
        self.assertEqual(cells[(3, 1)]["kind"], "date")
        self.assertEqual(cells[(3, 3)]["displayValue"], "680")
        self.assertEqual(cells[(3, 3)]["numberFormat"], '¥#,##0.00')
        title_style = preview["styles"][cells[(1, 1)]["styleId"]]
        self.assertEqual(title_style["font"], {"bold": True, "italic": False, "color": "#FFFFFF"})
        self.assertEqual(title_style["fill"], "#1F4E78")
        self.assertEqual(title_style["horizontal"], "center")
        self.assertTrue(title_style["wrapText"])
        self.assertEqual(title_style["borders"]["bottom"], {"style": "thin", "color": "#000000"})

    def test_text_processing_writes_atomic_canonical_page_manifest(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / "source"
            output = root / "output"
            request = root / "request.json"
            source.write_bytes(b"first\r\nsecond")
            request.write_text(
                json.dumps(
                    {
                        "schema": "knowledge.processing.request.v1",
                        "inputPath": str(source),
                        "displayName": "notes.md",
                        "contentType": "text/markdown",
                        "outputDirectory": str(output),
                    }
                ),
                encoding="utf-8",
            )
            processor.process(request)
            canonical = (output / "canonical.md").read_text(encoding="utf-8")
            manifest = json.loads((output / "manifest.json").read_text(encoding="utf-8"))
            self.assertEqual(canonical, "# notes.md\n\n## Page 1\n\nfirst\nsecond\n\n")
            self.assertEqual(manifest["pageCount"], 1)
            self.assertEqual(manifest["pages"][0]["pageText"]["route"], "nativeText")
            self.assertEqual(
                manifest["pages"][0]["pageText"]["textSha256"],
                processor.sha256_bytes(b"first\nsecond"),
            )

    def test_real_text_pdf_keeps_native_text_and_stable_page_numbers(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / "policy.pdf"
            output = root / "output"
            request = root / "request.json"
            source.write_bytes(
                text_pdf(
                    "First page has a complete policy sentence.",
                    "Second page has a different review sentence.",
                )
            )
            request.write_text(
                json.dumps(
                    {
                        "schema": "knowledge.processing.request.v1",
                        "inputPath": str(source),
                        "displayName": "policy.pdf",
                        "contentType": "application/pdf",
                        "outputDirectory": str(output),
                    }
                ),
                encoding="utf-8",
            )

            processor.process(request)

            canonical = (output / "canonical.md").read_text(encoding="utf-8")
            manifest = json.loads((output / "manifest.json").read_text(encoding="utf-8"))
            self.assertIn("## Page 1\n\nFirst page has a complete policy sentence.", canonical)
            self.assertIn("## Page 2\n\nSecond page has a different review sentence.", canonical)
            self.assertEqual(manifest["pageCount"], 2)
            self.assertEqual(
                [page["pageText"]["page"] for page in manifest["pages"]],
                [1, 2],
            )
            self.assertEqual(
                [page["pageText"]["route"] for page in manifest["pages"]],
                ["pdfNative", "pdfNative"],
            )

    def test_scanned_pdf_routes_each_physical_page_through_ocr(self):
        with tempfile.TemporaryDirectory() as directory:
            source = Path(directory) / "scan.pdf"
            source.write_bytes(scanned_pdf(2))

            def recognized(_path, page, width, height):
                self.assertEqual(list(_path.parent.glob("*.png")), [_path])
                text = f"recognized page {page}"
                return processor.page_text(
                    page,
                    "ppOcrV6Small",
                    width,
                    height,
                    text,
                    [
                        {
                            "text": text,
                            "bbox": [100, 200, 9_000, 1_500],
                            "confidenceMilli": 900,
                        }
                    ],
                )

            with patch.object(processor, "ocr_image", side_effect=recognized) as ocr:
                pages = list(processor.process_pdf(source))

            self.assertEqual(ocr.call_count, 2)
            self.assertEqual([page["page"] for page in pages], [1, 2])
            self.assertEqual(
                [page["route"] for page in pages],
                ["ppOcrV6Small", "ppOcrV6Small"],
            )
            self.assertEqual(
                [page["text"] for page in pages],
                ["recognized page 1", "recognized page 2"],
            )

    def test_ocr_image_normalizes_text_boxes_and_confidence(self):
        from PIL import Image

        with tempfile.TemporaryDirectory() as directory:
            image_path = Path(directory) / "page.png"
            Image.new("RGB", (100, 200), "white").save(image_path)
            pipeline = Mock()
            pipeline.predict.return_value = [
                {
                    "res": {
                        "rec_texts": ["  First line  "],
                        "rec_scores": [0.875],
                        "rec_boxes": [[10, 20, 90, 180]],
                    }
                }
            ]

            with patch.object(processor, "ocr_pipeline", return_value=pipeline):
                page = processor.ocr_image(image_path, 3, 612_000, 792_000)

            pipeline.predict.assert_called_once_with(str(image_path))
            self.assertEqual(page["page"], 3)
            self.assertEqual(page["route"], "ppOcrV6Small")
            self.assertEqual(page["text"], "First line")
            self.assertEqual(
                page["spans"],
                [
                    {
                        "text": "First line",
                        "bbox": [1_000, 1_000, 9_000, 9_000],
                        "confidenceMilli": 875,
                    }
                ],
            )

    def test_corrupt_pdf_fails_loudly(self):
        with tempfile.TemporaryDirectory() as directory:
            source = Path(directory) / "broken.pdf"
            source.write_bytes(b"not a PDF")

            with self.assertRaisesRegex(processor.ProcessingError, "could not open"):
                list(processor.process_pdf(source))

    def test_real_pdf_over_one_thousand_pages_streams_to_the_same_manifest_format(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / "long.pdf"
            source.write_bytes(text_pdf(*(f"Complete policy text for physical page {n}." for n in range(1, 1002))))
            output = root / "output"
            output.mkdir()
            with closing(processor.process_pdf(source)) as pages, patch.object(processor, "ocr_image") as ocr:
                count = processor.write_outputs("long.pdf", pages, output, None)
            self.assertEqual(count, 1001)
            ocr.assert_not_called()
            canonical = (output / "canonical.md").read_bytes()
            manifest = json.loads((output / "manifest.json").read_bytes())
            self.assertEqual(manifest["pageCount"], 1001)
            for index in (0, 999, 1000):
                page = manifest["pages"][index]
                self.assertEqual(page["pageText"]["page"], index + 1)
                self.assertEqual(canonical[page["canonicalStartByte"]:page["canonicalEndByte"]].decode(), page["pageText"]["text"])

    def test_multiframe_image_over_one_thousand_pages_and_zero_page_pdf(self):
        from PIL import Image

        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / "frames.tiff"
            with Image.new("RGB", (2, 2), "white") as frame:
                frame.save(source, save_all=True, append_images=[frame] * 1000)
            self.assertEqual(sum(1 for _ in processor.process_image(source)), 1001)
            empty = MagicMock()
            empty.__enter__.return_value = empty
            empty.__len__.return_value = 0
            with patch("pypdfium2.PdfDocument", return_value=empty), self.assertRaisesRegex(processor.ProcessingError, "pageCount=0"):
                list(processor.process_pdf(root / "empty.pdf"))
            empty.__exit__.assert_called_once()

    def test_streaming_preserves_utf8_offsets_and_multiline_headings(self):
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory)
            texts = ["法律条文\n第二行😀", "", "last line"]
            pages = (processor.page_text(n, "nativeText", 1000, 1000, text, []) for n, text in enumerate(texts, 1))
            processor.write_outputs("附件\n标题.doc", pages, output, None)
            canonical = (output / "canonical.md").read_bytes()
            manifest = json.loads((output / "manifest.json").read_bytes())
            for page, expected in zip(manifest["pages"], texts, strict=True):
                start, end = page["canonicalStartByte"], page["canonicalEndByte"]
                self.assertEqual(canonical[start:end], expected.encode())
                self.assertEqual(page["canonicalStartLine"], canonical[:start].count(b"\n") + 1)
                self.assertEqual(page["canonicalEndLine"], canonical[:end].count(b"\n") + 1)

    def test_output_budgets_stop_consumption_and_leave_no_partial_outputs(self):
        for limit_name in ("MAX_OUTPUT_BYTES", "MAX_MANIFEST_BYTES"):
            with self.subTest(limit_name=limit_name), tempfile.TemporaryDirectory() as directory:
                consumed = []

                def pages():
                    for n in range(1, 10001):
                        consumed.append(n)
                        yield processor.page_text(n, "nativeText", 1000, 1000, "policy" * 30, [])

                with patch.object(processor, limit_name, 512), self.assertRaisesRegex(processor.ProcessingError, "output byte limit"):
                    processor.write_outputs("input.pdf", pages(), Path(directory), None)
                self.assertLess(len(consumed), 5)
                self.assertEqual(list(Path(directory).iterdir()), [])

    def test_preview_is_copied_incrementally_and_shares_the_output_budget(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            preview = root / "converted.pdf"
            preview.write_bytes(b"x" * 100)
            output = root / "output"
            output.mkdir()
            with patch.object(Path, "read_bytes", side_effect=AssertionError("whole-file read")):
                processor.write_outputs("input.doc", iter([processor.empty_page(1, 1000, 1000)]), output, preview)
            self.assertEqual((output / "preview.pdf").read_bytes(), b"x" * 100)
            with patch.object(processor, "MAX_OUTPUT_BYTES", 110), self.assertRaisesRegex(processor.ProcessingError, "output byte limit"):
                processor.write_outputs("input.doc", iter([processor.empty_page(1, 1000, 1000)]), output, preview)
            self.assertEqual((output / "preview.pdf").read_bytes(), b"x" * 100)
            self.assertEqual({path.name for path in output.iterdir()}, {"canonical.md", "manifest.json", "preview.pdf"})

    def test_streaming_writer_memory_does_not_accumulate_pages(self):
        peaks = []
        for count in (100, 5000):
            with tempfile.TemporaryDirectory() as directory:
                pages = (processor.page_text(n, "nativeText", 1000, 1000, f"page {n} " + "x" * 1024, []) for n in range(1, count + 1))
                tracemalloc.start()
                try:
                    processor.write_outputs("input.pdf", pages, Path(directory), None)
                    peaks.append(tracemalloc.get_traced_memory()[1])
                finally:
                    tracemalloc.stop()
        self.assertLess(peaks[1] - peaks[0], 2 * 1024 * 1024)

    def test_pixel_and_page_text_bounds_still_fail_before_ocr(self):
        from PIL import Image

        with tempfile.TemporaryDirectory() as directory:
            source = Path(directory) / "image.png"
            with Image.new("RGB", (20, 20), "black") as image:
                image.save(source)
            with patch.object(Image, "MAX_IMAGE_PIXELS", processor.MAX_RENDERED_PIXELS_PER_PAGE), patch.object(processor, "MAX_RENDERED_PIXELS_PER_PAGE", 300), patch.object(processor, "ocr_image") as ocr:
                with self.assertRaisesRegex(processor.ProcessingError, "pixel limit"):
                    list(processor.process_image(source))
                ocr.assert_not_called()
            with patch.object(processor, "MAX_PAGE_TEXT_BYTES", 3), self.assertRaisesRegex(processor.ProcessingError, "text exceeds"):
                processor.page_text(1, "nativeText", 1000, 1000, "法律", [])


if __name__ == "__main__":
    unittest.main()
