"""Bound, read-only Run and object-version projections for Agent chat."""
from typing import Literal
from urllib.parse import quote, urlencode

from asgiref.sync import sync_to_async
from django.core.exceptions import ObjectDoesNotExist
from django.core.files.storage import default_storage
from django.db.models import Q
from django.http import HttpResponse, JsonResponse
from ninja import Router, Status
from pydantic import Field

from app_core.agent_messages import _trusted_message
from app_core.agent_inputs import require_native_agent_delegation
from app_core.app_delegations import DelegationRejected, session_authority_is_current
from app_core.assets import DeferredInputResolutionError
from app_core.deferred_input import DeferredInputBindingError, input_storage_batch
from app_core.models import AgentCoordinationSession, Artifact, SessionEvent
from app_core.session_event import committed_session_terminal_state

from .downloads import _is_code_preview
from .office_preview import FORMATS, _owner, _preview_selection
from .response_schema import COMMON_ERROR_RESPONSES
from .schema import StrictSchema
from .security import usage_auth
from .storage_stream import StoredObjectUnavailable, stored_file_response
from .workspaces import _authorized_transcript_session

router = Router(tags=['agent-previews'], by_alias=True)


class RunFact(StrictSchema):
    agent_run_id: str = Field(alias='agentRunId')
    status: Literal['queued', 'running', 'completed', 'failed', 'cancelled']
    source_type: Literal['hostedRun', 'sessionEvent', 'preAdmissionCancellation'] = Field(alias='sourceType')
    event_id: str | None = Field(alias='eventId')
    created_at_ms: int = Field(alias='createdAtMs')


class PreviewFile(StrictSchema):
    input_ref: str | None = Field(alias='inputRef')
    agent_run_id: str = Field(alias='agentRunId')
    owner_kind: Literal['artifact', 'userLibraryObject', 'sourceObject'] = Field(alias='ownerKind')
    object_ref: str = Field(alias='objectRef')
    source_version: str = Field(alias='sourceVersion')
    sha256: str
    display_name: str = Field(alias='displayName')
    content_type: str = Field(alias='contentType')
    size_bytes: int = Field(alias='sizeBytes')
    preview_url: str = Field(alias='previewUrl')
    download_url: str = Field(alias='downloadUrl')


class SessionPreview(StrictSchema):
    schema_id: Literal['session.preview.v1'] = Field(alias='schema')
    session_id: str = Field(alias='sessionId')
    run_fact: RunFact | None = Field(alias='runFact')
    outputs: list[PreviewFile]
    next_after_artifact_id: str | None = Field(alias='nextAfterArtifactId')
    has_more: bool = Field(alias='hasMore')


class MessageFiles(StrictSchema):
    schema_id: Literal['agent.message.files.v1'] = Field(alias='schema')
    agent_id: str = Field(alias='agentId')
    session_id: str = Field(alias='sessionId')
    message_id: str = Field(alias='messageId')
    agent_run_id: str = Field(alias='agentRunId')
    files: list[PreviewFile]


class PreviewRejected(Exception):
    def __init__(self, status=404, code='preview_not_available'):
        self.status, self.code = status, code


def _query(request, fields):
    if set(request.GET) - fields or any(len(request.GET.getlist(key)) != 1 for key in request.GET):
        raise PreviewRejected(400, 'preview_query_invalid')


def _run_fact(session):
    run = session.agent_runs.order_by('-createdAt', '-id').first()
    if run is None:
        return None
    terminal = committed_session_terminal_state(run)
    if terminal is not None:
        event = SessionEvent.objects.filter(agent_run=run, session_level=False).order_by('-agent_run_sequence').first()
        return {'agentRunId': run.pk, 'status': terminal,
            'sourceType': 'sessionEvent' if event else 'preAdmissionCancellation', 'eventId': event.pk if event else None,
            'createdAtMs': event.createdAtMs if event else int(run.preAdmissionCancelledAt.timestamp()*1000)}
    if run.status not in {'queued', 'running'}:
        return None
    return {'agentRunId': run.pk, 'status': run.status, 'sourceType': 'hostedRun', 'eventId': None,
        'createdAtMs': int(run.updatedAt.timestamp()*1000)}


