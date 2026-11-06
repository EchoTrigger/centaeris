import { useRef, useState } from "react";
import { Pencil } from "lucide-react";
import { useRevalidator } from "react-router";
import { apiJson, jsonOptions, requireWorkspaces } from "../api";
import { WorkspaceAvatar } from "../components/WorkspaceAvatar";
import { WorkspaceDialog } from "../components/WorkspaceDialog";
import { useTranslation } from "../i18n";

const AVATAR_TYPES = new Set(["image/png", "image/jpeg", "image/webp"]);

export default function WorkspaceProfileSettings({ workspace }) {
  const { t } = useTranslation();
  const revalidator = useRevalidator();
  const inputRef = useRef(null);
  const [editor, setEditor] = useState(null);
  const [name, setName] = useState(workspace.name);
  const [avatarDataUrl, setAvatarDataUrl] = useState(undefined);
  const [saved, setSaved] = useState(workspace);
  const [busy, setBusy] = useState(false);
  const [reading, setReading] = useState(false);
  const [error, setError] = useState("");
  const [notice, setNotice] = useState("");
  const avatarUrl = avatarDataUrl === undefined ? saved.avatarUrl : avatarDataUrl;
  const editingName = editor === "name";
  const dirty = editingName ? name.trim() !== saved.name : avatarUrl !== saved.avatarUrl;

  function openEditor(field) {
    setName(saved.name);
    setAvatarDataUrl(undefined);
    setError("");
    setNotice("");
    setEditor(field);
  }

  function closeEditor() {
    setEditor(null);
    setAvatarDataUrl(undefined);
  }

  async function selectAvatar(event) {
    const file = event.currentTarget.files?.[0];
    event.currentTarget.value = "";
    if (!file) return;
    setError("");
    setNotice("");
    if (!AVATAR_TYPES.has(file.type) || file.size > 1024 * 1024) {
      setError(t("workspaceProfile.avatarInvalid"));
      return;
    }
    setReading(true);
    try {
      const dataUrl = await new Promise((resolve, reject) => {
        const reader = new FileReader();
        reader.onload = () => resolve(reader.result);
        reader.onerror = () => reject(reader.error);
        reader.readAsDataURL(file);
      });
      setAvatarDataUrl(dataUrl);
    } catch {
      setError(t("workspaceProfile.avatarReadFailed"));
    } finally {
      setReading(false);
    }
  }

  async function submit(event) {
    event.preventDefault();
    if (!dirty || busy || reading || !name.trim()) return;
    setBusy(true);
    setError("");
    setNotice("");
    try {
      const result = await apiJson(`/api/workspaces/${encodeURIComponent(workspace.id)}/profile`, jsonOptions("PATCH", {
        name: name.trim(), ...(avatarDataUrl === undefined ? {} : { avatarDataUrl }),
      }));
      const [updated] = requireWorkspaces([result.workspace]);
      if (updated.id !== workspace.id) throw new Error("workspace_identity_invalid");
      setSaved(updated);
      setName(updated.name);
      await revalidator.revalidate();
      closeEditor();
      setNotice(t("workspaceProfile.saved"));
    } catch {
      setError(t("workspaceProfile.saveFailed"));
    } finally {
      setBusy(false);
    }
  }

  return <div className="preferenceSettings">
    <header className="settingsPageHeading"><h1>{t("workspaceProfile.title")}</h1></header>
    <div className="settingsPanel settingsListPanel">
      <div className="settingsEditRow">
        <span>{t("workspaceProfile.avatar")}</span>
        <div className="settingsEditValue">
          <WorkspaceAvatar workspace={saved} className="shWsAvatar workspaceProfileAvatar" />
          <button className="settingsEditIcon" type="button" aria-label={t("settingsRoute.editSetting", { setting: t("workspaceProfile.avatar") })} onClick={() => openEditor("avatar")}><Pencil aria-hidden="true" /></button>
        </div>
      </div>
      <div className="settingsEditRow">
        <span>{t("workspaceProfile.name")}</span>
        <div className="settingsEditValue">
          <span>{saved.name}</span>
          <button className="settingsEditIcon" type="button" aria-label={t("settingsRoute.editSetting", { setting: t("workspaceProfile.name") })} onClick={() => openEditor("name")}><Pencil aria-hidden="true" /></button>
        </div>
      </div>
      {notice ? <p className="settingsEditNotice" role="status">{notice}</p> : null}
    </div>
    {editor ? <WorkspaceDialog className="settingsEditDialog" title={t(editingName ? "workspaceProfile.name" : "workspaceProfile.avatar")} busy={busy || reading} onClose={closeEditor}>
      <form onSubmit={submit}>
        {editingName ? <label>
          {t("workspaceProfile.name")}
          <input autoFocus value={name} onChange={event => { setName(event.target.value); setError(""); }} maxLength={160} required disabled={busy} />
        </label> : <>
          <div className="workspaceProfileAvatarActions">
            <WorkspaceAvatar workspace={{ name: saved.name, avatarUrl }} className="shWsAvatar workspaceProfileAvatar" />
            <button className="secondary" type="button" disabled={busy || reading} onClick={() => inputRef.current?.click()}>{t("workspaceProfile.upload")}</button>
            {avatarUrl ? <button className="secondary" type="button" disabled={busy || reading} onClick={() => { setAvatarDataUrl(null); setError(""); }}>{t("workspaceProfile.remove")}</button> : null}
          </div>
          <small>{t("workspaceProfile.avatarHint")}</small>
          <input hidden ref={inputRef} type="file" aria-label={t("workspaceProfile.upload")} accept="image/png,image/jpeg,image/webp" disabled={busy || reading} onChange={selectAvatar} />
        </>}
        {error ? <p className="settingsEditError" role="alert">{error}</p> : null}
        <footer>
          <button className="secondary" type="button" disabled={busy || reading} onClick={closeEditor}>{t("agentRunRow.cancel")}</button>
          <button className="isPrimary" type="submit" disabled={!dirty || !name.trim() || busy || reading}>{busy ? t("workspaceProfile.saving") : t("modelSettings.save")}</button>
        </footer>
      </form>
    </WorkspaceDialog> : null}
  </div>;
}
