"""PostgreSQL 18 search over current authorized representations, with bounded preparation."""
import hashlib
import json
from dataclasses import dataclass
import re

from django.core import signing
from django.db import connection
from django.db.models import (BigIntegerField, Case, DateTimeField, Exists, F, Func, JSONField,
                             OuterRef, Q, Subquery, TextField, Value, When)
from django.db.models.functions import Collate
from django.utils.dateparse import parse_datetime

from . import material_access, material_evidence
from .assets import DeferredInputResolutionError
from .library_materials import library_descriptor, personal_library_allowed
from .material_contract import KnowledgeError
from .material_identity import representation_id, require_representation_input
from .material_operations import create_operations
from .models import DerivedRepresentation, KnowledgeSegment, MaterialProcessingTask, UserLibraryObject


PROCESSING_PAGE_SIZE = 20
CURSOR_SALT = "workspace.material.search.v1"
SEARCH_CURSOR_SCHEMA = "workspace.material.search.cursor.v1"
PROCESSING_CURSOR_SCHEMA = "workspace.material.processing.cursor.v1"
# Only the explicit generic archive MIME has a known unsupported content route.
# All other formats are left to the existing processor and its durable failure.
UNSUPPORTED_TYPES = {"application/zip"}


class TermOccurrences(Func):
    """A flat parameterized SQL sum, including queries with many distinct terms."""
    output_field = BigIntegerField()

    def __init__(self, folded, terms):
        from django.contrib.postgres.fields import ArrayField
        super().__init__(folded, Value(terms, output_field=ArrayField(TextField())))

    def as_postgresql(self, compiler, _connection, **extra_context):
        folded, params = compiler.compile(self.source_expressions[0])
        terms, term_params = compiler.compile(self.source_expressions[1])
        term = 'casefold(term.value COLLATE "pg_unicode_fast") COLLATE "C"'
        sql = (f"(SELECT COALESCE(SUM((char_length({folded}) - "
               f"char_length(replace({folded}, {term}, ''))) / char_length({term})), 0)::bigint "
               f"FROM unnest({terms}) AS term(value))")
        return sql, [*params, *params, *term_params]


@dataclass
class SearchScope:
    library: object
    attachments: dict


def _selection_digest(access, scope, refs, query, ranking):
    value = {"runId": access.agent_run.pk, "authorizationDigest": access.authorization_digest,
             "specDigest": access.spec_digest, "scope": scope, "inputRefs": sorted(refs) if refs is not None else None,
             "query": query, "ranking": ranking}
    return hashlib.sha256(json.dumps(value, ensure_ascii=False, sort_keys=True, separators=(",", ":")).encode()).hexdigest()


def _decode_cursor(token, schema, selection):
    if token is None:
        return None
    try:
        value = signing.loads(token, salt=CURSOR_SALT, fallback_keys=[])
        if (not isinstance(value, dict) or set(value) != {"schema", "selection", "asOf", "after"}
                or value["schema"] != schema or value["selection"] != selection):
            raise ValueError
        timestamp = parse_datetime(value["asOf"]) if isinstance(value["asOf"], str) else None
        if timestamp is None or timestamp.utcoffset() is None:
            raise ValueError
        if schema == SEARCH_CURSOR_SCHEMA:
            after = value["after"]
            if (not isinstance(after, list) or len(after) != 3 or type(after[0]) is not int or after[0] < 1
                    or not isinstance(after[1], str) or not isinstance(after[2], str) or not after[2]):
                raise ValueError
            updated = parse_datetime(after[1])
            if updated is None or updated.utcoffset() is None:
                raise ValueError
            value["after"] = (after[0], updated, after[2])
        elif not isinstance(value["after"], str):
            raise ValueError
        value["asOf"] = timestamp
        return value
    except (signing.BadSignature, ValueError, TypeError):
        raise KnowledgeError("material_search_cursor_invalid", 400) from None


def _cursor(schema, selection, as_of, after):
    return signing.dumps({"schema": schema, "selection": selection, "asOf": as_of.isoformat(), "after": after},
                         salt=CURSOR_SALT, compress=True)