def _urls(prefix, version, sha256):
    query = urlencode({'sourceVersion': str(version), 'sha256': sha256, 'lang': 'zh-CN'})
    return {'previewUrl': prefix+'/preview?'+query, 'downloadUrl': prefix+'/download?'+query}


def _artifact_fact(item):
    prefix = f'/api/sessions/{quote(item.session_id, safe="")}/outputs/{quote(item.pk, safe="")}'
    return {'inputRef': None, 'agentRunId': item.agent_run_id, 'ownerKind': 'artifact', 'objectRef': item.pk,
        'sourceVersion': str(item.contentGeneration), 'sha256': item.sha256, 'displayName': item.displayName,
        'contentType': item.contentType, 'sizeBytes': item.sizeBytes, **_urls(prefix, item.contentGeneration, item.sha256)}


@router.get('/sessions/{session_id}/preview', auth=usage_auth('sessions:read'),
    response={200: SessionPreview} | COMMON_ERROR_RESPONSES)
def session_preview(request, response: HttpResponse, session_id: str, afterArtifactId: str | None = None, limit: int = 50):
    response['Cache-Control'] = 'no-store'
    try:
        require_native_agent_delegation(request, 'sessions:read', session_id=session_id)
        _query(request, {'afterArtifactId', 'limit'})
        if not 1 <= limit <= 100:
            raise PreviewRejected(400, 'preview_query_invalid')
        session = _authorized_transcript_session(request.user, session_id)
        if session is None or session.status != 'active' or session.agent.status != 'active':
            raise PreviewRejected()
        rows = Artifact.objects.filter(session=session, agent_run__user=request.user,
            status='published', deletedAt__isnull=True).order_by('createdAt', 'id')
        if afterArtifactId is not None:
            anchor = rows.filter(pk=afterArtifactId).first()
            if anchor is None:
                raise PreviewRejected(400, 'preview_cursor_invalid')
            rows = rows.filter(Q(createdAt__gt=anchor.createdAt) | Q(createdAt=anchor.createdAt, id__gt=anchor.pk))
        items = list(rows[:limit+1])
        return {'schema': 'session.preview.v1', 'sessionId': session.pk, 'runFact': _run_fact(session),
            'outputs': [_artifact_fact(item) for item in items[:limit]], 'hasMore': len(items) > limit,
            'nextAfterArtifactId': items[limit-1].pk if len(items) > limit else None}
    except PreviewRejected as error:
        return Status(error.status, {'error': error.code})


def _message(user, agent_id, message_id):
    binding = AgentCoordinationSession.objects.select_related('agent').filter(agent_id=agent_id, agent__owner=user,
        agent__status='active').first()
    if binding is None or not session_authority_is_current(user.id, binding.session_id):
        raise PreviewRejected()
    event = SessionEvent.objects.select_related('agent_run__authorization').filter(eventId=message_id,
        session_id=binding.session_id, workspace_id=binding.agent.workspace_id, projects_to_agent_run_stream=True,
        payload__type='tool_result', payload__payload__toolName='send_message',
        payload__payload__resultState='successWithOutput').first()
    message = _trusted_message(binding, event) if event is not None else None
    if message is None:
        raise PreviewRejected()
    return binding, event.agent_run, message


def _resolve_message_file(user, agent_id, message_id, input_ref):
    binding, run, message = _message(user, agent_id, message_id)
    if input_ref not in message['fileRefs']:
        raise PreviewRejected()
    try:
        resolved, key = input_storage_batch(run, run.authorization.digest)(input_ref)
    except DeferredInputBindingError as error:
        raise PreviewRejected(409, 'preview_version_changed') from error
    except ObjectDoesNotExist as error:
        raise PreviewRejected() from error
    except DeferredInputResolutionError as error:
        if error.errorCode == 'stale_generation':
            raise PreviewRejected(409, 'preview_version_changed') from error
        raise PreviewRejected() from error
    return resolved, key, run


