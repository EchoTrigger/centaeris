import { ListCollapse, RefreshCw } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import {
  listProcesses,
  readProcess,
  stopProcess,
  ProcessOutputBuffer,
  type ProcessList,
  type ProcessSnapshot,
} from "../lib/processBridge";

function status(process: ProcessSnapshot): string {
  if (process.state !== "exited") return process.state;
  return `${process.terminationReason ?? "exited"} · ${process.exitCode ?? "no exit code"}${!process.cleanupComplete ? " · cleanup unconfirmed" : ""}`;
}
function ProcessOutput({ sessionId, id, active }: { sessionId: string; id: string; active: boolean }) {
  const [content, setContent] = useState("");
  const [error, setError] = useState("");
  const [truncated, setTruncated] = useState(false);
  const [revision, setRevision] = useState(0);
  const output = useRef({ cursor: "0", buffer: new ProcessOutputBuffer() });
  useEffect(() => {
    if (!active) return;
    let disposed = false,
      timer: ReturnType<typeof setTimeout>;
    const buffer = output.current.buffer;
    setError("");
    const read = async () => {
      try {
        const page = await readProcess(sessionId, id, output.current.cursor);
        if (disposed) return;
        setContent(buffer.append(page));
        setTruncated(buffer.truncated);
        setError(page.process.error ?? "");
        output.current.cursor = page.nextCursor;
        if (!page.process.outputComplete || page.hasMore)
          timer = setTimeout(read, page.hasMore ? 0 : 500);
      } catch (error) {
        if (!disposed) setError(String(error));
      }
    };
    void read();
    return () => {
      disposed = true;
      clearTimeout(timer);
    };
  }, [sessionId, id, revision, active]);
  return (
    <section className="workspaceTaskOutput" aria-label="Process output">
      {error ? (
        <div role="alert">
          {error}{" "}
          <button type="button" onClick={() => setRevision((v) => v + 1)}>
            Retry output
          </button>
        </div>
      ) : null}
      {truncated ? (
        <div role="status">Showing the latest 256 Ki characters</div>
      ) : null}
      <pre tabIndex={0}>{content || "No output yet"}</pre>
    </section>
  );
}
export function WorkspaceTasks({ sessionId, active = true }: { sessionId: string; active?: boolean }) {
  const [list, setList] = useState<ProcessList>({
    serviceInstanceId: "",
    processes: [],
  });
  const [selected, setSelected] = useState<string | null>(null);
  const [error, setError] = useState("");
  const [loading, setLoading] = useState(true);
  const [stopError, setStopError] = useState("");
  const [stopping, setStopping] = useState<string | null>(null);
  const [revision, setRevision] = useState(0);
  useEffect(() => {
    if (!active) return;
    let disposed = false,
      timer: ReturnType<typeof setTimeout>;
    const refresh = async () => {
      try {
        const next = await listProcesses(sessionId);
        if (disposed) return;
        setList(next);
        setLoading(false);
        setError("");
        setSelected((id) =>
          next.processes.some((p) => p.processSessionId === id)
            ? id
            : ((
                next.processes.find((p) => p.state !== "exited") ??
                next.processes[0]
              )?.processSessionId ?? null),
        );
        timer = setTimeout(refresh, 1500);
      } catch (error) {
        if (!disposed) { setError(String(error)); setLoading(false); }
      }
    };
    void refresh();
    return () => {
      disposed = true;
      clearTimeout(timer);
    };
  }, [sessionId, revision, active]);
  async function stop(id: string) {
    setStopping(id);
    setStopError("");
    try {
      const result = await stopProcess(sessionId, id);
      setList((current) => ({
        ...current,
        processes: current.processes.map((p) =>
          p.processSessionId === id ? result : p,
        ),
      }));
      setRevision((v) => v + 1);
    } catch (error) {
      setStopError(String(error));
    } finally {
      setStopping(null);
    }
  }
  return (
    <div className="workspaceTasks">
      <div className="workspaceTasksToolbar">
        <span className="workspaceTasksTitle"><ListCollapse aria-hidden="true"/>Task</span>
        <button
          type="button"
          aria-label="Refresh tasks"
          onClick={() => setRevision((v) => v + 1)}
        >
          <RefreshCw size={14} aria-hidden="true"/>
        </button>
      </div>
      {error || stopError ? <div role="alert">{error || stopError}</div> : null}
      <div className={`workspaceTaskList ${list.processes.length === 0 ? "is-empty" : ""}`} aria-label="Task">
        {list.processes.length === 0 && !error ? (
          <div className="workspaceTaskEmpty" role="status"><ListCollapse aria-hidden="true"/><span>{loading ? "Loading…" : "No tasks"}</span></div>
        ) : null}
        {list.processes.map((process) => (
          <div
            key={process.processSessionId}
            className={`workspaceTaskRow ${selected === process.processSessionId ? "is-selected" : ""}`}
          >
            <button
              type="button"
              aria-pressed={selected === process.processSessionId}
              onClick={() => setSelected(process.processSessionId)}
              title={process.processSessionId}
            >
              <span>{[process.program, ...process.args].join(" ")}</span>
              <small>{status(process)}</small>
            </button>
            <button
              type="button"
              aria-label={`Stop ${process.processSessionId}`}
              disabled={process.state !== "running" || stopping !== null}
              onClick={() => void stop(process.processSessionId)}
            >
              Stop
            </button>
          </div>
        ))}
      </div>
      {selected ? (
        <ProcessOutput
          active={active}
          key={`${sessionId}:${list.serviceInstanceId}:${selected}`}
          sessionId={sessionId}
          id={selected}
        />
      ) : null}
    </div>
  );
}
