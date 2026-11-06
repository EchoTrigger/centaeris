import { t, useTranslation } from "../i18n";
import { useState } from "react";
import { createPortal } from "react-dom";
import { FileText, LockKeyhole, Search, Upload, X } from "lucide-react";
import { useModalDialog } from "../components/useModalDialog";

const NOTE_TEMPLATES = () => ([
  { name: t("shellSidebar.taskList"), description: t("shellSidebar.trackTasksStatusAndNextSteps"), preview: [t("shellSidebar.toDo"), t("shellSidebar.inProgress"), t("shellSidebar.completed")], markdown: t("shellSidebar.taskListFirstTask") },
  { name: t("shellSidebar.projectNotes"), description: t("shellSidebar.organizeGoalsProgressAndKeyDecisions"), preview: [t("shellSidebar.goals"), t("shellSidebar.progress"), t("shellSidebar.nextSteps")], markdown: t("shellSidebar.projectNotesGoalsProgressDecisions") },
  { name: t("shellSidebar.researchNotes"), description: t("shellSidebar.collectQuestionsSourcesAndInterimFindings"), preview: [t("shellSidebar.questions"), t("libraryRoute.source"), t("shellSidebar.findings")], markdown: t("shellSidebar.researchNotesQuestionsSourcesFindings") },
  { name: t("shellSidebar.meetingNotes"), description: t("shellSidebar.recordTopicsDecisionsAndActionItems"), preview: [t("shellSidebar.topics"), t("shellSidebar.findings"), t("shellSidebar.actionItems")], markdown: t("shellSidebar.meetingNotesTopicsDecisionsActionItems") },
  { name: t("shellSidebar.decisionRecord"), description: t("shellSidebar.keepTheContextReasoningAndFollowUpImplications"), preview: [t("shellSidebar.context"), t("shellSidebar.decision"), t("shellSidebar.reasoning")], markdown: t("shellSidebar.decisionRecordContextDecisionReasoningFollowUp") },
  { name: t("shellSidebar.weeklyPlan"), description: t("shellSidebar.planYourPrioritiesAndReviewProgressThroughoutTheWeek"), preview: [t("shellSidebar.thisWeekSPriorities"), t("shellSidebar.tasks"), t("shellSidebar.review")], markdown: t("shellSidebar.weeklyPlanThisWeekSPrioritiesTasksReview") },
  { name: t("shellSidebar.readingList"), description: t("shellSidebar.organizeWhatToReadAndWhatYouLearn"), preview: [t("shellSidebar.toRead"), t("shellSidebar.reading"), t("shellSidebar.completed")], markdown: t("shellSidebar.readingListToReadReadingNotes") },
  { name: t("shellSidebar.contentOutline"), description: t("shellSidebar.organizeContentByTopicStructureAndMaterials"), preview: [t("shellSidebar.topic"), t("shellSidebar.structure"), t("shellSidebar.resources")], markdown: t("shellSidebar.contentOutlineTopicStructureResources") },
]);

export function LibraryTemplatesDialog({ onClose, onCreateNote, onUpload }) {
  const { t } = useTranslation();
  const [query, setQuery] = useState("");
  const dialogRef = useModalDialog({ onClose });
  const normalizedQuery = query.trim().toLocaleLowerCase();
  const visibleTemplates = NOTE_TEMPLATES().filter((template) => !normalizedQuery || `${template.name}${template.description}`.toLocaleLowerCase().includes(normalizedQuery));

  return createPortal(<div className="shPrivateCreateBackdrop" role="presentation" onMouseDown={onClose}>
    <section className="shPrivateCreateDialog" ref={dialogRef} role="dialog" aria-modal="true" aria-label={t("shellSidebar.addPrivateContent")} tabIndex={-1} onMouseDown={(event) => event.stopPropagation()}>
      <header className="shPrivateCreateHeader">
        <div><button className="quietCloseButton" type="button" aria-label={t("shellSidebar.closeAddMenu")} onClick={onClose}><X aria-hidden="true" /></button><span>{t("libraryRoute.addTo")}</span><strong><LockKeyhole aria-hidden="true" />{t("agentRoute.private")}</strong></div>
        <label><Search aria-hidden="true" /><input autoFocus aria-label={t("shellSidebar.searchTemplates")} placeholder={t("libraryRoute.search")} value={query} onChange={(event) => setQuery(event.target.value)} /></label>
      </header>
      <div className="shPrivateCreateScroll">
        <div className="shPrivateCreateContent">
          <div className="shPrivateCreateQuick">
            <button type="button" onClick={() => onCreateNote({ name: "Untitled", markdown: "" })}><FileText aria-hidden="true" /><strong>{t("shellSidebar.blankNote")}</strong></button>
            <button type="button" onClick={onUpload}><Upload aria-hidden="true" /><strong>{t("shellSidebar.uploadMaterials")}</strong></button>
          </div>
          <h2><FileText aria-hidden="true" />{t("shellSidebar.templates")}</h2>
          <div className="shPrivateTemplateGrid">
            {visibleTemplates.map((template) => <button className="shPrivateTemplateCard" type="button" key={template.name} onClick={() => onCreateNote(template)}>
              <span><strong>{template.name}</strong><small>{template.description}</small></span>
              <div className="shPrivateTemplatePreview" aria-hidden="true"><strong>{template.name}</strong>{template.preview.map((label) => <i key={label}><span /><em>{label}</em></i>)}</div>
            </button>)}
            {!visibleTemplates.length ? <p>{t("shellSidebar.noMatchingTemplates")}</p> : null}
          </div>
        </div>
      </div>
    </section>
  </div>, document.body);
}
