"""phi REPL driver: persistent Python namespace, ndjson protocol over stdio.

Frames from host: setup, exec {id, code, timeout?}, subcall_result
{id, text|value|error}, answer_state {id}, shutdown.
Frames to host: ready {error?}, exec_result {id, stdout, stderr, vars,
final_answer, truncated}, subcall {id, kind, ...}, answer_state {id, content,
ready}, tool_event {phase, name, call_id, ...}, fatal {error}.

The main thread reads stdin and dispatches; setup/exec run on a single worker
thread so subcall round-trips (which block the worker) can still be served.

Every execution runs on its own thread as a job. Foreground execs join that
thread up to the soft `timeout`; if the code is still running it keeps going
as a background job and the exec_result reports the job id plus output so
far, instead of the host killing the interpreter (which would lose the
namespace). `bg()` / `jobs()` / `job_status()` / `job_output()` expose the
same machinery to REPL code. Note: the host only reads our stdout while it
waits on a foreground exec, so `llm_query` etc. issued from a background job
block until the next foreground exec is pumping.
"""

import io
import json
import os
import queue
import sys
import tempfile
import threading
import time
import traceback
from concurrent.futures import ThreadPoolExecutor

_real_stdout = sys.stdout
_send_lock = threading.Lock()


class _Router(io.TextIOBase):
    """stdout/stderr replacement routing writes to the buffer registered for
    the current thread, so concurrent jobs capture output independently."""

    def __init__(self):
        self._local = threading.local()

    def register(self, buf):
        self._local.buf = buf

    def writable(self):
        return True

    def write(self, s):
        buf = getattr(self._local, "buf", None)
        if buf is not None:
            buf.write(s)
        return len(s)

    def flush(self):
        pass


_stdout_router = _Router()
_stderr_router = _Router()
sys.stdout = _stdout_router
sys.stderr = _stderr_router


def _send(frame):
    with _send_lock:
        _real_stdout.write(json.dumps(frame, default=str) + "\n")
        _real_stdout.flush()


_pending = {}
_pending_lock = threading.Lock()
_next_id = 0


def _subcall(payload):
    """Send a subcall frame and block until the host replies."""
    global _next_id
    event = threading.Event()
    with _pending_lock:
        _next_id += 1
        sid = _next_id
        _pending[sid] = [event, None]
    payload = dict(payload, type="subcall", id=sid)
    _send(payload)
    event.wait()
    with _pending_lock:
        return _pending.pop(sid)[1]


def _tool_event(phase, name, call_id, fields=None):
    """Fire-and-forget notification that a prelude tool started/finished.
    The host forwards these to its event stream; no reply is expected."""
    frame = dict(fields or {}, type="tool_event", phase=phase, name=name, call_id=call_id)
    _send(frame)


def _message(text):
    """Send a chat message to the user immediately, without ending the run.
    Fire-and-forget: the host renders it as an assistant message."""
    _send({"type": "message", "text": str(text)})


def _make_host_fn(name):
    def shim(*args, **kwargs):
        try:
            json.dumps([list(args), kwargs])
        except (TypeError, ValueError):
            raise TypeError(
                "%s(): arguments must be JSON-serializable" % name
            ) from None
        frame = _subcall(
            {"kind": "fn", "name": name, "args": list(args), "kwargs": kwargs}
        )
        if frame.get("error") is not None:
            raise RuntimeError(str(frame["error"]))
        return frame.get("value")

    shim.__name__ = name
    return shim


def _clean(text):
    return text.encode("utf-8", "replace").decode("utf-8")






class _AnswerDict(dict):
    """REPL-visible dict where ``answer["ready"] = True`` signals completion."""

    def __init__(self, on_ready=None):
        super().__init__()
        super().__setitem__("content", "")
        super().__setitem__("ready", False)
        self._on_ready = on_ready

    def __setitem__(self, key, value):
        super().__setitem__(key, value)
        if key == "ready" and value and self._on_ready is not None:
            try:
                self._on_ready(self.get("content", ""))
            except Exception:
                pass


