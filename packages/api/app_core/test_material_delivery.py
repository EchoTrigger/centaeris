import json
from django.test import SimpleTestCase
from .material_contract import sha256_bytes
from . import material_delivery as delivery


class MaterialDeliveryTests(SimpleTestCase):
    def test_large_unicode_result_is_preserved_and_paged_without_gaps(self):
        content = ('界😀\\\"\n' * 18000) + 'last line'
        item = {'content': content, 'inputRef': 'asset', 'displayName': 'Document',
                'representationId': 'representation', 'citationAllowed': True,
                'locator': {'kind': 'textSpan', 'startByte': 0, 'endByte': len(content.encode()),
                            'startLine': 1, 'endLine': 18001, 'pageStart': None, 'pageEnd': None},
                'startLine': 1, 'endLine': 18001, 'totalLines': 18001, 'nextOffset': None}
        snapshot = {'disposition': 'ready', 'items': [item]}
        cursor, chunks = '0:0', []
        while cursor is not None:
            page = delivery.render_page(snapshot, 'result_1', cursor)
            self.assertLessEqual(len(json.dumps(page, ensure_ascii=False).encode()), delivery.PAGE_BUDGET)
            value = page['items'][0]
            self.assertEqual(value['locator']['startByte'], len(''.join(chunks).encode()))
            self.assertEqual(value['evidenceSha256'], sha256_bytes(value['content'].encode()))
            self.assertIn('Showing', page['message'])
            self.assertTrue(page['completeResultSaved'])
            chunks.append(value['content'])
            cursor = page['continuation']['arguments']['cursor'] if page['continuation'] else None
        self.assertGreater(len(chunks), 1)
        self.assertEqual(''.join(chunks), content)
        self.assertEqual(snapshot['items'][0]['content'], content)

    def test_search_pages_keep_hit_order_and_do_not_discard_long_hits(self):
        hits = [{'content': 'x' * 90000, 'segmentId': str(n), 'citationAllowed': False,
                 'locator': {'kind': 'textSpan', 'startByte': 0, 'endByte': 90000,
                             'startLine': 1, 'endLine': 1}} for n in range(3)]
        snapshot = {'disposition': 'ready', 'hits': hits}
        cursor, collected = '0:0', {}
        while cursor is not None:
            page = delivery.render_page(snapshot, 'result_2', cursor)
            hit = page['hits'][0]
            collected[hit['segmentId']] = collected.get(hit['segmentId'], '') + hit['content']
            cursor = page['continuation']['arguments']['cursor'] if page['continuation'] else None
        self.assertEqual(list(collected), ['0', '1', '2'])
        self.assertEqual(list(collected.values()), ['x' * 90000] * 3)

    def test_small_search_hits_share_one_page(self):
        hits = [{"content": "small result", "segmentId": str(n), "citationAllowed": False,
                 "locator": {"kind": "textSpan", "startByte": 0, "endByte": 12,
                             "startLine": 1, "endLine": 1}} for n in range(10)]
        page = delivery.render_page({"disposition": "ready", "hits": hits}, "result_4", "0:0")
        self.assertEqual([item["segmentId"] for item in page["hits"]], [str(n) for n in range(10)])
        self.assertIsNone(page["continuation"])

    def test_empty_result_has_no_nonadvancing_continuation(self):
        page = delivery.render_page({'disposition': 'ready', 'hits': []}, 'result_3', '0:0')
        self.assertEqual(page['hits'], [])
        self.assertIsNone(page['continuation'])
