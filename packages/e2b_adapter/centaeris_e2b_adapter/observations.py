"""Optional local SDK durations; observations never grant dispatch authority."""
import json
import math
import re
import sys
from time import monotonic

_SCHEMA = "runtime.e2b.power_timing.v1"
_STARTUP_STAGES = frozenset(("sdkImport", "sdkClientSetup"))
_REQUEST_STAGES = frozenset(("sdkIdentityGet", "sdkPause", "sdkConnect", "sdkResumeVerifyGet", "sdkHandle"))
_OPERATIONS = frozenset(("capabilities", "pause", "connect"))
_REQUEST_ID = re.compile(r"[A-Za-z0-9][A-Za-z0-9._:-]{0,127}\Z")
_MAX_LINE_BYTES = 1024


class _Span:
    def __init__(self, timing, stage, request_id, operation, enabled):
        self._timing = timing
        self._stage = stage
        self._request_id = request_id
        self._operation = operation
        self._enabled = enabled
        self._started = None
        self.outcome = "ok"

    def __enter__(self):
        if self._enabled:
            try:
                self._started = self._timing.clock()
            except BaseException:
                pass
        return self

    def __exit__(self, kind, _error, _traceback):
        if kind is not None:
            self.outcome = "error"
        if self._started is not None:
            try:
                elapsed_ms = (self._timing.clock() - self._started) * 1000
                self._timing.emit(self._stage, elapsed_ms, self.outcome, self._request_id, self._operation)
            except BaseException:
                # Timing clocks and writers cannot change provider execution.
                pass
        return False


class _RequestContext:
    def __init__(self, timing, request_id=None, operation=None, enabled=False, stages=_REQUEST_STAGES):
        self._timing = timing
        self._request_id = request_id
        self._operation = operation
        self._enabled = enabled
        self._stages = stages

    @property
    def enabled(self):
        return self._enabled

    def span(self, stage):
        return _Span(self._timing, stage, self._request_id, self._operation,
                     self._enabled and stage in self._stages)


class PowerTiming:
    def __init__(self, enabled=False, *, writer=None, clock=None):
        self._enabled = enabled is True
        self._writer = writer if writer is not None else (sys.stderr if self._enabled else None)
        self.clock = monotonic if clock is None else clock

    def startup_span(self, stage):
        return _Span(self, stage, None, None, self._enabled and stage in _STARTUP_STAGES)

    def request_context(self, request):
        if self._enabled and type(request) is dict:
            request_id, operation = request.get("requestId"), request.get("operation")
            # Correlation IDs are public bounded identifiers, never arbitrary
            # URLs or diagnostics copied into a measurement stream.
            if (isinstance(request_id, str) and _REQUEST_ID.fullmatch(request_id)
                    and isinstance(operation, str) and operation in _OPERATIONS):
                return _RequestContext(self, request_id, operation, True)
        return _RequestContext(self)

    def emit(self, stage, value, outcome, request_id, operation):
        if (not self._enabled or type(value) not in (int, float)
                or not math.isfinite(value) or value < 0 or outcome not in ("ok", "error")):
            return
        if stage in _STARTUP_STAGES:
            if request_id is not None or operation is not None:
                return
        elif (stage not in _REQUEST_STAGES or not isinstance(request_id, str)
                or not _REQUEST_ID.fullmatch(request_id) or operation not in _OPERATIONS):
            return
        record = {"schema": _SCHEMA, "stage": stage, "value": value, "outcome": outcome,
                  "requestId": request_id, "operation": operation}
        try:
            line = json.dumps(record, ensure_ascii=True, allow_nan=False, separators=(",", ":")) + "\n"
            if len(line.encode("ascii")) <= _MAX_LINE_BYTES:
                self._writer.write(line)
                self._writer.flush()
        except Exception:
            pass


_SDK_STAGE_OPERATIONS = {
    "sdkHandle": frozenset(("create", "runCommand", "waitCommand", "recoverCommand", "uploadFile", "downloadFile")),
    "sdkCreate": frozenset(("create",)),
    "sdkCreateVerifyGet": frozenset(("create",)),
    "sdkCommandStart": frozenset(("runCommand",)),
    "sdkCommandWait": frozenset(("waitCommand", "recoverCommand")),
    "sdkFileUpload": frozenset(("uploadFile",)),
    "sdkFileDownload": frozenset(("downloadFile",)),
}


class SdkTiming(PowerTiming):
    """Separate closed SDK boundaries; the existing power wire stays unchanged."""
    def request_context(self, request):
        if self._enabled and type(request) is dict:
            request_id, operation = request.get("requestId"), request.get("operation")
            if (type(request_id) is str and _REQUEST_ID.fullmatch(request_id)
                    and type(operation) is str and operation in _SDK_STAGE_OPERATIONS["sdkHandle"]):
                stages = frozenset(stage for stage, operations in _SDK_STAGE_OPERATIONS.items() if operation in operations)
                return _RequestContext(self, request_id, operation, True, stages)
        return _RequestContext(self, stages=frozenset())

    def emit(self, stage, value, outcome, request_id, operation):
        if (not self._enabled or type(stage) is not str or type(operation) is not str
                or operation not in _SDK_STAGE_OPERATIONS.get(stage, ())
                or type(request_id) is not str or not _REQUEST_ID.fullmatch(request_id)
                or type(value) not in (int, float) or not math.isfinite(value) or value < 0
                or type(outcome) is not str or outcome not in ("ok", "error")):
            return
        record = {"schema": "runtime.e2b.sdk_timing.v1", "stage": stage, "value": value,
            "outcome": outcome, "requestId": request_id, "operation": operation}
        try:
            line = json.dumps(record, ensure_ascii=True, allow_nan=False, separators=(",", ":")) + "\n"
            if len(line.encode("ascii")) <= _MAX_LINE_BYTES:
                self._writer.write(line)
                self._writer.flush()
        except BaseException:
            pass
