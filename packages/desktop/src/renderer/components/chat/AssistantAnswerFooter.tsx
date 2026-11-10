import { memo, useEffect, useRef, useState } from "react";
import { Check, Copy } from "lucide-react";
import { Button } from "../ui/button";
import { formatUserMessageTimestamp } from "./chatRuntimeCore";

export const AssistantAnswerFooter = memo(function AssistantAnswerFooter({
	text,
	completedAtMs,
}: {
	text: string;
	completedAtMs?: number;
}) {
	const [copied, setCopied] = useState(false);
	const [failed, setFailed] = useState(false);
	const owner = useRef<{ text: string } | null>(null);
	useEffect(() => {
		owner.current = { text };
		setCopied(false);
		setFailed(false);
		return () => {
			owner.current = null;
		};
	}, [text]);
	useEffect(() => {
		if (!copied) return;
		const timer = setTimeout(() => setCopied(false), 2000);
		return () => clearTimeout(timer);
	}, [copied]);
	const copy = async () => {
		const request = owner.current;
		try {
			await navigator.clipboard.writeText(text);
			if (request && owner.current === request) {
				setCopied(true);
				setFailed(false);
			}
		} catch {
			if (request && owner.current === request) setFailed(true);
		}
	};
	const hasTime =
		completedAtMs !== undefined && Number.isFinite(completedAtMs) && completedAtMs > 0;
	return (
		<div className="assistantAnswerFooter">
			<Button
				variant="ghost"
				size="icon-sm"
				aria-label={copied ? "Copied answer" : "Copy answer"}
				onClick={() => void copy()}
			>
				{copied ? <Check size={14} aria-hidden="true" /> : <Copy size={14} aria-hidden="true" />}
			</Button>
			{hasTime ? (
				<time dateTime={new Date(completedAtMs).toISOString()}>
					{formatUserMessageTimestamp(completedAtMs)}
				</time>
			) : null}
			{failed ? <span role="status">Couldn’t copy. Try again.</span> : null}
		</div>
	);
});
