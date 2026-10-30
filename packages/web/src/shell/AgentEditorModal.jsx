
import { useTranslation } from "../i18n";
import { useEffect, useRef, useState } from "react";
import { FileText, Lock, Pencil, Upload, X } from "lucide-react";
import { useModalDialog } from "../components/useModalDialog";
import { AgentMark } from "./AgentMark";

const AVATAR_CHOICES = [
  ["centaeris", "Centaeris"],
  ["banana", "Banana"],
];

export function AgentEditorModal({ agent, heading, submitLabel, busy = false, error = "", onClose, onSave }) {
  const { t } = useTranslation();
  const [view, setView] = useState("profile");
  const [name, setName] = useState(agent.name || "");
  const [description, setDescription] = useState(agent.description || "");
  const [instructions, setInstructions] = useState(agent.instructions || "");
  const [avatarKind, setAvatarKind] = useState(agent.avatarKind);
  const [avatarDataBase64, setAvatarDataBase64] = useState(undefined);
  const [avatarPreview, setAvatarPreview] = useState("");
  const [avatarError, setAvatarError] = useState("");
  const [readingAvatar, setReadingAvatar] = useState(false);
  const avatarInputRef = useRef(null);
  const avatarReaderRef = useRef(null);
  const hasCustomAvatar = Boolean(avatarPreview || (avatarDataBase64 === undefined && agent.avatarUrl));
  useEffect(() => () => {
    const reader = avatarReaderRef.current;
    avatarReaderRef.current = null;
    reader?.abort();
  }, []);
  const [confirmingClose, setConfirmingClose] = useState(false);
  const continueEditingRef = useRef(null);
  const dirty = name !== (agent.name || "")
    || description !== (agent.description || "")
    || instructions !== (agent.instructions || "")
    || avatarKind !== agent.avatarKind || avatarDataBase64 !== undefined;
  const dialogRef = useModalDialog({
    focusKey: view,
    busy,
    onClose: () => view === "instructions" ? setView("profile") : confirmingClose ? setConfirmingClose(false) : requestClose(),
  });

  useEffect(() => {
    if (confirmingClose) continueEditingRef.current?.focus();
  }, [confirmingClose]);

  function requestClose() {
    if (busy) return;
    if (dirty) setConfirmingClose(true);
    else onClose();
  }

  function submit(event) {
    event.preventDefault();
    const trimmedName = name.trim();
    if (!trimmedName || readingAvatar || busy) return;
    onSave({
      name: trimmedName,
      description: description.trim(),
      instructions: instructions.trim(),
      avatarKind,
      ...(avatarDataBase64 === undefined ? {} : { avatarDataBase64 }),
    });
  }

  function cancelAvatarRead() {
    const reader = avatarReaderRef.current;
    avatarReaderRef.current = null;
    reader?.abort();
    setReadingAvatar(false);
  }

  function chooseAvatar(kind) {
    cancelAvatarRead();
    setAvatarKind(kind);
    setAvatarDataBase64(agent.avatarUrl ? null : undefined);
    setAvatarPreview("");
    setAvatarError("");
  }

  function selectAvatar(event) {
    const file = event.target.files?.[0];
    event.target.value = "";
    if (!file || busy) return;
    cancelAvatarRead();
    if (file.size > 1024 * 1024 || !["image/png", "image/jpeg", "image/webp"].includes(file.type)) {
      setAvatarError(t("agentEditorModal.avatarInvalid"));
      return;
    }
    setAvatarError("");
    setReadingAvatar(true);
    const reader = new FileReader();
    avatarReaderRef.current = reader;
    reader.onload = () => {
      if (avatarReaderRef.current !== reader) return;
      avatarReaderRef.current = null;
      setReadingAvatar(false);
      const result = reader.result;
      if (typeof result !== "string" || !result.startsWith(`data:${file.type};base64,`)) {
        setAvatarError(t("agentEditorModal.avatarInvalid"));
        return;
      }
      setAvatarDataBase64(result.slice(result.indexOf(",") + 1));
      setAvatarPreview(result);
    };
    reader.onerror = () => {
      if (avatarReaderRef.current !== reader) return;
      avatarReaderRef.current = null;
      setReadingAvatar(false);
      setAvatarError(t("agentEditorModal.avatarInvalid"));
    };
    reader.readAsDataURL(file);
  }

  function reset() {
    cancelAvatarRead();
    setAvatarDataBase64(undefined);
    setAvatarPreview("");
    setAvatarError("");
    setName(agent.name || "");
    setDescription(agent.description || "");
    setInstructions(agent.instructions || "");
    setAvatarKind(agent.avatarKind);
    setConfirmingClose(false);
    setView("profile");
  }

  if (view === "instructions") {
    return (
      <section
        className="shSoulDocumentPage libraryPreviewMain"
        ref={dialogRef}
        role="dialog"
        aria-modal="true"
        tabIndex={-1}
        aria-label={`${heading} SOUL.md`}
      >
        <header className="libraryPreviewHeader">
          <nav className="libraryNoteIdentity" aria-label={t("agentEditorModal.soulMdPath")}>
            <button type="button" onClick={() => setView("profile")}>{t("agentRunRow.agent")}</button>
            <span aria-hidden="true">/</span>
            <strong translate="no">SOUL.md</strong>
            <small><Lock aria-hidden="true" />{t("agentRoute.private")}</small>
          </nav>
          <button className="shSoulDocumentDone" type="button" onClick={() => setView("profile")}>{t("agentEditorModal.finishEditing")}</button>
        </header>
        <section className="libraryPreviewBody libraryNotePreview">
          <div className="libraryNoteEditor">
            <label className="srOnly" htmlFor="agentInstructions">{t("agentEditorModal.agentInstructions")}</label>
            <textarea
              id="agentInstructions"
              name="agentInstructions"
              autoComplete="off"
              maxLength={16000}
              value={instructions}
              onChange={(event) => setInstructions(event.target.value)}
              placeholder={t("agentEditorModal.identityAndResponsibilitiesDescribeHowThisAgentShouldWork")}
              aria-label={t("agentEditorModal.agentInstructions")}
            />
          </div>
        </section>
      </section>
    );
  }

  return (
    <div className="shModalBackdrop" role="presentation" onMouseDown={requestClose}>
      <form
        className="shAgentCreate"
        ref={dialogRef}
        role="dialog"
        aria-modal="true"
        aria-label={heading}
        tabIndex={-1}
        onSubmit={submit}
        onMouseDown={(event) => event.stopPropagation()}
      >
        <button className="shModalClose quietCloseButton" type="button" disabled={busy} onClick={requestClose} aria-label={t("workspaceContextPanel.close")}><X aria-hidden="true" /></button>

        <h1>{heading}</h1>
            <div className="shAgentIdentity">
              <button className="shAgentAvatarEdit" type="button" disabled={busy} onClick={() => avatarInputRef.current?.click()} aria-label={t("agentEditorModal.uploadAvatar")} title={t("agentEditorModal.uploadAvatar")}>
                <AgentMark className="shAgentHeroMark" agent={{ avatarKind, avatarUrl: avatarDataBase64 === undefined ? agent.avatarUrl : null }} previewSrc={avatarPreview || undefined} />
                <span><Pencil aria-hidden="true" /></span>
              </button>
              <input
                className="shAgentNameInput"
                name="agentName"
                autoComplete="off"
                maxLength={255}
                value={name}
                onChange={(event) => setName(event.target.value)}
                placeholder={t("agentEditorModal.nameYourAgent")}
                aria-label={t("agentEditorModal.agentName")}
              />
              <span className="shAgentPrivate"><Lock aria-hidden="true" />{t("agentRoute.onlyYouCanSeeThis")}</span>
            </div>

            <fieldset className="shAvatarChoices">
              <legend>{t("agentEditorModal.chooseAvatar")}</legend>
              {AVATAR_CHOICES.map(([kind, label]) => (
                <button
                  className={!hasCustomAvatar && avatarKind === kind ? "isActive" : ""}
                  type="button"
                  aria-pressed={!hasCustomAvatar && avatarKind === kind}
                  disabled={busy}
                  onClick={() => chooseAvatar(kind)}
                  key={kind}
                >
                  <AgentMark agent={{ avatarKind: kind }} />
                  <span>{label}</span>
                </button>
              ))}
            </fieldset>

            <div className="shAvatarUpload">
              <input ref={avatarInputRef} type="file" accept="image/png,image/jpeg,image/webp" aria-label={t("agentEditorModal.uploadAvatar")} hidden disabled={busy} onChange={selectAvatar} />
              <div><button type="button" disabled={busy} onClick={() => avatarInputRef.current?.click()}><Upload aria-hidden="true" />{t("agentEditorModal.uploadAvatar")}</button>{hasCustomAvatar ? <button type="button" disabled={busy} onClick={() => chooseAvatar("centaeris")}>{t("agentEditorModal.restoreAvatar")}</button> : null}</div>
              <small>{t("agentEditorModal.avatarHint")}</small>
              {avatarError ? <p className="errorBanner" role="alert">{avatarError}</p> : null}
              <span className="srOnly" role="status" aria-live="polite">{readingAvatar ? t("agentEditorModal.readingAvatar") : ""}</span>
            </div>

            <label className="shCreateLabel" htmlFor="agentDescription">{t("agentEditorModal.overview")}</label>
            <input
              className="shAgentDescriptionInput"
              id="agentDescription"
              name="agentDescription"
              autoComplete="off"
              maxLength={128}
              value={description}
              onChange={(event) => setDescription(event.target.value)}
              placeholder={t("agentEditorModal.describeItsResponsibilitiesInOneSentence")}
              aria-label={t("agentEditorModal.agentDescription")}
            />

            <section className="shInstructionsSection" aria-labelledby="agentInstructionsHeading">
              <small id="agentInstructionsHeading">Instructions</small>
              <button className="shSoulCard" type="button" onClick={() => setView("instructions")} aria-label={t("agentEditorModal.editSoulMd")}>
                <span className="shSoulCardIcon"><FileText aria-hidden="true" /></span>
                <span className="shSoulCardCopy">
                  <strong translate="no">SOUL.md</strong>
                  <small>{instructions.trim().split("\n")[0] || t("agentRoute.noInstructionsYetTheAgentWillUseItsDefault")}</small>
                </span>
                <span className="shSoulCardAction"><Pencil aria-hidden="true" />{t("agentRunRow.edit")}</span>
              </button>
            </section>

            {error ? <div className="errorBanner" role="alert">{error}</div> : null}
            {confirmingClose ? <footer className="shAgentDiscardPrompt" role="alert">
              <span><strong>{t("agentEditorModal.discardUnsavedChanges")}</strong><small>{t("agentEditorModal.yourNameDescriptionAvatarAndSoulMdDraftsWill")}</small></span>
              <button ref={continueEditingRef} type="button" onClick={() => setConfirmingClose(false)}>{t("agentEditorModal.continueEditing")}</button>
              <button className="isDanger" type="button" onClick={onClose}>{t("agentEditorModal.discardChanges")}</button>
            </footer> : <footer>
              <span className="srOnly" role="status" aria-live="polite">{busy ? t("agentEditorModal.savingAgent") : ""}</span>
              <button type="button" disabled={busy} onClick={reset}>{t("agentEditorModal.reset")}</button>
              <button className="shPrimaryButton" type="submit" disabled={busy || readingAvatar || !name.trim()}>{busy ? t("modelSettings.saving") : submitLabel}</button>
            </footer>}
      </form>
    </div>
  );
}