# When a subcall result exceeds this many chars, the full text is written to
# a tmp file and printing the value shows a preview + the path instead of the
# whole thing. REPL stdout enters the model's conversation history (and is
# truncated at max_output_chars), so dumping a huge subagent result there
# both blows context and loses the tail. Ported from upstream rlm's
# SubagentResult (ref/rlm/rlm/environments/local_repl.py).
SPILL_THRESHOLD = 50_000


class SubagentResult:
    """String-like wrapper for large subcall results.

    Below SPILL_THRESHOLD the raw ``str`` is returned instead of this class,
    so a wrapper only ever exists for spilled results. ``str(result)`` (and
    thus ``print``) yields a short preview plus the tmp-file path; the full
    text stays available as ``result.full_result``, by slicing, or by reading
    the file. Non-dunder string methods delegate to the full text.
    """

    __slots__ = ("full_result", "tmp_path", "_char_count")

    def __init__(self, response, tmp_dir):
        self.full_result = response
        self._char_count = len(response)
        fd, self.tmp_path = tempfile.mkstemp(
            prefix="phi-subagent-", suffix=".log", dir=tmp_dir
        )
        with os.fdopen(fd, "w") as f:
            f.write(response)

    def __getattr__(self, name):
        if name.startswith("_") or name == "full_result":
            raise AttributeError(name)
        return getattr(self.full_result, name)

    def __str__(self):
        preview = self.full_result[:500]
        return (
            "%s\n... + [%d chars]\n[Full output: %s — access via .full_result, "
            "slicing, or read_file]" % (preview, self._char_count - 500, self.tmp_path)
        )

    def __repr__(self):
        return "SubagentResult(%d chars -> %s)" % (self._char_count, self.tmp_path)

    def __len__(self):
        return self._char_count

    def __getitem__(self, key):
        return self.full_result[key]

    def __iter__(self):
        return iter(self.full_result)

    def __contains__(self, item):
        return item in self.full_result

    def __eq__(self, other):
        if isinstance(other, SubagentResult):
            return self.full_result == other.full_result
        return self.full_result == other

    def __hash__(self):
        return hash(self.full_result)

    def __add__(self, other):
        if isinstance(other, SubagentResult):
            return self.full_result + other.full_result
        return self.full_result + other

    def __radd__(self, other):
        return other + self.full_result







