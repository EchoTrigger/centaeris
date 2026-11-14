import { memo } from "react";
import {
  ArrowUp,
  LoaderCircle,
  Plus,
  SlidersHorizontal,
  Square,
} from "lucide-react";
import { useTranslation } from "../i18n";
import { ContextUsagePicker } from "../components/ContextUsagePicker";
import { ModelPicker, ThinkingPicker } from "../components/ModelSelectionPickers";
import { AttachmentCard, LocalAttachmentCard, localAttachmentKey } from "./AttachmentCard";

export const WorkspaceComposer = memo(function WorkspaceComposer({
  formRef,
  fileInputRef,
  isHome,
  onSubmit,
  pendingAttachments,
  pendingUploadFiles,
  onPreviewAttachment,
  onRemoveAttachment,
  onRemoveUpload,
  draft,
  feedback,
  onDraftChange,
  enterStartsNewLine,
  sending,
  hasActiveAgentRun,
  uploadingAttachment,
  onUploadAttachment,
  sessionId,
  contextRevision,
  currentModel,
  thinkingMode,
  onThinkingModeChange,
  modelGroups,
  modelId,
  onModelIdChange,
  cancellingAgentRunId,
  controlBusy = false,
  onCancelActiveAgentRun,
}) {
  const { t } = useTranslation();
  const showStop = hasActiveAgentRun && !draft.trim();
  return (
    <form ref={formRef} className={`workspaceComposer ${isHome ? "shComposerHero" : ""}`} onSubmit={onSubmit}>
      {pendingAttachments.length || pendingUploadFiles.length ? (
        <div className="workspaceComposerAttachments" aria-label={t("appRoute.referenceMaterialsForThisConversation")}>
          {pendingAttachments.map((link) => (
            <AttachmentCard className="workspaceComposerAttachment" attachment={link} onPreview={() => onPreviewAttachment(link)} onRemove={() => onRemoveAttachment(link)} key={link.id} />
          ))}
          {pendingUploadFiles.map((file, index) => (
            <LocalAttachmentCard file={file} onRemove={() => onRemoveUpload(index)} key={localAttachmentKey(file)} />
          ))}
        </div>
      ) : null}
      <label className="srOnly" htmlFor="messageDraft">{t("appRoute.message")}</label>
      <textarea
        id="messageDraft"
        autoFocus={isHome}
        value={draft}
        onChange={(event) => onDraftChange(event.target.value)}
        onKeyDown={(event) => {
          if (event.key !== "Enter" || event.nativeEvent.isComposing) return;
          const shouldSend = enterStartsNewLine ? event.metaKey || event.ctrlKey : !event.shiftKey;
          if (shouldSend) {
            event.preventDefault();
            event.currentTarget.form?.requestSubmit();
          }
        }}
        placeholder={t(isHome ? "appRoute.describeATask" : "appRoute.reply")}
      />
      {feedback}
      <div className="workspaceComposerFooter">
        <div className="workspaceComposerControlGroup">
          <span className="workspaceComposerControl">
            <button
              className="workspaceComposerIconButton"
              type="button"
              onClick={() => fileInputRef.current?.click()}
              disabled={hasActiveAgentRun || sending || uploadingAttachment}
              aria-label={t("appRoute.add")}
            >
              {uploadingAttachment ? <LoaderCircle className="statusIcon" aria-hidden="true" /> : <Plus aria-hidden="true" />}
            </button>
          </span>
          <span className="workspaceComposerControl isPending" role="img" aria-label={t("appRoute.settingsComingSoon2")}>
            <SlidersHorizontal aria-hidden="true" />
          </span>
          <ContextUsagePicker sessionId={sessionId} contextRevision={contextRevision} />
        </div>
        <input ref={fileInputRef} className="srOnly" type="file" multiple aria-label={t("appRoute.selectOneOrMoreMaterials")} onChange={onUploadAttachment} />
        <div className="workspaceComposerControlGroup isRuntime">
          <span className="workspaceComposerControl">
            <ThinkingPicker model={currentModel} value={thinkingMode} onChange={onThinkingModeChange} disabled={!currentModel?.thinkingModes?.length || sending || hasActiveAgentRun} />
          </span>
          <span className="workspaceComposerControl">
            <ModelPicker groups={modelGroups} model={currentModel} value={modelId} onChange={onModelIdChange} disabled={!modelGroups.length || sending} />
          </span>
          <span className="workspaceComposerControl">
            {showStop ? (
              <button
                className="workspaceSendButton"
                type="button"
                aria-label={t("appRoute.stop")}
                aria-busy={controlBusy || Boolean(cancellingAgentRunId)}
                onClick={onCancelActiveAgentRun}
              >
                {cancellingAgentRunId ? <LoaderCircle className="statusIcon" aria-hidden="true" /> : <Square aria-hidden="true" />}
              </button>
            ) : (
              <button
                className="workspaceSendButton"
                type="submit"
                aria-label={t("appRoute.send")}
                aria-busy={controlBusy || sending}
              >
                {sending ? <LoaderCircle className="statusIcon" aria-hidden="true" /> : <ArrowUp aria-hidden="true" />}
              </button>
            )}
          </span>
        </div>
      </div>
    </form>
  );
});
