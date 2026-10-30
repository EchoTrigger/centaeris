"""Behavioral contracts for model-independent experience timing reports."""
import copy
import json
import tempfile
from pathlib import Path
import unittest

from perf.harness.experience_timing import build_report, read_records


def span(phase, elapsed, end, run="run-a", **extra):
    return {"schema":"workspace.perf.v1", "source":"runtime", "phase":phase,
            "jobId":"agent_run.lifecycle:"+run, "elapsedMs":elapsed,
            "atUnixMs":end, "outcome":"ok", **extra}


def clients():
    return [
        {"phase":"accepted", "agentRunId":"run-a", "sessionId":"session-a", "submittedAtMs":1000},
        {"phase":"toolTiming", "agentRunId":"run-a", "sessionId":"session-a", "callId":"call-a",
         "toolIndex":1, "toolName":"bash", "toolCallCreatedAtMs":5000, "toolResultCreatedAtMs":5100,
         "toolCallObservedAtMs":1100, "toolResultObservedAtMs":1220, "clientCallToResultMs":120,
         "resultState":"successWithOutput", "serverTimingState":"observed", "clientTimingState":"observed", "contractState":"verified"},
        {"phase":"verified", "agentRunId":"run-a", "sessionId":"session-a", "admissionMs":20,
         "firstSuccessfulToolResultMs":220, "firstModelTextMs":350, "terminalMs":400,
         "firstModelTextSource":"live", "secondToolClientCallToResultMs":None},
    ]


