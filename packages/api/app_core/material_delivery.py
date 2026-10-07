"""Durable material results, bounded model pages, and exact evidence ranges."""
import copy
import json
import re

from .material_contract import KnowledgeError, sha256_bytes

# Leave room for the receipt and citation IDs added after page construction.
PAGE_BUDGET = 48 * 1024


def _single_page(snapshot, result_ref, cursor):
    if not isinstance(cursor, str) or not re.fullmatch(r"[0-9]{1,10}:[0-9]{1,12}", cursor):
        raise KnowledgeError("material_result_cursor_invalid", 400)
    index, offset = map(int, cursor.split(":"))
    key = "items" if "items" in snapshot else "hits"
    values = snapshot[key]
    if not values:
        if index or offset:
            raise KnowledgeError("material_result_cursor_invalid", 400)
        return {"disposition": "ready", key: [], "resultRef": result_ref,
                "completeResultSaved": True, "continuation": None,
                "message": "No matching content. The complete result is preserved."}
    if index >= len(values):
        raise KnowledgeError("material_result_cursor_invalid", 400)
    original = values[index]
    raw = original["content"].encode("utf-8")
    if offset > len(raw) or (offset < len(raw) and raw[offset] & 0xC0 == 0x80):
        raise KnowledgeError("material_result_cursor_invalid", 400)
    prefix = raw[:offset].decode("utf-8")
    remaining = raw[offset:].decode("utf-8")

    def candidate(count):
        text = remaining[:count]
        end = offset + len(text.encode("utf-8"))
        item = copy.deepcopy(original)
        item["content"] = text
        item["evidenceSha256"] = sha256_bytes(text.encode("utf-8"))
        item["citationAllowed"] = bool(text) and original.get("citationAllowed", False)
        locator = copy.deepcopy(original.get("locator"))
        if locator is not None and text:
            locator["startByte"] += offset
            locator["endByte"] = locator["startByte"] + len(text.encode("utf-8"))
            locator["startLine"] += prefix.count("\n")
            locator["endLine"] = locator["startLine"] + text.count("\n") - int(text.endswith("\n"))
            ranges = snapshot.get("_pageRanges", {}).get(original.get("representationId"), [])
            if ranges:
                pages = [page for start, stop, page in ranges
                         if start < locator["endByte"] and stop > locator["startByte"]]
                locator["pageStart"] = min(pages) if pages else None
                locator["pageEnd"] = max(pages) if pages else None
        item["locator"] = locator if text else None
        if key == "items":
            item.update(outputBytes=len(text.encode("utf-8")),
                        startLine=locator["startLine"] if locator else original["startLine"],
                        endLine=locator["endLine"] if locator else original["endLine"],
                        pageStart=locator.get("pageStart") if locator else None,
                        pageEnd=locator.get("pageEnd") if locator else None)
        next_cursor = f"{index}:{end}" if end < len(raw) else (f"{index + 1}:0" if index + 1 < len(values) else None)
        continuation = ({"tool": "read_material_result", "arguments": {"result_ref": result_ref, "cursor": next_cursor}}
                        if next_cursor else None)
        if not continuation and key == "items" and original.get("nextOffset") is not None:
            continuation = {"tool": "read_material", "arguments": {
                "input_ref": original["inputRef"], "offset": original["nextOffset"],
                "limit": original.get("maxLines", 2000)}}
        if key == "items":
            item["nextOffset"] = original.get("nextOffset") if next_cursor is None else None
            item["truncated"] = continuation is not None
            item["firstLineExceedsLimit"] = False
            item["truncatedBy"] = "bytes" if next_cursor else original.get("truncatedBy")
        line_range = (f"lines {locator['startLine']}–{locator['endLine']}" if locator else "empty content")
        total = f" of {original['totalLines']}" if "totalLines" in original else ""
        message = (f"Showing result {index + 1} of {len(values)}, {line_range}{total}; "
                   f"content bytes {offset}–{end} of {len(raw)} (end exclusive). "
                   "The complete result is preserved. "
                   + ("More content is available. Continue with " + json.dumps(continuation, ensure_ascii=False)
                      if continuation else "This result has been fully returned."))
        return {"message": message, "disposition": "ready", "resultRef": result_ref,
                "completeResultSaved": True, "resultSha256": snapshot.get("_digest"),
                "continuation": continuation, key: [item]}

    low, high = 0, len(remaining)
    while low < high:
        middle = (low + high + 1) // 2
        if len(json.dumps(candidate(middle), ensure_ascii=False).encode()) <= PAGE_BUDGET:
            low = middle
        else:
            high = middle - 1
    if remaining and not low:
        raise KnowledgeError("material_result_metadata_exceeds_budget")
    result = candidate(low)
    if len(json.dumps(result, ensure_ascii=False).encode()) > PAGE_BUDGET:
        raise KnowledgeError("material_result_metadata_exceeds_budget")
    return result


