"""``test.libregrtest.result`` — per-test outcome classification.

A faithful subset of CPython 3.13's ``Lib/test/libregrtest/result.py``:
the ``State`` constants and the ``TestResult`` record the runner fills in
for each module it executes.
"""


class State:
    PASSED = "PASSED"
    FAILED = "FAILED"
    SKIPPED = "SKIPPED"
    UNCAUGHT_EXC = "UNCAUGHT_EXC"
    REFLEAK = "REFLEAK"
    ENV_CHANGED = "ENV_CHANGED"
    RESOURCE_DENIED = "RESOURCE_DENIED"
    INTERRUPTED = "INTERRUPTED"
    MULTIPROCESSING_ERROR = "MULTIPROCESSING_ERROR"
    DID_NOT_RUN = "DID_NOT_RUN"
    TIMEOUT = "TIMEOUT"

    @staticmethod
    def is_failed(state):
        return state in {
            State.FAILED,
            State.UNCAUGHT_EXC,
            State.REFLEAK,
            State.MULTIPROCESSING_ERROR,
            State.TIMEOUT,
        }

    @staticmethod
    def has_meaningful_duration(state):
        return state not in {
            State.SKIPPED,
            State.RESOURCE_DENIED,
            State.INTERRUPTED,
            State.DID_NOT_RUN,
        }

    @staticmethod
    def must_stop(state):
        return state in {State.INTERRUPTED, State.MULTIPROCESSING_ERROR}


class TestResult:
    def __init__(self, test_name, state=None, duration=None, errors=None,
                 failures=None, stats=None):
        self.test_name = test_name
        self.state = state
        self.duration = duration
        # (name, formatted-traceback) pairs for failed/errored cases.
        self.errors = errors
        self.failures = failures
        # (run, failures, errors, skipped) counts.
        self.stats = stats

    def is_failed(self, fail_env_changed=False):
        if self.state == State.ENV_CHANGED:
            return fail_env_changed
        return State.is_failed(self.state)

    def __str__(self):
        return "%s: %s" % (self.test_name, self.state)

    def __repr__(self):
        return "<TestResult %s (%s)>" % (self.test_name, self.state)
