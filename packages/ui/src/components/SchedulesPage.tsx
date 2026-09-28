import { Clock, RefreshCw } from "lucide-react";
import { useEffect, useState } from "react";
import { invokeHost } from "../host/hostBridge";
type Plan = {
	id: string;
	enabled: boolean;
	nextAt: number | null;
	spec: {
		name: string;
		prompt: string;
		cwd: string;
		timezone: string;
		cron: string | null;
		at: number | null;
	};
};
type Run = {
	id: string;
	scheduledAt: number;
	status: string;
	sessionId: string | null;
	error: string | null;
};
export function SchedulesPage({
	onSession,
}: {
	onSession: (id: string) => void;
}) {
	const [plans, setPlans] = useState<Plan[]>([]);
	const [enabled, setEnabled] = useState(false);
	const [selected, setSelected] = useState<string | null>(null);
	const [runs, setRuns] = useState<Run[]>([]);
	const [error, setError] = useState("");
	const [revision, setRevision] = useState(0);
	const [loading, setLoading] = useState(true);
	const [planCursor, setPlanCursor] = useState<string | null>(null);
	const [nextPlans, setNextPlans] = useState<string | null>(null);
	const [runCursor, setRunCursor] = useState<string | null>(null);
	const [nextRuns, setNextRuns] = useState<string | null>(null);
	const [runsLoading, setRunsLoading] = useState(false);
	useEffect(() => {
		let disposed = false;
		setLoading(true);
		setError("");
		void invokeHost<{
			schedules: Plan[];
			serviceEnabled: boolean;
			nextCursor: string | null;
		}>("schedule_manage", {
			request: {
				action: "list",
				...(planCursor ? { cursor: planCursor } : {}),
			},
		})
			.then((result) => {
				if (!disposed) {
					setPlans((old) =>
						planCursor
							? [
									...old,
									...result.schedules.filter(
										(p) => !old.some((o) => o.id === p.id),
									),
								]
							: result.schedules,
					);
					setEnabled(result.serviceEnabled);
					setNextPlans(result.nextCursor ?? null);
				}
			})
			.catch((error) => {
				if (!disposed) setError(String(error));
			})
			.finally(() => {
				if (!disposed) setLoading(false);
			});
		return () => {
			disposed = true;
		};
	}, [revision, planCursor]);
	useEffect(() => {
		let disposed = false;
		if (!runCursor) setRuns([]);
		setNextRuns(null);
		setRunsLoading(Boolean(selected));
		if (selected)
			void invokeHost<{ runs: Run[]; nextCursor: string | null }>(
				"schedule_manage",
				{
					request: {
						action: "history",
						scheduleId: selected,
						...(runCursor ? { cursor: runCursor } : {}),
					},
				},
			)
				.then((result) => {
					if (!disposed) {
						setRuns((old) =>
							runCursor
								? [
										...old,
										...result.runs.filter(
											(r) => !old.some((o) => o.id === r.id),
										),
									]
								: result.runs,
						);
						setNextRuns(result.nextCursor ?? null);
					}
				})
				.catch((error) => {
					if (!disposed) setError(String(error));
				})
				.finally(() => {
					if (!disposed) setRunsLoading(false);
				});
		return () => {
			disposed = true;
		};
	}, [selected, revision, runCursor]);
	const plan = plans.find((plan) => plan.id === selected);
	return (
		<section className="managementPage schedulesPage" aria-label="Scheduled">
			<header>
				<div>
					<h1>Your schedules</h1>
					<p className="pageDescription">
						Ask the agent to create or change a schedule in a chat.
					</p>
				</div>
				<button
					type="button"
					aria-label="Refresh schedules"
					onClick={() => {
						setPlanCursor(null);
						setRunCursor(null);
						setRevision((v) => v + 1);
					}}
					disabled={loading}
				>
					<RefreshCw size={16} aria-hidden="true" />
				</button>
			</header>

			{!loading && !error ? (
				<p className="pageDescription">
					{enabled ? "Scheduling enabled" : "Scheduling paused"} · Runs require
					this computer to be awake.
				</p>
			) : null}
			{error ? <p role="alert">{error}</p> : null}
			{loading ? (
				<p role="status">Loading…</p>
			) : !error && !plans.length ? (
				<div className="scheduleEmpty">
					<Clock size={28} aria-hidden="true" />
					<h2>No schedules yet</h2>
					<p>Start with a chat, such as “Review my project every morning.”</p>
				</div>
			) : null}
			<div className="scheduleList">
				{plans.map((plan) => (
					<button
						key={plan.id}
						aria-pressed={selected === plan.id}
						onClick={() => {
							setRunCursor(null);
							setSelected(plan.id);
						}}
					>
						<strong>{plan.spec.name}</strong>
						<span>{plan.enabled ? "Enabled" : "Paused"}</span>
						<small>
							{plan.nextAt
								? new Date(plan.nextAt).toLocaleString()
								: "No upcoming run"}
						</small>
					</button>
				))}
			</div>
			{nextPlans ? (
				<button disabled={loading} onClick={() => setPlanCursor(nextPlans)}>
					Load more schedules
				</button>
			) : null}
			{plan ? (
				<section className="scheduleDetail">
					<h2>{plan.spec.name}</h2>
					<p>{plan.spec.prompt}</p>
					<p className="pageDescription">
						{plan.spec.cwd} · {plan.spec.timezone} ·{" "}
						{plan.spec.cron ?? "One-time"}
					</p>
					<h3>Run history</h3>
					{!runs.length ? (
						<p>No runs yet</p>
					) : (
						runs
							.slice()
							.sort(
								(a, b) =>
									b.scheduledAt - a.scheduledAt || b.id.localeCompare(a.id),
							)
							.map((run) => (
								<div className="scheduleRun" key={run.id}>
									<span>
										{new Date(run.scheduledAt).toLocaleString()} · {run.status}
									</span>
									{run.sessionId ? (
										<button onClick={() => onSession(run.sessionId!)}>
											Open chat
										</button>
									) : null}
									{run.error ? <p role="alert">{run.error}</p> : null}
								</div>
							))
					)}
					{nextRuns ? (
						<button
							disabled={runsLoading}
							onClick={() => setRunCursor(nextRuns)}
						>
							Load more runs
						</button>
					) : null}
				</section>
			) : null}
		</section>
	);
}