def render_page(snapshot, result_ref, cursor):
    page = _single_page(snapshot, result_ref, cursor)
    if "hits" not in page:
        return page
    start_index = int(cursor.split(":")[0])
    while page["continuation"]:
        continuation = page["continuation"]
        if continuation["tool"] != "read_material_result":
            break
        next_cursor = continuation["arguments"]["cursor"]
        index, offset = map(int, next_cursor.split(":"))
        if offset:
            break
        following = _single_page(snapshot, result_ref, next_cursor)
        candidate = {**page, "hits": page["hits"] + following["hits"],
                     "continuation": following["continuation"]}
        candidate["message"] = (
            f"Showing results {start_index + 1}–{index + 1} of {len(snapshot['hits'])}. "
            "Each hit includes the exact returned line and byte range. The complete result is preserved. "
            + ("More content is available. Continue with " + json.dumps(candidate["continuation"], ensure_ascii=False)
               if candidate["continuation"] else "This result has been fully returned."))
        if len(json.dumps(candidate, ensure_ascii=False).encode()) > PAGE_BUDGET:
            break
        page = candidate
    return page


def save_result(access, call, result):
    from .models import DerivedRepresentation, MaterialResultSnapshot
    payload = copy.deepcopy(result)
    key = "items" if "items" in payload else "hits"
    ids = {item["representationId"] for item in payload[key]}
    payload["_pageRanges"] = {r.representationId: [
        [p["canonicalStartByte"], p["canonicalEndByte"], p["pageText"]["page"]]
        for p in r.manifest["pages"]] for r in DerivedRepresentation.objects.filter(pk__in=ids)}
    digest = sha256_bytes(json.dumps(payload, ensure_ascii=False, sort_keys=True).encode())
    identity = "material.result:" + sha256_bytes(call.pk.encode()).split(":")[1]
    stored, _ = MaterialResultSnapshot.objects.get_or_create(call=call, defaults={
        "id": identity, "payload": payload, "sha256": digest})
    if stored.sha256 != digest or stored.payload != payload:
        raise KnowledgeError("material_result_replay_conflict")
    payload["_digest"] = digest
    return render_page(payload, stored.pk, "0:0")


def read_result(access, result_ref, cursor):
    from .material_access import bind_inputs
    from .models import MaterialResultSnapshot
    try:
        stored = MaterialResultSnapshot.objects.get(pk=result_ref, call__session=access.agent_run.session,
                                                    call__workspace=access.agent_run.workspace)
    except MaterialResultSnapshot.DoesNotExist as error:
        raise KnowledgeError("material_result_not_authorized", 403) from error
    payload = copy.deepcopy(stored.payload)
    if sha256_bytes(json.dumps(payload, ensure_ascii=False, sort_keys=True).encode()) != stored.sha256:
        raise KnowledgeError("material_result_integrity_failed")
    values = payload["items" if "items" in payload else "hits"]
    bindings = {item["inputRef"]: {"inputRef": item["inputRef"], "representationId": item["representationId"]}
                for item in values}
    bind_inputs(access.agent_run, access.authorization_digest, list(bindings.values()), access.spec_digest)
    payload["_digest"] = stored.sha256
    return render_page(payload, stored.pk, cursor)
