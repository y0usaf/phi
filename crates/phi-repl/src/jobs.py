"""Background job system for the phi REPL driver.

Extracted from driver.py — manages the job lifecycle: each execution runs on
its own daemon thread with output captured via the stdout/stderr routers.
Orthogonal to namespace management and the protocol layer."""

import io
import threading
import time


_jobs = {}
_jobs_lock = threading.Lock()
_job_counter = [0]


class _Job:
    """One execution on its own thread, output captured via the routers."""

    def __init__(self, code, env):
        with _jobs_lock:
            _job_counter[0] += 1
            self.id = _job_counter[0]
            _jobs[self.id] = self
        self.code = code
        self.env = env
        self.out = io.StringIO()
        self.err = io.StringIO()
        self.status = "running"
        self.started = time.time()
        self.finished = None
        self.thread = threading.Thread(target=self._run, daemon=True)
        self.thread.start()

    def _run(self):
        _stdout_router.register(self.out)
        _stderr_router.register(self.err)
        try:
            self.env._run_code(self.code)
            self.status = "done"
        except BaseException as e:
            self.err.write("\n%s: %s" % (type(e).__name__, e))
            self.status = "error"
        finally:
            self.finished = time.time()

    def runtime(self):
        end = self.finished if self.finished is not None else time.time()
        return end - self.started

    def output(self):
        out = self.out.getvalue()
        err = self.err.getvalue()
        if not err:
            return out
        if out and not out.endswith("\n"):
            out += "\n"
        return out + err

    def describe(self):
        if isinstance(self.code, str):
            head = " ".join(self.code.split())[:60]
        else:
            head = getattr(self.code, "__name__", repr(self.code))
        return "job %d [%s, %.1fs]: %s" % (self.id, self.status, self.runtime(), head)


def _get_job(jid):
    with _jobs_lock:
        job = _jobs.get(jid)
    if job is None:
        raise KeyError("no such job: %r" % (jid,))
    return job
