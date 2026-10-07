import os
import unittest
import threading
from unittest.mock import Mock
from unittest.mock import patch

os.environ.update(RUNTIME_INTERNAL_URL="http://runtime.invalid", API_INTERNAL_URL="http://api.invalid",
                  INTERNAL_API_TOKEN="test-internal-token")
import worker


class WorkRecoveryTests(unittest.TestCase):
    def page(self,count=3,next_page=False):
        key=lambda i:{"insertedAt":"2026-10-02T00:00:00+00:00","eventId":f"{i:03}"}
        return {"schema":"workspace.agent_work.discovered.v1","through":key(99),
            "next":key(count-1) if next_page else None,
            "entries":[{"cursor":key(i),"sourceEventId":f"{i:03}"} for i in range(count)]}

    def test_batch_attempts_multiple_sources_and_advances_past_first_failure(self):
        def key(index):
            return {"insertedAt":"2026-10-02T00:00:00+00:00", "eventId":str(index)}
        calls = []
        def api(path, body, reason, **kwargs):
            calls.append((path, body))
            if path.endswith("discover"):
                return {"schema":"workspace.agent_work.discovered.v1", "through":key(3),
                    "next":None, "entries":[{"cursor":key(i), "sourceEventId":str(i)} for i in range(3)]}
            if body["sourceEventId"] == "0":
                raise worker.DependencyUnavailable("busy")
            return {"schema":"agent.work.materialized.v1"}
        with patch.object(worker, "api_request", side_effect=api):
            scan = worker.WorkRequestReconciler()
            scan()
        self.assertEqual([body["sourceEventId"] for path,body in calls if path.endswith("materialize")], ["0","1","2"])
        self.assertIsNone(scan.after)

    def test_budget_keeps_unprocessed_tail_and_same_upper_for_next_round(self):
        page=self.page()
        calls=[]
        clock=[0]
        def api(path,body,reason,**kwargs):
            calls.append((path,body,kwargs))
            if path.endswith("discover"):
                return page if body["after"] is None else {**page,"entries":page["entries"][1:]}
            if body["sourceEventId"]=="000":
                clock[0]=worker.RECONCILE_INTERVAL_SECONDS
            return {}
        with patch.object(worker,"api_request",side_effect=api),patch.object(worker.time,"monotonic",side_effect=lambda:clock[0]):
            scan=worker.WorkRequestReconciler()
            scan()
            self.assertEqual(scan.after,page["entries"][0]["cursor"])
            self.assertEqual(scan.through,page["through"])
            scan()
        self.assertEqual([body["sourceEventId"] for path,body,_ in calls if path.endswith("materialize")],["000","001","002"])
        self.assertEqual(calls[2][1]["after"],page["entries"][0]["cursor"])
        self.assertIsNone(scan.after)

    def test_discovery_failure_preserves_cursor_and_restart_begins_at_head(self):
        scan=worker.WorkRequestReconciler()
        scan.after=self.page()["entries"][0]["cursor"]
        scan.through=self.page()["through"]
        with patch.object(worker,"api_request",side_effect=worker.DependencyUnavailable("offline")):
            with self.assertRaises(worker.DependencyUnavailable): scan()
        self.assertEqual(scan.after,self.page()["entries"][0]["cursor"])
        with patch.object(worker,"api_request",return_value={**self.page(),"entries":[],"next":None}) as api:
            worker.WorkRequestReconciler()()
        self.assertIsNone(api.call_args.args[1]["after"])

    def test_admitted_rows_advance_without_http_admission_and_round_work_is_bounded(self):
        page=self.page(100,next_page=True)
        for entry in page["entries"]: entry["sourceEventId"]=None
        with patch.object(worker,"api_request",return_value=page) as api:
            scan=worker.WorkRequestReconciler()
            scan()
        self.assertEqual(api.call_count,1)
        self.assertEqual(scan.after,page["next"])

    def test_forged_nonadvancing_page_does_not_mutate_cursor(self):
        page=self.page()
        page["entries"][1]["cursor"]=page["entries"][0]["cursor"]
        with patch.object(worker,"api_request",return_value=page):
            scan=worker.WorkRequestReconciler()
            with self.assertRaises(RuntimeError): scan()
        self.assertIsNone(scan.after)
        self.assertIsNone(scan.through)

    def test_slow_scanner_thread_does_not_hold_lifecycle_or_waiter(self):
        entered=threading.Event()
        release=threading.Event()
        stopped=threading.Event()
        def api(path,body,reason,**kwargs):
            if path.endswith("discover"):
                entered.set()
                self.assertTrue(release.wait(5))
                return {**self.page(),"entries":[],"next":None}
            return {"activeNext":None,"deadLetterNext":None}
        def runtime(path,body):
            return {"disposition":"reconciled","checked":0,"waiters":[],"next":None} if path.endswith("reconcile-waiters") else {}
        with patch.object(worker,"api_request",side_effect=api),patch.object(worker,"runtime_request",side_effect=runtime):
            thread=threading.Thread(target=worker.run_loop,args=(worker.WorkRequestReconciler(stopped),5,stopped))
            thread.start()
            try:
                self.assertTrue(entered.wait(5))
                self.assertIn("waiters",worker.LifecycleReconciler()())
                stopped.set()
            finally:
                release.set()
                thread.join(5)
            self.assertFalse(thread.is_alive())

    def test_service_uses_single_bounded_scan_control_thread_and_joins_it(self):
        # Exercise actual serve wiring without creating job threads or HTTP.
        threads=[]
        class Thread:
            def __init__(self,**kwargs): self.kwargs=kwargs; self.joined=False; threads.append(self)
            def start(self):
                if self.kwargs["name"]=="workspace-work-reconciler": self.kwargs["args"][2].set()
            def join(self): self.joined=True
        with patch.object(worker.threading,"Thread",Thread),patch.object(worker.signal,"signal",return_value=None):
            worker.run_worker_service()
        scanners=[t for t in threads if t.kwargs["name"]=="workspace-work-reconciler"]
        self.assertEqual(len(scanners),1)
        self.assertIsInstance(scanners[0].kwargs["args"][0],worker.WorkRequestReconciler)
        self.assertTrue(scanners[0].joined)

    def test_failed_control_round_waits_before_retry_and_stop_prevents_admission(self):
        stop=Mock()
        stop.is_set.side_effect=[False,True]
        operation=Mock(side_effect=RuntimeError("failure"))
        worker.run_loop(operation,worker.RECONCILE_INTERVAL_SECONDS,stop)
        stop.wait.assert_called_once_with(worker.RECONCILE_INTERVAL_SECONDS)
        stopped=threading.Event(); stopped.set()
        with patch.object(worker,"api_request",return_value=self.page()) as api:
            worker.WorkRequestReconciler(stopped)()
        self.assertEqual(api.call_count,0)

    def test_multiple_pages_hold_upper_and_do_not_retry_failed_source_until_new_pass(self):
        pages=[self.page(2,next_page=True),self.page(4)]
        pages[1]["entries"]=pages[1]["entries"][2:]
        calls=[]
        def api(path,body,reason,**kwargs):
            calls.append((path,body))
            if path.endswith("discover"): return pages.pop(0)
            if body["sourceEventId"]=="000": raise worker.DependencyUnavailable("busy")
            return {}
        with patch.object(worker,"api_request",side_effect=api):
            scan=worker.WorkRequestReconciler()
            scan(); scan()
        discoveries=[body for path,body in calls if path.endswith("discover")]
        self.assertEqual(discoveries[1]["through"],self.page()["through"])
        self.assertEqual([body["sourceEventId"] for path,body in calls if path.endswith("materialize")],["000","001","002","003"])
        self.assertIsNone(scan.after)
