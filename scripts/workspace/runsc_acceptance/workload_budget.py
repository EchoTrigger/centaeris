"""One finite budget plan for the unchanged rolling workload and serial readbacks."""

LIMIT_SECONDS = 4 * 60 * 60
HTTP_REQUEST_SECONDS = 60  # k6/http default, including the admission request.
ARTIFACT_DOWNLOAD_SECONDS = 30  # Explicit common.js download timeout.
COMMAND_SECONDS = 60  # FixtureDriver.command, including SQL and status wrappers.
DRIVER_HTTP_SECONDS = 30
POLL_SLEEP_MS = 200  # FixtureDriver.wait can sleep before its final observation.
FINAL_JOB_SETTLE_SECONDS = 5  # Core cleanup can precede the Worker's complete_job RPC.


def workload_budget(environments, iterations, terminal_seconds, between_run_ms=0, initial_stagger_ms=0, *, execution_mode='retained'):
    if execution_mode not in ('perRun','retained'):raise ValueError('supported workload execution mode required')
    values = [(environments, 1, 64), (iterations, 1, 20), (terminal_seconds, 1, 600),
              (between_run_ms, 0, 60000), (initial_stagger_ms, 0, 120000)]
    if any(type(value) is not int or not lower <= value <= upper for value, lower, upper in values):
        raise ValueError('bounded rolling workload budget parameters required')
    stagger_total_ms = initial_stagger_ms * (environments - 1)
    if stagger_total_ms > 120000:
        raise ValueError('bounded initial workload stagger required')
    ceil_ms = lambda milliseconds: (milliseconds + 999) // 1000
    scenario = (iterations * (terminal_seconds + HTTP_REQUEST_SECONDS + ARTIFACT_DOWNLOAD_SECONDS)
                + ceil_ms(stagger_total_ms + (iterations - 1) * between_run_ms))
    k6 = {'scenarioSeconds': scenario, 'setupSeconds': 3 * HTTP_REQUEST_SECONDS, 'gracefulStopSeconds': 30}
    # Existing terminate/kill waits (10 + 10) and reader join (5). The same
    # finite allowance covers k6 initialization, summary export and normal exit.
    process_cleanup = 25
    process = sum(k6.values()) + process_cleanup
    # wait() checks its deadline after observe(), so reserve the final poll's
    # complete command/RPC chain and one 200-ms sleep beyond each polling window.
    completed = environments * iterations * ceil_ms((120 + COMMAND_SECONDS) * 1000 + POLL_SLEEP_MS)
    pause = environments * ceil_ms((120 + 2 * COMMAND_SECONDS) * 1000 + POLL_SLEEP_MS)
    retirement = environments * (ceil_ms((180 + DRIVER_HTTP_SECONDS + COMMAND_SECONDS) * 1000 + POLL_SLEEP_MS)
                                  + 2 * COMMAND_SECONDS)
    per_run_cleanup=environments*iterations*(2*COMMAND_SECONDS+2*DRIVER_HTTP_SECONDS) if execution_mode=='perRun' else 0
    final_inventory=COMMAND_SECONDS if execution_mode=='perRun' else 0
    if execution_mode=='perRun':pause=retirement=0
    budget = {'schema': 'workspace.perf.workload_budget.v1', 'executionMode':execution_mode, 'k6': k6,
              'httpRequestSeconds': HTTP_REQUEST_SECONDS, 'artifactDownloadSeconds': ARTIFACT_DOWNLOAD_SECONDS,
              'k6ProcessSeconds': process, 'processCleanupSeconds': process_cleanup,
              'driverInitializationSeconds': 3 * COMMAND_SECONDS + 3 * DRIVER_HTTP_SECONDS,
              'completedReadbackSeconds': completed,
              'durableTimingSeconds': ceil_ms((FINAL_JOB_SETTLE_SECONDS + COMMAND_SECONDS) * 1000 + POLL_SLEEP_MS),
              'finalPauseSeconds': pause, 'retirementSeconds': retirement,
              'perRunCleanupReadbackSeconds':per_run_cleanup,'finalManagedInventorySeconds':final_inventory,
              'observerStopSeconds': 30,  # stop() plus one finally retry, each join <= 15.
              'outerCaptureSeconds': 10, 'limitSeconds': LIMIT_SECONDS}
    budget['coordinatorSeconds'] = (process + process_cleanup + budget['driverInitializationSeconds']
        + completed + budget['durableTimingSeconds'] + pause + retirement + per_run_cleanup + final_inventory
        + budget['observerStopSeconds'] + budget['outerCaptureSeconds'])
    if budget['coordinatorSeconds'] > LIMIT_SECONDS:
        raise ValueError('rolling workload exceeds the four-hour total budget')
    return budget