def _bindings(access, refs):
    from .library_materials import resolve_library_input, input_identity
    declared = material_access.declared_inputs_for_run(access.agent_run)
    requests = []
    for ref in refs:
        if ref in declared:
            identity = declared[ref]["inputIdentity"]
        else:
            try:
                library = resolve_library_input(access.agent_run, ref, include_content=False)
            except DeferredInputResolutionError as error:
                raise KnowledgeError(error.errorCode, 403) from error
            if library is None:
                raise KnowledgeError("knowledge_input_not_authorized", 403)
            identity = input_identity(library[0])
        requests.append({"inputRef": ref, "representationId": representation_id(identity, access.spec_digest)})
    return material_access.bind_inputs(access.agent_run, access.authorization_digest, requests, access.spec_digest)


def _scope(access, scope, refs):
    if scope not in {"all", "library", "attached"}:
        raise KnowledgeError("knowledge_search_arguments_invalid", 400)
    personal = personal_library_allowed(access.agent_run)
    if scope == "library" and not personal:
        raise KnowledgeError("knowledge_input_not_authorized", 403)
    declared = material_access.declared_inputs_for_run(access.agent_run)
    if refs is not None:
        if (not isinstance(refs, list) or len(refs) > 128
                or any(not isinstance(ref, str) or not ref for ref in refs) or len(set(refs)) != len(refs)):
            raise KnowledgeError("knowledge_search_inputs_invalid", 400)
        if scope == "attached" and any(ref not in declared for ref in refs):
            raise KnowledgeError("knowledge_input_not_authorized", 403)
        selected = _bindings(access, refs)
        if scope == "library" and any(item.input_identity["ownerKind"] != "userLibraryObject" for item in selected):
            raise KnowledgeError("knowledge_input_not_authorized", 403)
        return SearchScope(None, {item.representation_id: item for item in selected})
    library = UserLibraryObject.objects.filter(owner_id=access.agent_run.user_id, status="ready").exclude(objectKind="folder") if personal and scope != "attached" else None
    attached_refs = [ref for ref, item in declared.items() if scope != "library"
                     and not (library is not None and item["inputIdentity"]["ownerKind"] == "userLibraryObject")]
    return SearchScope(library, {item.representation_id: item for item in _bindings(access, attached_refs)})


def _owner_representations(access):
    return DerivedRepresentation.objects.filter(ownerKind="userLibraryObject", ownerId=OuterRef("pk"),
        ownerContentGeneration=OuterRef("contentGeneration"), ownerSha256=OuterRef("sha256"),
        processingSpecification_id=access.spec_digest)


def _task_failure_query(access):
    as_json = lambda field: Func(OuterRef(field), function="to_jsonb", output_field=JSONField())
    return MaterialProcessingTask.objects.filter(payload__specDigest=access.spec_digest,
        payload__inputIdentity__ownerKind="userLibraryObject", payload__inputIdentity__ownerId=as_json("pk"),
        payload__inputIdentity__generation=as_json("contentGeneration"), payload__inputIdentity__sha256=as_json("sha256")).filter(
            Q(status__in=["failed", "completed"]) | ~Q(executionBackend="platform"))


def _eligible_q(max_bytes):
    return ~Q(contentType__in=UNSUPPORTED_TYPES) & Q(sizeBytes__lte=max_bytes)


def _preparation_error(owner, max_bytes):
    if owner.sizeBytes is None or owner.sizeBytes > max_bytes:
        return "material_input_too_large"
    if owner.contentType in UNSUPPORTED_TYPES:
        return "material_format_unsupported"
    return None


def _task_error(task):
    if task is None:
        return None
    if task.executionBackend != "platform":
        return "material_task_backend_conflict"
    if task.status == "failed":
        return task.errorCode or "material_processing_failed"
    if task.status == "completed":
        return "material_processing_result_missing"
    return None


def _representations(access, scope):
    library_match = Q(pk__in=[])
    updated = Value(None, output_field=DateTimeField())
    if scope.library is not None:
        owner = scope.library.filter(pk=OuterRef("ownerId"), contentGeneration=OuterRef("ownerContentGeneration"), sha256=OuterRef("ownerSha256"))
        library_match = Q(ownerKind="userLibraryObject") & Q(Exists(owner))
        updated = Subquery(owner.values("updatedAt")[:1], output_field=DateTimeField())
    cases = [When(pk=key, then=Value(material_evidence.owner_updated_at(item.owner))) for key, item in scope.attachments.items()]
    if cases:
        updated = Case(*cases, default=updated, output_field=DateTimeField())
    return DerivedRepresentation.objects.filter(processingSpecification_id=access.spec_digest).filter(
        library_match | Q(pk__in=list(scope.attachments))).annotate(_source_updated_at=updated)


