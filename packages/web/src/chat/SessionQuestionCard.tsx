import { useRef, useState } from "react";
import { ArrowRight, MessageCircleQuestion, Pencil, X } from "lucide-react";
import { useTranslation } from "../i18n";

export type SessionQuestion = Readonly<{ question: string; options: readonly string[] }>;
export type SessionQuestionAnswer = Readonly<{ action: "answer"; answer: string } | { action: "skip" }>;

export function SessionQuestionCard({ question, onAnswer }: Readonly<{
  question: SessionQuestion;
  onAnswer(answer: SessionQuestionAnswer): Promise<void>;
}>) {
  const { t } = useTranslation();
  const [collapsed, setCollapsed] = useState(false);
  const [selected, setSelected] = useState<number | null>(null);
  const [text, setText] = useState("");
  const [sending, setSending] = useState(false);
  const [error, setError] = useState("");
  const submitting = useRef(false);
  const answerFor = (index: number | null) => [index === null ? "" : `${index + 1}. ${question.options[index]}`, text.trim()].filter(Boolean).join("\n\n");
  const answer = answerFor(selected);
  async function submit(value: SessionQuestionAnswer) {
    if (submitting.current) return;
    submitting.current = true;
    setSending(true);
    setError("");
    try { await onAnswer(value); }
    catch { setError(t("sessionQuestion.failed")); }
    finally { submitting.current = false; setSending(false); }
  }
  if (collapsed) return <div className="sessionQuestionCollapsed">
    <span><MessageCircleQuestion aria-hidden="true" />{t("sessionQuestion.pending")}</span>
    <button type="button" onClick={() => setCollapsed(false)}>{t("sessionQuestion.expand")}</button>
  </div>;
  return <section className="sessionQuestion" aria-label={question.question} aria-busy={sending}>
    <div className="sessionQuestionTop">
      <span><MessageCircleQuestion aria-hidden="true" />{t("sessionQuestion.label")}</span>
      <button type="button" className="sessionQuestionClose" aria-label={t("sessionQuestion.collapse")} onClick={() => setCollapsed(true)}><X aria-hidden="true" /></button>
    </div>
    <p className="sessionQuestionHeading">{question.question}</p>
    <div className="sessionQuestionOptions" aria-label={t("sessionQuestion.choose")}>
      {question.options.map((option, index) => <button type="button" className="sessionQuestionOption" key={index}
        disabled={sending} aria-pressed={selected === index} onClick={() => {
          if (submitting.current) return;
          setSelected(index);
          void submit({ action: "answer", answer: answerFor(index) });
        }}>
        <span className="sessionQuestionNumber">{index + 1}</span>{" "}<span className="sessionQuestionOptionCopy">{option}</span><ArrowRight aria-hidden="true" />
      </button>)}
    </div>
    <div className="sessionQuestionFooter">
      <label className="sessionQuestionFree"><Pencil aria-hidden="true" /><textarea rows={1} value={text} disabled={sending}
        aria-label={t("sessionQuestion.answer")} placeholder={t("sessionQuestion.placeholder")} onChange={event => setText(event.target.value)} /></label>
      <div className="sessionQuestionActions">
        <button type="button" disabled={sending} onClick={() => void submit({ action: "skip" })}>{t("sessionQuestion.skip")}</button>
        <button type="button" className="sessionQuestionSend" disabled={sending || !answer} onClick={() => void submit({ action: "answer", answer })}>{t(sending ? "sessionQuestion.sending" : "sessionQuestion.send")}</button>
      </div>
    </div>
    {error ? <p className="sessionQuestionError" role="alert">{error}</p> : null}
  </section>;
}
