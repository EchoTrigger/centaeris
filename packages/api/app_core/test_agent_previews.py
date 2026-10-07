"""Read-only HTTP/PG preview facts; committed events are synthetic fixtures."""
import hashlib
import json
import os
from pathlib import Path
from unittest.mock import patch
from urllib.parse import urlsplit, parse_qs

from asgiref.sync import sync_to_async
from django.core.files.base import ContentFile
from django.core.files.storage import default_storage
from django.test import TransactionTestCase
from django.utils import timezone

from .assets import captured_input_fields
from .models import Artifact, MaterialProcessingTask, SessionAssetLink, Source, SourceGrant, SourceObject, UserLibraryObject, WorkspaceGroup
from .test_agent_messages import AgentMessageTests as Fixture


class AgentPreviewTests(TransactionTestCase):
    serialized_rollback = True
    bind = Fixture.bind
    message_run = Fixture.message_run
    validate = Fixture.validate
    commit = Fixture.commit
    accepted_pair = Fixture.accepted_pair

    def setUp(self):
        Fixture.setUp(self)
        self.coordination = self.bind()

    def preview(self, session=None, **query):
        return self.client.get(f'/api/sessions/{(session or self.coordination).pk}/preview', query)

    def artifact(self, run, name='result.txt', **fields):
        content = b'Published output'
        key = default_storage.save('preview-fixture/'+run.id+'/'+name, ContentFile(content))
        return Artifact.objects.create(workspace=self.workspace, session=run.session, agent_run=run, createdBy=self.user,
            displayName=name, safeFilename=name, contentType='text/plain', sizeBytes=len(content),
            sha256='sha256:'+hashlib.sha256(content).hexdigest(), storageKey=key, status='published',
            contentGeneration=1, publishedAt=timezone.now(), **fields)

    def attachment(self, name='evidence.txt'):
        content = b'Original exact file version'
        key = default_storage.save('preview-fixture/'+self.coordination.id+'/'+name, ContentFile(content))
        item = UserLibraryObject.objects.create(owner=self.user, objectKind='file', displayName=name,
            contentType='text/plain', sizeBytes=len(content), sha256='sha256:'+hashlib.sha256(content).hexdigest(),
            storageKey=key, status='ready', contentGeneration=1)
        link = SessionAssetLink.objects.create(workspace=self.workspace, session=self.coordination,
            userLibraryObject=item, attachedBy=self.user, capturedDisplayName=name, capturedContentType=item.contentType,
            **captured_input_fields(item))
        run = self.message_run(self.coordination)
        event = self.accepted_pair(run, file_refs=[link.id])
        path = f'/api/agents/{self.agent.pk}/messages/{event.eventId}/files'
        return item, link, event, path, content

    def read_bytes(self, response):
        from .tests import streaming_response_bytes
        self.assertEqual(response.status_code, 200)
        content = streaming_response_bytes(response)
        for closer in response._resource_closers:
            closer()
        return content

    def test_child_terminal_does_not_complete_agent_and_committed_run_terminal_has_own_fact(self):
        child = self.message_run(self.work)
        terminal = self.commit(child, 'agent_run_completed', {})
        own = self.preview()
        work = self.preview(self.work)
        self.assertEqual((own.status_code, work.status_code), (200, 200))
        self.assertIsNone(own.json()['runFact'])
        fact = work.json()['runFact']
        self.assertEqual((fact['agentRunId'], fact['status'], fact['eventId']), (child.pk, 'completed', terminal.pk))
        self.assertEqual(fact['sourceType'], 'sessionEvent')

    def test_admitted_run_is_separate_from_previous_terminal_and_absence_proves_no_terminal(self):
        previous = self.message_run(self.coordination)
        self.commit(previous, 'agent_run_completed', {})
        current = self.message_run(self.coordination)
        type(current).objects.filter(pk=current.pk).update(status='queued')
        value = self.preview().json()['runFact']
        self.assertEqual((value['agentRunId'], value['status'], value['eventId']), (current.pk, 'queued', None))
        type(current).objects.filter(pk=current.pk).update(status='completed')
        self.assertIsNone(self.preview().json()['runFact'], 'cached terminal without a committed fact is not a completion proof')
        type(current).objects.filter(pk=current.pk).update(status='cancelled', preAdmissionCancelledAt=timezone.now())
        value = self.preview().json()['runFact']
        self.assertEqual((value['agentRunId'], value['status'], value['sourceType'], value['eventId']),
            (current.pk, 'cancelled', 'preAdmissionCancellation', None))

    def test_outputs_are_only_published_owned_artifacts_with_bounded_server_pagination(self):
        run = self.message_run(self.work)
        first, second = self.artifact(run, 'one.txt'), self.artifact(run, 'two.txt')
        hidden = self.artifact(run, 'hidden.txt')
        Artifact.objects.filter(pk=hidden.pk).update(status='staging')
        response = self.preview(self.work, limit=1)
        self.assertEqual(response.status_code, 200)
        page = response.json()
        self.assertEqual(page['outputs'][0]['ownerKind'], 'artifact')
        self.assertEqual(page['outputs'][0]['objectRef'], first.pk)
        self.assertEqual(page['outputs'][0]['sourceVersion'], '1')
        self.assertTrue(page['hasMore'])
        next_page = self.preview(self.work, limit=1, afterArtifactId=page['nextAfterArtifactId']).json()
        self.assertEqual([item['objectRef'] for item in next_page['outputs']], [second.pk])
        self.assertFalse(next_page['hasMore'])
        for query in [{'limit': 101}, {'after_artifact_id': first.pk}, {'afterArtifactId': 'unknown'}]:
            self.assertEqual(self.preview(self.work, **query).status_code, 400)

    def test_message_metadata_and_content_bind_original_file_identity(self):
        item, link, event, path, content = self.attachment()
        response = self.client.get(path)
        self.assertEqual(response.status_code, 200, response.content)
        value = response.json()
        file = value['files'][0]
        self.assertEqual((file['inputRef'], file['ownerKind'], file['objectRef'], file['sourceVersion'], file['sha256']),
            (link.pk, 'userLibraryObject', item.pk, '1', item.sha256))
        self.assertEqual(value['messageId'], event.pk)
        self.assertEqual(self.read_bytes(self.client.get(file['previewUrl'])), content)
        artifact = self.artifact(event.agent_run)
        outputs = self.preview().json()
        if os.environ.get('PREVIEW_DTO_OUTPUT'):
            Path(os.environ['PREVIEW_DTO_OUTPUT']).write_text(json.dumps({'files': value, 'session': outputs})+'\n', encoding='utf-8')
        self.assertEqual(outputs['outputs'][0]['objectRef'], artifact.pk)

    def test_revocation_is_checked_again_for_metadata_content_and_session_outputs(self):
        item, link, event, path, content = self.attachment()
        file = self.client.get(path).json()['files'][0]
        self.membership.delete()
        for url in [path, file['previewUrl'], file['downloadUrl'], f'/api/sessions/{self.coordination.pk}/preview']:
            self.assertEqual(self.client.get(url).status_code, 404)

    def test_source_grant_revocation_blocks_bound_metadata_and_actual_content(self):
        self.membership.role = 'member'
        self.membership.save(update_fields=['role'])
        content = b'Granted source file'
        key = default_storage.save('preview-fixture/source.txt', ContentFile(content))
        source = Source.objects.create(workspace=self.workspace, sourceType='fileTree', name='Source', status='ready', createdBy=self.other)
        item = SourceObject.objects.create(workspace=self.workspace, source=source, objectType='file',
            displayPath='source.txt', displayName='source.txt', contentType='text/plain', sizeBytes=len(content),
            sha256='sha256:'+hashlib.sha256(content).hexdigest(), storageKey=key, status='ready', contentGeneration=1)
        group = WorkspaceGroup.objects.create(workspace=self.workspace, name='Readers', createdBy=self.other)
        group.members.add(self.membership)
        grant = SourceGrant.objects.create(workspace=self.workspace, source=source, workspaceGroup=group, createdBy=self.other)
        link = SessionAssetLink.objects.create(workspace=self.workspace, session=self.coordination, sourceObject=item,
            attachedBy=self.user, capturedDisplayName=item.displayName, capturedContentType=item.contentType, **captured_input_fields(item))
        event = self.accepted_pair(self.message_run(self.coordination), file_refs=[link.pk])
        path = f'/api/agents/{self.agent.pk}/messages/{event.pk}/files'
        file = self.client.get(path).json()['files'][0]
        self.assertEqual(file['ownerKind'], 'sourceObject')
        self.assertEqual(self.read_bytes(self.client.get(file['previewUrl'])), content)
        grant.delete()
        for url in [path, file['previewUrl'], file['downloadUrl']]:
            self.assertEqual(self.client.get(url).status_code, 404)

    def test_missing_original_run_authorization_never_opens_an_owned_file(self):
        item, link, event, path, content = self.attachment()
        file = self.client.get(path).json()['files'][0]
        event.agent_run.authorization.delete()
        for url in [path, file['previewUrl']]:
            response = self.client.get(url)
            self.assertEqual(response.status_code, 404)
            self.assertEqual(response['Cache-Control'], 'no-store')

    def test_permission_is_rechecked_at_actual_storage_open_after_metadata_selection(self):
        item, link, event, path, content = self.attachment()
        file = self.client.get(path).json()['files'][0]
        from .http.storage_stream import stored_file_response
        async def revoke_before_open(*args, **kwargs):
            await sync_to_async(self.membership.delete, thread_sensitive=True)()
            return await stored_file_response(*args, **kwargs)
        with patch('app_core.http.agent_previews.stored_file_response', side_effect=revoke_before_open):
            response = self.client.get(file['previewUrl'])
        self.assertEqual(response.status_code, 409)
        self.assertFalse(response.streaming)

    def test_missing_stored_bytes_fail_without_returning_an_unbound_file(self):
        item, link, event, path, content = self.attachment()
        file = self.client.get(path).json()['files'][0]
        with patch('app_core.http.agent_previews.default_storage.open', side_effect=FileNotFoundError):
            response = self.client.get(file['previewUrl'])
        self.assertEqual(response.status_code, 409)
        self.assertFalse(response.streaming)

    def test_changed_version_or_unknown_message_never_falls_back_to_current_library_object(self):
        item, link, event, path, content = self.attachment()
        file = self.client.get(path).json()['files'][0]
        UserLibraryObject.objects.filter(pk=item.pk).update(contentGeneration=2)
        self.assertEqual(self.client.get(path).status_code, 409)
        self.assertEqual(self.client.get(file['previewUrl']).status_code, 409)
        self.assertEqual(self.client.get(path.replace(event.pk, 'unknown-message')).status_code, 404)
        # A record outside the committed message feed must not become a file
        # preview merely because its payload happens to resemble a result.
        event.payload['type'] = 'assistant_message'
        type(event).objects.filter(pk=event.pk).update(payload=event.payload)
        self.assertEqual(self.client.get(path).status_code, 404)
        event.payload['type'] = 'tool_result'
        type(event).objects.filter(pk=event.pk).update(payload=event.payload, projects_to_agent_run_stream=False)
        self.assertEqual(self.client.get(path).status_code, 404)

    def test_other_owner_and_version_tampering_cannot_open_a_published_artifact(self):
        run = self.message_run(self.work)
        self.artifact(run)
        output = self.preview(self.work).json()['outputs'][0]
        self.assertEqual(self.read_bytes(self.client.get(output['previewUrl'])), b'Published output')
        tampered = output['previewUrl'].replace('sourceVersion=1', 'sourceVersion=2')
        self.assertEqual(self.client.get(tampered).status_code, 409)
        for invalid in ['0', '01', '-1', '1'*5000]:
            self.assertEqual(self.client.get(output['previewUrl'].replace('sourceVersion=1', 'sourceVersion='+invalid)).status_code, 400)
        self.client.force_login(self.other)
        self.assertEqual(self.preview(self.work).status_code, 404)
        self.assertEqual(self.client.get(output['previewUrl']).status_code, 404)

    def test_office_conversion_retry_keeps_message_and_captured_version_binding(self):
        item, link, event, path, content = self.attachment('evidence.docx')
        file = self.client.get(path).json()['files'][0]
        from .http.office_preview import _loading
        def converting(user_id, owner_kind, object_id, lang, *, expected_identity, refresh_url):
            self.assertEqual((owner_kind, object_id), ('userLibraryObject', item.pk))
            self.assertEqual(expected_identity, {'generation': 1, 'sha256': item.sha256})
            self.assertEqual(refresh_url, file['previewUrl'])
            return _loading(item.displayName, lang, refresh_url)
        with patch('app_core.http.agent_previews._preview_selection', side_effect=converting):
            response = self.client.get(file['previewUrl'])
        self.assertEqual(response.status_code, 202)
        self.assertEqual(response['Refresh'], '1;url='+file['previewUrl'])
        self.assertEqual(parse_qs(urlsplit(file['previewUrl']).query)['sourceVersion'], ['1'])

    def test_office_selector_rejects_original_version_mismatch_without_enqueuing_a_task(self):
        item, link, event, path, content = self.attachment('evidence.docx')
        from .http.office_preview import _preview_selection
        response = _preview_selection(self.user.pk, 'userLibraryObject', item.pk, 'zh-CN',
            expected_identity={'generation': 2, 'sha256': item.sha256}, refresh_url='/bound-preview')
        self.assertEqual(response.status_code, 409)
        self.assertFalse(MaterialProcessingTask.objects.exists())