def _coverage(access, scope, representations, as_of):
    max_bytes = access.processing_specification["options"]["maxInputBytes"]
    indexed = representations.count()
    searched = representations.filter(createdAt__lte=as_of, _source_updated_at__lte=as_of).count()
    failed = unsupported = unprocessed = total = 0
    if scope.library is not None:
        sources = scope.library.annotate(_indexed=Exists(_owner_representations(access)), _failed=Exists(_task_failure_query(access)))
        total += sources.count()
        missing = sources.filter(_indexed=False)
        unsupported += missing.exclude(_eligible_q(max_bytes)).count()
        eligible = missing.filter(_eligible_q(max_bytes))
        failed += eligible.filter(_failed=True).count()
        unprocessed += eligible.filter(_failed=False).count()
    present = set(DerivedRepresentation.objects.filter(pk__in=scope.attachments, processingSpecification_id=access.spec_digest).values_list("pk", flat=True))
    tasks = MaterialProcessingTask.objects.in_bulk(set(scope.attachments) - present)
    for key, item in scope.attachments.items():
        total += 1
        if key in present:
            continue
        if _preparation_error(item.owner, max_bytes):
            unsupported += 1
        elif _task_error(tasks.get(key)):
            failed += 1
        else:
            unprocessed += 1
    changed = representations.filter(Q(createdAt__gt=as_of) | Q(_source_updated_at__gt=as_of)).exists()
    if scope.library is not None:
        # Include tombstones: a source removed during a sweep must require refresh too.
        changed = changed or UserLibraryObject.objects.filter(owner_id=access.agent_run.user_id, updatedAt__gt=as_of).exclude(objectKind="folder").exists()
    complete = not (unprocessed or failed or unsupported or changed) and searched == total
    return {"totalMaterials": total, "indexedMaterials": indexed, "searchedMaterials": searched,
            "unprocessedMaterials": unprocessed, "failedMaterials": failed, "unsupportedMaterials": unsupported,
            "complete": complete, "refreshRequired": changed}


def _processing_page(access, scope, after, selection, as_of):
    max_bytes = access.processing_specification["options"]["maxInputBytes"]
    candidates = []
    present = set(DerivedRepresentation.objects.filter(pk__in=scope.attachments, processingSpecification_id=access.spec_digest).values_list("pk", flat=True))
    for key, item in scope.attachments.items():
        position = "attached|" + item.resolved["inputRef"]
        if key not in present and position > after:
            candidates.append((position, item.resolved["inputRef"], key, item.owner))
    if scope.library is not None:
        missing = scope.library.annotate(_indexed=Exists(_owner_representations(access))).filter(_indexed=False)
        if after.startswith("library|"):
            missing = missing.filter(pk__gt=after.split("|", 1)[1])
        for owner in missing.order_by("pk")[:PROCESSING_PAGE_SIZE + 1]:
            resolved = library_descriptor(access.agent_run, owner)
            identity = {"ownerKind": "userLibraryObject", "ownerId": owner.pk, "generation": owner.contentGeneration, "sha256": owner.sha256}
            candidates.append(("library|" + owner.pk, resolved["inputRef"], representation_id(identity, access.spec_digest), owner))
    candidates.sort(key=lambda value: value[0])
    page = candidates[:PROCESSING_PAGE_SIZE]
    tasks = MaterialProcessingTask.objects.in_bulk([value[2] for value in page])
    missing, failures = [], []
    for _position, ref, identity, owner in page:
        error = _preparation_error(owner, max_bytes) or _task_error(tasks.get(identity))
        if error:
            failures.append({"inputRef": ref, "errorCode": error})
        else:
            missing.append({"inputRef": ref, "representationId": identity})
    operations = create_operations(access, missing) if missing else []
    cursor = _cursor(PROCESSING_CURSOR_SCHEMA, selection, as_of, page[-1][0]) if len(candidates) > PROCESSING_PAGE_SIZE else None
    return operations, failures, cursor


