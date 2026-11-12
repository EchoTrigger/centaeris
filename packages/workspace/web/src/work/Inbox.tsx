import { Archive, ArchiveRestore, Mail, MailOpen, X } from "lucide-react";
import { Link } from "react-router";
import { useTranslation } from "../i18n";
import { WorkBreadcrumb } from "./WorkBreadcrumb";

export type InboxItem = {
  id: string;
  title: string;
  summary: string;
  body: string;
  actor: string;
  occurredAt: string;
  readAt: string | null;
  archivedAt: string | null;
  target: { href: string; label: string } | null;
};
export type InboxView = "inbox" | "unread" | "archived";
type Props = {
  items: InboxItem[];
  view: InboxView;
  selectedId: string | null;
  onSelect: (id: string | null) => void;
  onViewChange: (view: InboxView) => void;
  onChange: (id: string, change: Pick<Partial<InboxItem>, "readAt" | "archivedAt">) => void;
};

// The view consumes notifications and acknowledgement actions independently of their provider.
export function Inbox({ items, view, selectedId, onSelect, onViewChange, onChange }: Props) {
  const { t, i18n } = useTranslation();
  const selected = items.find(item => item.id === selectedId);
  const visible = items.filter(item => view === "archived" ? item.archivedAt : !item.archivedAt && (view !== "unread" || !item.readAt));
  const date = new Intl.DateTimeFormat(i18n.resolvedLanguage, { month: "short", day: "numeric", hour: "2-digit", minute: "2-digit" });
  return <>
    <header className="workHeader"><WorkBreadcrumb current={t("work.inbox")} /></header>
    <div className={`inboxLayout${selected ? " hasSelection" : ""}`}>
      <section className="inboxList" aria-label={t("inbox.notifications")}>
        <nav className="workTabs inboxTabs" aria-label={t("inbox.views")}>
          {(["inbox", "unread", "archived"] as const).map(value => <button key={value} type="button" aria-pressed={view === value} onClick={() => onViewChange(value)}>{t(`inbox.${value}`)}</button>)}
        </nav>
        <ul>{visible.map(item => <li key={item.id}>
          <button className="inboxItem" type="button" aria-current={selectedId === item.id ? "true" : undefined} onClick={() => onSelect(item.id)}>
            <span className={`inboxReadDot${item.readAt ? " isRead" : ""}`} aria-label={t(item.readAt ? "inbox.read" : "inbox.unread")} />
            <span className="inboxItemCopy"><span className="inboxItemMeta"><span>{item.actor}</span><time dateTime={item.occurredAt}>{date.format(new Date(item.occurredAt))}</time></span>
              <strong>{item.title}</strong><span className="inboxSummary">{item.summary}</span>{item.target ? <small>{item.target.label}</small> : null}</span>
          </button>
        </li>)}</ul>
        {!visible.length ? <p className="catalogEmpty">{t(view === "archived" ? "inbox.noArchived" : "inbox.caughtUp")}</p> : null}
      </section>
      <section className="inboxDetail" aria-label={t("inbox.detail")}>
        {selected ? <>
          <div className="inboxActions">
            {selected.target ? <Link to={selected.target.href}>{selected.target.label}</Link> : null}<span />
            <button type="button" aria-label={t(selected.readAt ? "inbox.markUnread" : "inbox.markRead")} onClick={() => onChange(selected.id, { readAt: selected.readAt ? null : new Date().toISOString() })}>{selected.readAt ? <Mail /> : <MailOpen />}</button>
            <button type="button" aria-label={t(selected.archivedAt ? "inbox.restore" : "inbox.archive")} onClick={() => onChange(selected.id, { archivedAt: selected.archivedAt ? null : new Date().toISOString() })}>{selected.archivedAt ? <ArchiveRestore /> : <Archive />}</button>
            <button type="button" aria-label={t("inbox.close")} onClick={() => onSelect(null)}><X /></button>
          </div>
          <article className="inboxArticle"><p className="inboxByline">{selected.actor}<span>·</span><time dateTime={selected.occurredAt}>{date.format(new Date(selected.occurredAt))}</time></p>
            <h2>{selected.title}</h2><p className="inboxLead">{selected.summary}</p><p>{selected.body}</p>
            {selected.target ? <Link className="catalogTextLink" to={selected.target.href}>{t("inbox.openItem", { name: selected.target.label })}</Link> : null}
          </article>
        </> : <div className="inboxNoSelection"><span>{t("inbox.selectNotification")}</span></div>}
      </section>
    </div>
  </>;
}