class Env:
    def __init__(self, setup):
        self.max_output = int(setup.get("max_output_chars") or 20000)
        self.max_concurrent = int(setup.get("max_concurrent_subcalls") or 4)
        self.temp_dir = tempfile.mkdtemp(prefix="phi_repl_")
        self.work_dir = setup.get("cwd") or self.temp_dir
        os.chdir(self.work_dir)
        self.final_answer = None
        self._ns_lock = threading.Lock()
        self.globals = {
            "__builtins__": dict(_SAFE_BUILTINS),
            "__name__": "__main__",
            "SHOW_VARS": self._show_vars,
            "llm_query": self._llm_query,
            "llm_query_batched": self._llm_query_batched,
            "subagent": self._subagent,
            "subagent_batched": self._subagent_batched,
            "bg": self._bg,
            "jobs": self._jobs_fn,
            "job_status": self._job_status,
            "job_output": self._job_output,
            "__phi_tool_event__": _tool_event,
            "message": _message,
            "_subcall": _subcall,
        }
        self.locals = {"answer": _AnswerDict(self._capture_answer)}
        for name in setup.get("host_fns") or []:
            self.globals[name] = _make_host_fn(name)
        self._load_context(setup)
        if setup.get("setup_code"):
            self.execute(setup["setup_code"])
        # Scaffold snapshot: everything the host installed (context vars plus
        # every prelude-defined tool, which lands in locals via the setup_code
        # exec above). `_restore_scaffold` reasserts these bindings after every
        # exec so model code can't corrupt the environment by rebinding
        # `read_file`, `context`, etc. `answer` is excluded — it has its own
        # re-wrap logic that must not resurrect stale content.
        self._scaffold = {k: v for k, v in self.locals.items() if k != "answer"}

    def _load_context(self, setup):
        context_str = setup.get("context_str")
        context = setup.get("context")
        if context_str is not None:
            path = os.path.join(self.temp_dir, "context_0.txt")
            with open(path, "w") as f:
                f.write(context_str)
            self.locals["context_0"] = context_str
        elif context is not None:
            path = os.path.join(self.temp_dir, "context_0.json")
            with open(path, "w") as f:
                json.dump(context, f)
            self.locals["context_0"] = context
        if "context_0" in self.locals:
            self.locals["context"] = self.locals["context_0"]

    def _capture_answer(self, content):
        # A spilled subcall result assigned to answer['content'] must submit
        # the full text, not the preview-with-path that str() would give.
        if isinstance(content, SubagentResult):
            self.final_answer = content.full_result
        else:
            self.final_answer = str(content)

    def _show_vars(self):
        available = {
            k: type(v).__name__
            for k, v in self.locals.items()
            if not k.startswith("_") and k != "answer"
        }
        if not available:
            return "No variables created yet. Use ```repl``` blocks to create variables."
        return "Available variables: %s" % available

    def _spill(self, text):
        """Wrap oversized subcall text so printing it can't flood stdout."""
        if isinstance(text, str) and len(text) > SPILL_THRESHOLD:
            return SubagentResult(text, self.temp_dir)
        return text

    def _llm_query(self, prompt, model=None):
        frame = _subcall({"kind": "llm", "prompt": str(prompt), "model": model})
        if frame.get("error") is not None:
            return "Error: %s" % frame["error"]
        return self._spill(frame.get("text", ""))

    def _subagent(self, prompt, model=None, preamble=None):
        payload = {"kind": "rlm", "prompt": str(prompt), "model": model}
        if preamble is not None:
            payload["preamble"] = str(preamble)
        frame = _subcall(payload)
        if frame.get("error") is not None:
            return {"text": "Error: %s" % frame["error"], "stop": "failed", "iterations": 0, "ok": False}
        # The host returns a dict via frame["value"]; return it verbatim,
        # spilling an oversized 'text' field to a tmp file.
        value = frame.get("value", frame.get("text", ""))
        if isinstance(value, dict):
            value["text"] = self._spill(value.get("text", ""))
        return value

    def _batched(self, fn, prompts, model, **kwargs):
        prompts = list(prompts)
        if not prompts:
            return []
        if len(prompts) == 1:
            return [fn(prompts[0], model, **kwargs)]
        workers = min(self.max_concurrent, len(prompts))
        with ThreadPoolExecutor(max_workers=workers) as pool:
            return list(pool.map(lambda p: fn(p, model, **kwargs), prompts))

    def _llm_query_batched(self, prompts, model=None):
        return self._batched(self._llm_query, prompts, model)

    def _subagent_batched(self, prompts, model=None, preamble=None):
        return self._batched(self._subagent, prompts, model, preamble=preamble)

    def _restore_scaffold(self):
        """Undo model overwrites of scaffold names so the next cell still works.

        The function scaffold in ``self.globals`` (llm_query, subagent, host
        fns, ...) is copy-protected by construction — each exec runs against a
        fresh ``dict(self.globals)`` and the copy-back loop skips global names.
        What model code CAN corrupt persistently: prelude tools and context
        vars (they live in ``self.locals``), the ``answer`` dict, and the
        shared ``__builtins__`` dict (mutated in place). All three are
        restored here after every exec (upstream: ``_restore_scaffold``,
        ref/rlm/rlm/environments/local_repl.py).
        """
        # __builtins__ is shared by reference into every exec's namespace
        # copy, so in-place mutation would otherwise persist.
        self.globals["__builtins__"] = dict(_SAFE_BUILTINS)
        for name, value in getattr(self, "_scaffold", {}).items():
            if self.locals.get(name) is not value:
                self.locals[name] = value
        current = self.locals.get("answer")
        if not isinstance(current, _AnswerDict):
            replacement = _AnswerDict(self._capture_answer)
            if isinstance(current, dict):
                for k, v in current.items():
                    dict.__setitem__(replacement, k, v)
                if current.get("ready") and self.final_answer is None:
                    self.final_answer = str(current.get("content", ""))
            self.locals["answer"] = replacement
        if "context_0" in self.locals:
            self.locals["context"] = self.locals["context_0"]

    def _var_names(self):
        return [
            k
            for k, v in self.locals.items()
            if not k.startswith("_")
            and k != "answer"
            and isinstance(
                v, (str, int, float, bool, list, dict, tuple, set, SubagentResult)
            )
        ]

    def _truncate(self, text):
        if len(text) <= self.max_output:
            return text, False
        extra = len(text) - self.max_output
        return text[: self.max_output] + "... + [%d chars...]" % extra, True

    def _run_code(self, code):
        """Run a code string (or callable) against the shared namespace."""
        if callable(code):
            code()
            with self._ns_lock:
                self._restore_scaffold()
            return
        with self._ns_lock:
            combined = dict(self.globals)
            combined.update(self.locals)
        exec(code, combined, combined)
        with self._ns_lock:
            for key, value in combined.items():
                if key not in self.globals and not key.startswith("_"):
                    self.locals[key] = value
            self._restore_scaffold()

    def _bg(self, code):
        if not (isinstance(code, str) or callable(code)):
            raise TypeError("bg(): pass a code string or a zero-argument callable")
        return _Job(code, self).id

    def _jobs_fn(self):
        with _jobs_lock:
            items = sorted(_jobs.values(), key=lambda j: j.id)
        if not items:
            return "No background jobs."
        return "\n".join(j.describe() for j in items)

    def _job_status(self, jid):
        return _get_job(jid).describe()

    def _job_output(self, jid):
        return _get_job(jid).output()

    def execute(self, code, soft_timeout=None):
        job = _Job(code, self)
        job.thread.join(soft_timeout)
        if job.thread.is_alive():
            out = job.out.getvalue()
            if out and not out.endswith("\n"):
                out += "\n"
            stdout = out + (
                "[still running after %.0fs: continuing as background job %d; "
                "use job_status(%d) / job_output(%d) to check on it]"
                % (job.runtime(), job.id, job.id, job.id)
            )
            stderr = job.err.getvalue()
        else:
            # Finished foreground runs don't clutter the job table.
            with _jobs_lock:
                _jobs.pop(job.id, None)
            stdout = job.out.getvalue()
            stderr = job.err.getvalue()
        stdout, t_out = self._truncate(_clean(stdout))
        stderr, t_err = self._truncate(_clean(stderr))
        final = self.final_answer
        self.final_answer = None
        return {
            "stdout": stdout,
            "stderr": stderr,
            "vars": self._var_names(),
            "final_answer": final,
            "truncated": t_out or t_err,
        }