@router.get('/agents/{agent_id}/messages/{message_id}/files', auth=usage_auth('artifacts:read'),
    response={200: MessageFiles} | COMMON_ERROR_RESPONSES)
def message_files(request, response: HttpResponse, agent_id: str, message_id: str):
    response['Cache-Control'] = 'no-store'
    try:
        require_native_agent_delegation(request, 'artifacts:read', agent_id=agent_id)
        _query(request, set())
        binding, run, message = _message(request.user, agent_id, message_id)
        files = []
        for input_ref in dict.fromkeys(message['fileRefs']):
            resolved, _, _ = _resolve_message_file(request.user, agent_id, message_id, input_ref)
            prefix = f'/api/agents/{quote(agent_id, safe="")}/messages/{quote(message_id, safe="")}/files/{quote(input_ref, safe="")}'
            files.append({key: resolved[key] for key in ['inputRef', 'ownerKind', 'objectRef', 'sourceVersion', 'sha256', 'displayName', 'contentType', 'sizeBytes']} |
                {'agentRunId': run.pk, **_urls(prefix, resolved['sourceVersion'], resolved['sha256'])})
        return {'schema': 'agent.message.files.v1', 'agentId': binding.agent_id, 'sessionId': binding.session_id,
            'messageId': message_id, 'agentRunId': run.pk, 'files': files}
    except PreviewRejected as error:
        return Status(error.status, {'error': error.code})


def _select_content(user, request, source_version, sha256, lang, *, session_id=None, artifact_id=None, agent_id=None, message_id=None, input_ref=None, input_id=None, agent_attachment=False, download=False):
    require_native_agent_delegation(request, 'artifacts:read', agent_id=agent_id, session_id=session_id)
    _query(request, {'sourceVersion', 'sha256', 'lang'})
    if lang not in {'en', 'zh-CN'} or len(source_version) > 20 or not source_version.isascii() or not source_version.isdigit() or str(int(source_version)) != source_version or int(source_version) < 1:
        raise PreviewRejected(400, 'preview_query_invalid')
    if input_ref is not None:
        if agent_attachment:
            resolved, storage_key = _resolve_agent_input_file(user, agent_id, input_id, input_ref)
        else:
            resolved, storage_key, _ = _resolve_message_file(user, agent_id, message_id, input_ref)
        kind, object_id = resolved['ownerKind'], resolved['objectRef']
        version, digest = resolved['sourceVersion'], resolved['sha256']
        filename, content_type, size = resolved['displayName'], resolved['contentType'], resolved['sizeBytes']
    else:
        session = _authorized_transcript_session(user, session_id)
        item = _owner(user.id, 'artifact', artifact_id)
        if session is None or session.status != 'active' or session.agent.status != 'active' or item is None or item.session_id != session_id:
            raise PreviewRejected()
        kind, object_id = 'artifact', item.pk
        version, digest = str(item.contentGeneration), item.sha256
        filename, content_type, size, storage_key = item.displayName, item.contentType, item.sizeBytes, item.storageKey
    if version != source_version or digest != sha256:
        raise PreviewRejected(409, 'preview_version_changed')
    if not download and filename.rsplit('.', 1)[-1].lower() in FORMATS:
        selected = _preview_selection(user.id, kind, object_id, lang,
            expected_identity={'generation': int(version), 'sha256': digest}, refresh_url=request.get_full_path())
        return selected if not isinstance(selected, tuple) else (selected[0], 'application/pdf', selected[1], selected[2])
    if not download and _is_code_preview(filename, content_type):
        content_type = 'text/plain'
    return storage_key, content_type, filename, size


