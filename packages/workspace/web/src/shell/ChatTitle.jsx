import { useEffect, useRef, useState } from "react";
import { Menu } from "@centaeris/ui/primitives/menu";
import { Archive, Clipboard, Ellipsis, Pencil, Star } from "lucide-react";
import { useTranslation } from "../i18n";
import { createWorkspaceTranscriptTransport } from "../chat/transcriptTransport";
import { readChatMarkdown } from "../chat/chatMarkdown";

export function ChatTitle({ session, actions, running, titleControl }) {
  const { t } = useTranslation();
  const trigger = useRef(null);
  const copyingRequest = useRef(null);
  const [renaming, setRenaming] = useState(false);
  const [copyState, setCopyState] = useState("");
  useEffect(() => () => copyingRequest.current?.abort(), []);

  function finishRename(value) {
    setRenaming(false);
    if (value !== undefined) void actions.renameSession(session, value);
    window.requestAnimationFrame(() => trigger.current?.focus());
  }

  async function copy() {
    const controller = new AbortController(); copyingRequest.current = controller;
    setCopyState("copying");
    try {
      const markdown = await readChatMarkdown(createWorkspaceTranscriptTransport(), session, controller.signal);
      await navigator.clipboard.writeText(markdown);
      if (!controller.signal.aborted) setCopyState("copied");
    } catch {
      if (!controller.signal.aborted) setCopyState("copyFailed");
    } finally {
      if (copyingRequest.current === controller) copyingRequest.current = null;
    }
  }

  if (renaming) return <form className="workspaceConversationRename" onSubmit={event => { event.preventDefault(); finishRename(event.currentTarget.elements.title.value); }}>
    <input name="title" aria-label={t("appRoute.renameValue", { value1: session.title })} autoFocus maxLength={200}
      defaultValue={session.title} onFocus={event => event.currentTarget.select()}
      onBlur={event => finishRename(event.currentTarget.value)} onKeyDown={event => {
        if (event.nativeEvent.isComposing) return;
        if (event.key === "Escape") { event.preventDefault(); finishRename(undefined); }
      }} />
  </form>;

  return <div className="workspaceConversationHeading">
    <h1 className="workspaceConversationTitle">{titleControl}</h1>
    <button className="workspacePanelToggle chatFavorite" type="button" aria-label={t(session.isPinned ? "chat.unfavorite" : "chat.favorite")}
      aria-pressed={session.isPinned} disabled={Boolean(actions.savingSessionId)} onClick={() => actions.onTogglePin(session)}><Star aria-hidden="true" /></button>
    <Menu.Root onOpenChange={open => { if (open && copyState !== "copying") setCopyState(""); }}>
      <Menu.Trigger ref={trigger} className="workspacePanelToggle" aria-label={t("chat.actions", { title: session.title })}><Ellipsis aria-hidden="true" /></Menu.Trigger>
      <Menu.Portal><Menu.Positioner className="chatActionsPositioner" align="start" sideOffset={5}>
        <Menu.Popup className="chatActionsMenu" finalFocus={!renaming}>
          <Menu.Item disabled={Boolean(actions.savingSessionId)} onClick={() => setRenaming(true)}><Pencil aria-hidden="true" />{t("appRoute.rename")}</Menu.Item>
          <Menu.Item closeOnClick={false} disabled={copyState === "copying"} onClick={() => void copy()}><Clipboard aria-hidden="true" />{t(copyState === "copying" || copyState === "copied" ? `chat.${copyState}` : "chat.copyMarkdown")}</Menu.Item>
          {copyState === "copyFailed" ? <p role="alert">{t("chat.copyFailed")}</p> : null}
          <Menu.Separator />
          <Menu.Item disabled={running || Boolean(actions.archivingSessionId)} onClick={() => void actions.onArchive(session)}><Archive aria-hidden="true" />{t("archive.title")}</Menu.Item>
        </Menu.Popup>
      </Menu.Positioner></Menu.Portal>
    </Menu.Root>
  </div>;
}
