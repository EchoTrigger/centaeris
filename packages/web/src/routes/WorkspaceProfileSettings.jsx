import { useRef, useState } from "react";
import { useRevalidator } from "react-router";
import { apiJson, jsonOptions, requireWorkspaces } from "../api";
import { WorkspaceAvatar } from "../components/WorkspaceAvatar";
import { useTranslation } from "../i18n";

const AVATAR_TYPES = new Set(["image/png", "image/jpeg", "image/webp"]);

export default function WorkspaceProfileSettings({ workspace, onClose }) {
  const { t } = useTranslation();
  const revalidator = useRevalidator();
  const inputRef = useRef(null);
  const [name, setName] = useState(workspace.name);
  const [avatarDataUrl, setAvatarDataUrl] = useState(undefined);
  const [saved, setSaved] = useState(workspace);
  const [busy, setBusy] = useState(false);
  const [reading, setReading] = useState(false);
  const [error, setError] = useState("");
  const [notice, setNotice] = useState("");
  const avatarUrl = avatarDataUrl === undefined ? saved.avatarUrl : avatarDataUrl;
  const dirty = name.trim() !== saved.name || avatarUrl !== saved.avatarUrl;

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
      setAvatarDataUrl(undefined);
      await revalidator.revalidate();
      setNotice(t("workspaceProfile.saved"));
    } catch {
      setError(t("workspaceProfile.saveFailed"));
    } finally {
      setBusy(false);
    }
  }

  return <div className="accountSecuritySettings">
    <header><h1>{t("workspaceProfile.title")}</h1></header>
    <form className="accountSecurityForm workspaceProfileForm" onSubmit={submit}>
      <div className="workspaceProfileAvatarRow">
        <WorkspaceAvatar workspace={{ name, avatarUrl }} className="shWsAvatar workspaceProfileAvatar" />
        <div>
          <strong>{t("workspaceProfile.avatar")}</strong>
          <small>{t("workspaceProfile.avatarHint")}</small>
          <div className="workspaceProfileAvatarActions">
            <button type="button" disabled={busy || reading} onClick={() => inputRef.current?.click()}>{t("workspaceProfile.upload")}</button>
            {avatarUrl ? <button type="button" disabled={busy || reading} onClick={() => { setAvatarDataUrl(null); setError(""); setNotice(""); }}>{t("workspaceProfile.remove")}</button> : null}
          </div>
        </div>
      </div>
      <input hidden ref={inputRef} type="file" accept="image/png,image/jpeg,image/webp" disabled={busy || reading} onChange={selectAvatar} />
      <label>{t("workspaceProfile.name")}<input value={name} onChange={event => { setName(event.target.value); setError(""); setNotice(""); }} maxLength={160} required disabled={busy} /></label>
      {error ? <p className="accountSecurityError" role="alert">{error}</p> : null}
      {notice ? <p className="accountSecurityNotice" role="status">{notice}</p> : null}
      <div className="accountSecurityActions workspaceProfileActions">
        <button type="button" disabled={busy} onClick={onClose}>{t("agentRunRow.cancel")}</button>
        <button type="submit" disabled={!dirty || !name.trim() || busy || reading}>{busy ? t("workspaceProfile.saving") : t("workspaceProfile.save")}</button>
      </div>
    </form>
  </div>;
}