async def _content(request, source_version, sha256, lang, **identity):
    def select():
        return _select_content(request.user, request, source_version, sha256, lang, **identity)
    try:
        selected = await sync_to_async(select, thread_sensitive=True)()
    except (PreviewRejected, DelegationRejected) as error:
        response = JsonResponse({'error': error.code}, status=error.status)
        response['Cache-Control'] = 'no-store'
        return response
    if not isinstance(selected, tuple):
        return selected
    def open_current():
        try:
            current = select()
            if current != selected:
                raise StoredObjectUnavailable()
            return default_storage.open(current[0], 'rb')
        except (PreviewRejected, DelegationRejected, FileNotFoundError) as error:
            raise StoredObjectUnavailable() from error
    response = await stored_file_response(selected[0], selected[1], selected[2], content_length=selected[3],
        as_attachment=identity.get('download', False), authorized_open=lambda: sync_to_async(open_current, thread_sensitive=True)())
    response['Cache-Control'] = 'no-store'
    response['X-Content-Type-Options'] = 'nosniff'
    return response


@router.get('/agents/{agent_id}/messages/{message_id}/files/{input_ref}/{action}', auth=usage_auth('artifacts:read'), response=None)
async def message_file_content(request, agent_id: str, message_id: str, input_ref: str, action: Literal['preview', 'download'], sourceVersion: str, sha256: str, lang: str):
    return await _content(request, sourceVersion, sha256, lang, agent_id=agent_id, message_id=message_id, input_ref=input_ref, download=action == 'download')


def _resolve_agent_input_file(user, agent_id, input_id, input_ref):
    from app_core.agent_input_attachments import resolve_agent_attachment
    from app_core.agent_inputs import AgentInputError
    try:
        return resolve_agent_attachment(user, agent_id, input_ref, input_id)
    except AgentInputError as error:
        raise PreviewRejected(error.status, error.code) from error
    except DeferredInputResolutionError as error:
        raise PreviewRejected(409 if error.errorCode == 'stale_generation' else 404,
            'preview_version_changed' if error.errorCode == 'stale_generation' else 'preview_not_available') from error


async def _agent_input_content(request, agent_id, input_id, input_ref, action, lang):
    try:
        await sync_to_async(require_native_agent_delegation, thread_sensitive=True)(request, 'artifacts:read', agent_id=agent_id)
        resolved, _ = await sync_to_async(_resolve_agent_input_file, thread_sensitive=True)(request.user, agent_id, input_id, input_ref)
    except (PreviewRejected, DelegationRejected) as error:
        response = JsonResponse({'error': error.code}, status=error.status)
        response['Cache-Control'] = 'no-store'
        return response
    return await _content(request, resolved['sourceVersion'], resolved['sha256'], lang,
        agent_id=agent_id, input_id=input_id, input_ref=input_ref, agent_attachment=True, download=action == 'download')


@router.get('/agents/{agent_id}/inputs/{input_id}/attachments/{input_ref}/{action}', auth=usage_auth('artifacts:read'), response=None)
async def agent_input_content(request, agent_id: str, input_id: str, input_ref: str, action: Literal['preview', 'download'], lang: str = 'zh-CN'):
    return await _agent_input_content(request, agent_id, input_id, input_ref, action, lang)


@router.get('/agents/{agent_id}/attachments/{input_ref}/{action}', auth=usage_auth('artifacts:read'), response=None)
async def agent_draft_attachment_content(request, agent_id: str, input_ref: str, action: Literal['preview', 'download'], lang: str = 'zh-CN'):
    return await _agent_input_content(request, agent_id, None, input_ref, action, lang)


@router.get('/sessions/{session_id}/outputs/{artifact_id}/{action}', auth=usage_auth('artifacts:read'), response=None)
async def artifact_content(request, session_id: str, artifact_id: str, action: Literal['preview', 'download'], sourceVersion: str, sha256: str, lang: str):
    return await _content(request, sourceVersion, sha256, lang, session_id=session_id, artifact_id=artifact_id, download=action == 'download')
