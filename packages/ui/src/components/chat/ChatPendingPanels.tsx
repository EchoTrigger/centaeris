import { useId } from "react";
import { Button } from "../ui/button";
import { Textarea } from "../ui/textarea";
import { Checkbox } from "../ui/checkbox";
import { t } from "../../i18n";
import type { PendingQuestionState } from "./types";

type PendingQuestionPanelProps = {
  pendingQuestion: PendingQuestionState;
  pendingQuestionError: string;
  onOptionToggle: (option: string) => void;
  onTextChange: (value: string) => void;
  onSubmit: () => void;
};

export function PendingQuestionPanel({
  pendingQuestion,
  pendingQuestionError,
  onOptionToggle,
  onTextChange,
  onSubmit,
}: PendingQuestionPanelProps) {
  const optionsName = useId();
  return (
    <div className="pending-request-card pending-request-drawer desktopPendingQuestion" role="group" aria-label="Question from agent" aria-busy={pendingQuestion.submitting}>
      <div className="pending-request-header">
        <span className="pending-request-title">{t("chatPendingPanels.waitingForMoreInformation")}</span>
        <span className="pending-request-status">{t("chatPendingPanels.awaitingAnswer")}</span>
      </div>
      <div className="pending-request-desc">
        {pendingQuestion.request.question}
      </div>
      {pendingQuestion.request.options.length > 0 ? (
        <div className="action-inline-selection">
          {pendingQuestion.request.options.map((option) => {
            const selected = pendingQuestion.selectedOptions.includes(option);
            return (
              <label className="action-inline-selection-row" key={option}>
                {pendingQuestion.request.multiSelect ? <Checkbox checked={selected} onCheckedChange={checked => { if (checked !== selected) onOptionToggle(option); }} disabled={pendingQuestion.submitting} /> : <input type="radio" name={optionsName} checked={selected} onChange={event => { if (event.target.checked) onOptionToggle(option); }} disabled={pendingQuestion.submitting} />}
                <span>{option}</span>
              </label>
            );
          })}
        </div>
      ) : null}
      <Textarea
        aria-label="Additional answer"
        className="pending-request-command"
        value={pendingQuestion.answerText}
        placeholder={t("chatPendingPanels.youCanAddAWrittenAnswer")}
        onChange={(event) => onTextChange(event.target.value)}
        disabled={pendingQuestion.submitting}
      />
      {pendingQuestionError.trim() ? (
        <div className="tool-timeline-error" role="alert">{pendingQuestionError.trim()}</div>
      ) : null}
      <div className="pending-request-actions">
        <Button size="sm"
          type="button"
          className="desktopQuestionSubmit"
          onClick={onSubmit}
          disabled={pendingQuestion.submitting}
        >
          {pendingQuestion.submitting ? t("chatPendingPanels.submitting") : t("chatPendingPanels.submitAnswer")}
        </Button>
      </div>
    </div>
  );
}
