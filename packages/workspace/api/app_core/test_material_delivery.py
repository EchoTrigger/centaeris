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

    def test_exhausted_read_window_returns_a_valid_content_read_continuation(self):
        from .platform_mcp import VALIDATORS

        item = {'content': 'first line\n', 'inputRef': 'library:note:1:hash',
                'citationAllowed': True,
                'locator': {'kind': 'textSpan', 'startByte': 0, 'endByte': 11,
                            'startLine': 1, 'endLine': 1},
                'startLine': 1, 'endLine': 1, 'totalLines': 2, 'nextOffset': 1, 'maxLines': 1}
        page = delivery.render_page({'disposition': 'ready', 'items': [item]}, 'result_5', '0:0')
        continuation = page['continuation']
        self.assertEqual(continuation['tool'], 'read_material')
        self.assertEqual(continuation['arguments'], {
            'input_ref': item['inputRef'], 'delivery': 'content', 'offset': 1, 'limit': 1,
        })
        VALIDATORS[continuation['tool']].validate(continuation['arguments'])

    def test_search_progress_survives_content_paging_without_mixing_cursors(self):
        content = '资料😀' * 15000
        metadata = {'query': '资料', 'ranking': 'relevance', 'scope': 'all',
                    'coverage': {'totalMaterials': 4, 'indexedMaterials': 1, 'searchedMaterials': 1,
                                 'unprocessedMaterials': 1, 'failedMaterials': 1, 'unsupportedMaterials': 1,
                                 'complete': False, 'refreshRequired': False},
                    'nextSearchCursor': 'signed-hit-cursor', 'processingCursor': 'signed-source-cursor',
                    'operations': [{'operationId': 'operation', 'status': 'pending'}],
                    'processingFailures': [{'inputRef': 'failed', 'errorCode': 'processing_failed'}]}
        snapshot = {**metadata, 'disposition': 'ready', 'hits': [
            {'content': content, 'segmentId': 'segment', 'citationAllowed': False, 'locator': None}]}
        cursor, chunks = '0:0', []
        while cursor is not None:
            page = delivery.render_page(snapshot, 'result_6', cursor)
            self.assertLessEqual(len(json.dumps(page, ensure_ascii=False).encode()), delivery.PAGE_BUDGET)
            for name, value in metadata.items():
                self.assertEqual(page[name], value)
            self.assertIn('Search is incomplete', page['message'])
            chunks.append(page['hits'][0]['content'])
            if page['continuation']:
                self.assertEqual(page['continuation']['tool'], 'read_material_result')
                cursor = page['continuation']['arguments']['cursor']
                self.assertNotIn(cursor, [metadata['nextSearchCursor'], metadata['processingCursor']])
            else:
                cursor = None
        self.assertEqual(''.join(chunks), content)

    def test_empty_search_window_retains_incomplete_coverage_and_refresh_requirement(self):
        coverage = {'totalMaterials': 2, 'indexedMaterials': 1, 'searchedMaterials': 0,
                    'unprocessedMaterials': 1, 'failedMaterials': 0, 'unsupportedMaterials': 0,
                    'complete': False, 'refreshRequired': True}
        page = delivery.render_page({'disposition': 'ready', 'hits': [], 'coverage': coverage,
                                     'nextSearchCursor': None, 'processingCursor': None}, 'result_7', '0:0')
        self.assertEqual(page['coverage'], coverage)
        self.assertIsNone(page['continuation'])
        self.assertIn('Search is incomplete', page['message'])
        self.assertIn('search again without cursor', page['message'])
        reloaded = dict(reversed(list(coverage.items())))
        replay = delivery.render_page({'disposition': 'ready', 'hits': [], 'coverage': reloaded,
                                       'nextSearchCursor': None, 'processingCursor': None}, 'result_7', '0:0')
        self.assertEqual(replay, page)