def _ranked_segments(representations, query):
    folded = Collate(Func(Collate(F("boundedText"), "pg_unicode_fast"), function="casefold", output_field=TextField()), "C")
    folded_query = query.casefold()
    phrase = Collate(Func(Collate(Value(folded_query), "pg_unicode_fast"), function="casefold", output_field=TextField()), "C")
    terms = list(dict.fromkeys(re.findall(r"\w+", folded_query))) or [folded_query]
    current = representations.filter(pk=OuterRef("representation_id"))
    return KnowledgeSegment.objects.filter(representation_id__in=representations.values("pk")).alias(_folded=folded).annotate(
        _source_updated_at=Subquery(current.values("_source_updated_at")[:1], output_field=DateTimeField()),
        _score=TermOccurrences(F("_folded"), terms) + Case(
            When(condition=Q(_folded__contains=phrase), then=Value(2)), default=Value(0), output_field=BigIntegerField()),
    ).filter(_score__gt=0)


def search_library_materials(access, *, query, scope="all", input_refs=None, ranking="relevance", limit=10,
                             cursor=None, processing_cursor=None):
    if (not isinstance(query, str) or not query.strip() or len(query.encode("utf-8")) > 2048
            or ranking not in {"relevance", "recent"} or type(limit) is not int or not 1 <= limit <= 20):
        raise KnowledgeError("knowledge_search_arguments_invalid", 400)
    sources = _scope(access, scope, input_refs)
    selection = _selection_digest(access, scope, input_refs, query, ranking)
    search_position = _decode_cursor(cursor, SEARCH_CURSOR_SCHEMA, selection)
    processing_position = _decode_cursor(processing_cursor, PROCESSING_CURSOR_SCHEMA, selection)
    if search_position and processing_position and search_position["asOf"] != processing_position["asOf"]:
        raise KnowledgeError("material_search_cursor_invalid", 400)
    position = search_position or processing_position
    if position:
        as_of = position["asOf"]
    else:
        with connection.cursor() as db:
            db.execute("SELECT statement_timestamp()")
            as_of = db.fetchone()[0]
    representations = _representations(access, sources)
    searchable = representations.filter(createdAt__lte=as_of, _source_updated_at__lte=as_of)
    matches = _ranked_segments(searchable, query)
    if search_position:
        score, updated, segment_id = search_position["after"]
        if ranking == "recent":
            matches = matches.filter(Q(_source_updated_at__lt=updated) | Q(_source_updated_at=updated, _score__lt=score)
                                     | Q(_source_updated_at=updated, _score=score, segmentId__gt=segment_id))
        else:
            matches = matches.filter(Q(_score__lt=score) | Q(_score=score, _source_updated_at__lt=updated)
                                     | Q(_score=score, _source_updated_at=updated, segmentId__gt=segment_id))
    order = ("-_source_updated_at", "-_score", "segmentId") if ranking == "recent" else ("-_score", "-_source_updated_at", "segmentId")
    window = list(matches.order_by(*order).only("segmentId", "representation", "ordinal", "boundedText", "locator", "textSha256")[:limit + 1])
    selected = window[:limit]
    selected_representations = DerivedRepresentation.objects.only("representationId", "ownerKind", "ownerId",
        "ownerContentGeneration", "ownerSha256", "processingSpecification").in_bulk({item.representation_id for item in selected})
    hits = []
    bindings = dict(sources.attachments)
    for segment in selected:
        representation = selected_representations[segment.representation_id]
        bound = bindings.get(representation.pk)
        if bound is None:
            owner = UserLibraryObject.objects.get(pk=representation.ownerId, owner_id=access.agent_run.user_id)
            ref = library_descriptor(access.agent_run, owner)["inputRef"]
            bound = _bindings(access, [ref])[0]
            bindings[representation.pk] = bound
        require_representation_input(representation, bound)
        hits.append(material_evidence.search_evidence(segment, bound, representation, segment._score, full_window=True))
    next_search = None
    if len(window) > limit:
        last = selected[-1]
        next_search = _cursor(SEARCH_CURSOR_SCHEMA, selection, as_of,
                              [last._score, last._source_updated_at.isoformat(), last.pk])
    operations, failures, next_processing = _processing_page(access, sources,
        processing_position["after"] if processing_position else "", selection, as_of)
    coverage = _coverage(access, sources, representations, as_of)
    pending = coverage["unprocessedMaterials"] > 0 and not hits
    return {"disposition": "pending" if pending else "ready", "query": query, "ranking": ranking, "hits": hits,
            "processingSpecification": access.processing_specification, "coverage": coverage,
            "nextSearchCursor": next_search, "processingCursor": next_processing,
            "operations": operations, "processingFailures": failures}