_exec_queue = queue.Queue()
_env = None


def _worker():
    global _env
    while True:
        frame = _exec_queue.get()
        if frame is None:
            return
        kind = frame.get("type")
        try:
            if kind == "setup":
                _env = Env(frame)
                _send({"type": "ready"})
            elif kind == "exec":
                result = _env.execute(frame.get("code") or "", frame.get("timeout"))
                result["type"] = "exec_result"
                result["id"] = frame.get("id")
                _send(result)
        except Exception:
            if kind == "setup":
                _send({"type": "ready", "error": traceback.format_exc()})
            else:
                _send({"type": "fatal", "error": traceback.format_exc()})


def main():
    threading.Thread(target=_worker, daemon=True).start()
    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        frame = json.loads(line)
        kind = frame.get("type")
        if kind in ("setup", "exec"):
            _exec_queue.put(frame)
        elif kind == "subcall_result":
            with _pending_lock:
                entry = _pending.get(frame.get("id"))
            if entry is not None:
                entry[1] = frame
                entry[0].set()
        elif kind == "answer_state":
            ans = _env.locals.get("answer") if _env is not None else None
            content = str(ans.get("content", "")) if isinstance(ans, dict) else ""
            ready = bool(ans.get("ready")) if isinstance(ans, dict) else False
            _send(
                {
                    "type": "answer_state",
                    "id": frame.get("id"),
                    "content": content,
                    "ready": ready,
                }
            )
        elif kind == "shutdown":
            break


if __name__ == "__main__":
    try:
        main()
    except Exception:
        sys.__stderr__.write(traceback.format_exc())
        sys.exit(1)