class ExperienceTimingTests(unittest.TestCase):
    def report(self, observations=(), records=None, **options):
        return build_report(clients() if records is None else records, observations,
                            execution_mode="perRun", model_kind="immediateMock", **options)

    def test_tool_before_text_does_not_turn_final_text_into_model_ttft(self):
        row=self.report([span("toolService",100,5100,callId="call-a",requestId="request-a")])["runs"][0]
        self.assertEqual(row["client"]["newRunToFirstToolSuccessMs"],220)
        self.assertEqual(row["client"]["runStartToFirstObservedAssistantTextMs"],350)
        self.assertEqual(row["client"]["newRunToTerminalMs"],400)
        self.assertEqual(row["tools"][0]["serviceMs"],100)
        self.assertEqual(row["tools"][0]["clientCallToResultMs"],120)
        self.assertEqual(row["modelRequests"],[])
        self.assertIn("modelRequestTiming",row["notObserved"])

    def test_nested_spans_and_parallel_startup_metrics_are_not_added(self):
        startup=span("startupProfile",250,1250)
        startup.pop("elapsedMs")
        startup.update(runtimePreludeMs=10,sandboxEnsureMs=200,mcpCredentialResolveMs=20,
                       mcpDiscoveryMs=180,workspaceRestoreMs=30,sessionStartCommitMs=10,
                       startupTotalMs=250,serverCount=1,credentialCount=1,mcpConnectionCount=1)
        row=self.report([span("globalHold",200,1250),span("dockerCreate",150,1220),startup])["runs"][0]
        self.assertEqual(row["runtimeCoveredMs"],200)
        self.assertEqual(len(row["intervals"]),2)
        self.assertEqual(row["startupProfiles"][0]["startupTotalMs"],250)
        self.assertEqual(row["startupProfiles"][0]["mcpDiscoveryMs"],180)
        self.assertEqual(row["criticalPath"],"notObserved")

    def test_model_first_event_and_first_text_are_request_local_and_attempt_scoped(self):
        observations=[span("modelHttp",90,2090,requestId="request-a",attempt=1,streaming=True),
                      span("modelFirstEvent",20,2020,requestId="request-a",attempt=1,streaming=True),
                      span("modelHttp",120,3120,requestId="request-b",attempt=1,streaming=True),
                      span("modelFirstText",60,3060,requestId="request-b",attempt=1,streaming=True)]
        rows=self.report(observations)["runs"][0]["modelRequests"]
        self.assertEqual(rows[0]["firstEventMs"],20)
        self.assertIsNone(rows[0]["firstTextMs"])
        self.assertEqual(rows[1]["firstTextMs"],60)
        self.assertEqual(rows[1]["httpMs"],120)

    def test_host_clocks_are_joined_by_task_identity_without_cross_clock_subtraction(self):
        host=lambda phase,elapsed,end,task="task-a":{
            "schema":"workspace.perf.v1","source":"host","phase":phase,
            "taskId":task,"elapsedMs":elapsed,"atUnixMs":end,"outcome":"ok"}
        observations=[span("hostCommand",150,5150,runId="run-a",operationId="operation-a",taskId="task-a"),
                      host("taskReady",30,900030),host("taskLeaderExited",40,900040),
                      host("taskReady",0,100,"foreign-task")]
        row=self.report(observations)["runs"][0]["hostCommands"][0]
        self.assertEqual(row["elapsedMs"],150)
        self.assertEqual(row["readyReceivedMs"],30)
        self.assertEqual(row["leaderExitedReceivedMs"],40)
        self.assertEqual(row["readyToLeaderExitReceivedMs"],10)
        self.assertNotIn("foreign-task",json.dumps(row))

    def test_missing_diagnostics_and_failure_never_become_zero_latency_or_acceptance(self):
        report=self.report([span("toolService",70,5070,callId="call-a",requestId="request-a",outcome="error")])
        self.assertEqual(report["scope"],"latencyOnly")
        self.assertEqual(report["toolPerformanceVerdict"],"notEvaluated")
        self.assertEqual(report["physicalAcceptance"],"notEvaluated")
        row=report["runs"][0]
        self.assertIsNone(row["tools"][0]["serviceMs"])
        self.assertEqual(row["tools"][0]["serviceOutcome"],"error")
        self.assertIn("startupProfile",row["notObserved"])
        self.assertIn("hostCommandTiming",row["notObserved"])

    def test_repeated_run_attempts_are_retained_and_ambiguous_tool_pair_is_unknown(self):
        observations=[span("toolService",100,5100,callId="call-a",requestId="request-a"),
                      span("toolService",90,6100,callId="call-a",requestId="request-a")]
        row=self.report(observations)["runs"][0]
        self.assertEqual(len(row["intervals"]),2)
        self.assertIsNone(row["tools"][0]["serviceMs"])
        self.assertEqual(row["tools"][0]["serviceOutcome"],"ambiguous")

    def test_same_task_cannot_be_attributed_to_two_original_runs(self):
        records=clients()+[{"phase":"accepted","agentRunId":"run-b","sessionId":"session-b","submittedAtMs":1000}]
        observations=[span("hostCommand",10,5100,runId="run-a",operationId="op-a",taskId="task-a"),
                      span("hostCommand",10,5200,run="run-b",runId="run-b",operationId="op-b",taskId="task-a")]
        with self.assertRaisesRegex(ValueError,"task binding"):
            self.report(observations,records=records)

    def test_only_allowlisted_fields_are_projected_and_foreign_jobs_are_ignored(self):
        observations=[span("dockerCreate",100,5100,body={"token":"private-token"},error="private-error"),
                      span("dockerCreate",50,5200,run="foreign")]
        output=json.dumps(self.report(observations))
        for value in ["private-token","private-error",'"body"',"foreign"]:
            self.assertNotIn(value,output)

    def test_invalid_times_and_binding_are_rejected_without_logging_values(self):
        for value in [True,-1,float("inf"),float("nan"),"100"]:
            with self.subTest(value=repr(value)),self.assertRaisesRegex(ValueError,"timing observation"):
                self.report([span("dockerCreate",value,5100)])
        with self.assertRaisesRegex(ValueError,"Run binding"):
            self.report([span("hostCommand",10,5100,runId="foreign",operationId="op-a",taskId="task-a")])
        records=clients();records[1]["sessionId"]="other-session"
        with self.assertRaisesRegex(ValueError,"client Run binding"):
            self.report(records=records)

    def test_second_tool_timing_preserves_its_own_clock_and_full_result(self):
        records=clients()
        second=copy.deepcopy(records[1]);second.update(callId="call-b",toolIndex=2,
            toolCallObservedAtMs=1300,toolResultObservedAtMs=1500,clientCallToResultMs=200,
            toolCallCreatedAtMs=None,toolResultCreatedAtMs=None,serverTimingState="notObserved")
        records.insert(2,second);records[-1]["secondToolClientCallToResultMs"]=200
        row=self.report([span("toolService",180,9000,callId="call-b",requestId="request-b")],records=records)["runs"][0]
        self.assertEqual(row["tools"][1]["serviceMs"],180)
        self.assertEqual(row["client"]["secondToolClientCallToResultMs"],200)
        self.assertIsNone(row["tools"][1]["toolCallCreatedAtMs"])

    def test_reads_bare_and_docker_json_logs_without_retaining_arbitrary_logs(self):
        with tempfile.TemporaryDirectory() as name:
            path=Path(name)/"runtime.jsonl"
            event=span("dockerCreate",100,5100)
            path.write_text('ordinary private log\n'+json.dumps(event)+'\n'+
                json.dumps({"log":json.dumps(event)+"\n","stream":"stderr","time":"date"})+'\n',encoding="utf-8")
            self.assertEqual(read_records(path),[event,event])
            path.write_text('{"schema":"workspace.perf.v1","phase":"dockerCreate",',encoding="utf-8")
            with self.assertRaisesRegex(ValueError,"malformed timing observation"):
                read_records(path)

    def test_irrelevant_json_events_do_not_consume_the_selected_record_bound(self):
        with tempfile.TemporaryDirectory() as name:
            path=Path(name)/"runtime.jsonl"
            path.write_text((json.dumps({"schema":"workspace.perf.v1","phase":"requestAdmission"})+'\n')*20001+
                            json.dumps(span("dockerCreate",100,5100))+'\n',encoding="utf-8")
            self.assertEqual(read_records(path),[span("dockerCreate",100,5100)])

    def test_unpaired_or_failed_commit_cannot_report_successful_tool_duration(self):
        unpaired=span("toolService",100,5100,callId="call-a",requestId="request-a",outcome="unpaired")
        unpaired.pop("elapsedMs")
        for event,expected in [(unpaired,"unpaired"),
            (span("toolService",100,5100,callId="call-a",requestId="request-a",outcome="commitError"),"commitError")]:
            with self.subTest(outcome=expected):
                row=self.report([event])["runs"][0]
                self.assertEqual(row["tools"][0]["serviceOutcome"],expected)
                self.assertIsNone(row["tools"][0]["serviceMs"])
        row=self.report([unpaired])["runs"][0]
        self.assertIsNone(row["runtimeCoveredMs"])
        self.assertEqual(len(row["unpairedTools"]),1)

    def test_clock_rollback_and_partial_server_markers_remain_unknown(self):
        records=clients();records[1].update(toolCallCreatedAtMs=5000,toolResultCreatedAtMs=None,
            serverTimingState="notObserved",toolCallObservedAtMs=1300,toolResultObservedAtMs=1200,
            clientCallToResultMs=None,clientTimingState="notObserved")
        row=self.report(records=records)["runs"][0]["tools"][0]
        self.assertIsNone(row["clientCallToResultMs"])
        self.assertEqual(row["clientTimingState"],"notObserved")
        self.assertEqual(row["toolCallCreatedAtMs"],5000)
        self.assertIsNone(row["toolResultCreatedAtMs"])

    def test_partial_unpaired_duration_does_not_become_completed_tool_time(self):
        row=self.report([span("toolService",75,5075,callId="call-a",requestId="request-a",outcome="unpaired")])["runs"][0]
        self.assertEqual(row["tools"][0]["serviceOutcome"],"unpaired")
        self.assertIsNone(row["tools"][0]["serviceMs"])
        self.assertEqual(row["unpairedTools"][0]["elapsedMs"],75)

    def test_repeated_model_attempt_identity_remains_ambiguous_without_hiding_samples(self):
        report=self.report([span("modelHttp",100,5100,requestId="request-a",attempt=1,streaming=True),
                            span("modelHttp",200,6200,requestId="request-a",attempt=1,streaming=True)])
        row=report["runs"][0]
        self.assertEqual(len(row["intervals"]),2)
        self.assertIsNone(row["modelRequests"][0]["httpMs"])
        self.assertEqual(row["modelRequests"][0]["outcome"],"ambiguous")
        self.assertEqual(row["modelRequests"][0]["ambiguousFields"],["httpMs"])

    def test_assistant_text_source_is_preserved_and_second_tool_summary_must_match(self):
        records=clients();records[-1]["firstModelTextTimingKind"]="runStartToFirstObservedAssistantText"
        row=self.report(records=records)["runs"][0]
        self.assertEqual(row["client"]["firstModelTextSource"],"live")
        self.assertEqual(row["client"]["firstModelTextTimingKind"],"runStartToFirstObservedAssistantText")
        records[-1]["secondToolClientCallToResultMs"]=999
        with self.assertRaisesRegex(ValueError,"second tool timing"):
            self.report(records=records)
        second=copy.deepcopy(records[1]);second.update(callId="call-b",toolIndex=2)
        records.insert(2,second)
        with self.assertRaisesRegex(ValueError,"second tool timing"):
            self.report(records=records)

    def test_diagnostic_plan_and_bounds_do_not_claim_a_latency_slo(self):
        for options in [{"model_kind":"unknown"},{"execution_mode":"prerun"}]:
            with self.assertRaises(ValueError):build_report(clients(),[],**options)
        with self.assertRaisesRegex(ValueError,"record bound"):
            self.report([span("dockerCreate",1,5000)]*20001)


if __name__=="__main__":unittest.main()
