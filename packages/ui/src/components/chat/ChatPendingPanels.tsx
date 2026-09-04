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
  return (
    <div className="pending-request-card pending-request-drawer">
      <div className="pending-request-header">
        <span className="pending-request-title">等待补充信息</span>
        <span className="pending-request-status">待回答</span>
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
                <input
                  type={pendingQuestion.request.multiSelect ? "checkbox" : "radio"}
                  checked={selected}
                  onChange={() => onOptionToggle(option)}
                  disabled={pendingQuestion.submitting}
                />
                <span>{option}</span>
              </label>
            );
          })}
        </div>
      ) : null}
      <textarea
        className="pending-request-command"
        value={pendingQuestion.answerText}
        placeholder="可补充文字回答"
        onChange={(event) => onTextChange(event.target.value)}
        disabled={pendingQuestion.submitting}
      />
      {pendingQuestionError.trim() ? (
        <div className="tool-timeline-error">{pendingQuestionError.trim()}</div>
      ) : null}
      <div className="pending-request-actions">
        <button
          type="button"
          className="pending-request-btn primary"
          onClick={onSubmit}
          disabled={pendingQuestion.submitting}
        >
          {pendingQuestion.submitting ? "提交中..." : "提交回答"}
        </button>
      </div>
    </div>
  );
}
