"""phi REPL driver: persistent Python namespace, ndjson protocol over stdio.

Frames from host: setup, exec {id, code}, subcall_result
{id, text|value|error}, answer_state {id}, shutdown.
Frames to host: ready {error?}, exec_result {id, stdout, stderr, vars,
final_answer, final_summary, truncated}, subcall {id, kind, ...}, answer_state
{id, content, ready}, tool_event {phase, name, call_id, ...}, fatal {error}.

The main thread reads stdin and dispatches; setup/exec run on a single worker
thread so subcall round-trips (which block the worker) can still be served.

Executions run serially on the worker thread and complete within the host's
hard timeout. If the host decides a run is wedged, it kills and respawns the
interpreter with a fresh namespace (one lifetime model); there is no
software soft-timeout / background-job layer. Subcalls (`llm_query`/`subagent`)
and host fns issued by exec code block on the round-trip reply pumped by this
same process.
"""

# The driver script is spawned from a temp file, so the system temp dir
# lands on sys.path[0]. That lets unrelated files shadow stdlib modules -- a
# stray /tmp/inspect.py breaks 'import inspect' inside traceback and kills
# the driver at startup. Drop the script dir from the import path before any
# stdlib import below.
import sys as _sys
import os as _os
_phi_script_dir = _os.path.dirname(_os.path.abspath(__file__))
_sys.path = [p for p in _sys.path if _os.path.abspath(p) != _phi_script_dir]
import io
import json
import os
import queue
import sys
import tempfile
import threading
import traceback
from dataclasses import dataclass

_real_stdout = sys.stdout
_send_lock = threading.Lock()


def _send(frame):
    with _send_lock:
        _real_stdout.write(json.dumps(frame, default=str) + "\n")
        _real_stdout.flush()


_pending = {}
_pending_lock = threading.Lock()
_next_id = 0


def _subcall(payload):
    """Send a subcall and block until the host replies."""
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


def _ext_tool(name, args):
    """Invoke a host-registered extension tool (the target of the generated
    prelude stubs). Blocks on the subcall round-trip; errors surface as
    exceptions so stub callers see a normal Python failure."""
    frame = _subcall({"kind": "ext_tool", "name": name, "args": args})
    if frame.get("error") is not None:
        raise RuntimeError(str(frame["error"]))
    if frame.get("text") is not None:
        return frame["text"]
    return frame.get("value", "")


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
            "subagent": self._subagent,
            "__phi_tool_event__": _tool_event,
            "__phi_ext_tool__": _ext_tool,
            "__phi_native_call__": self._native_call,
            "message": _message,
            "_subcall": _subcall,
        }
        self.locals = {"answer": _AnswerDict(self._capture_answer)}
        for name in setup.get("host_fns") or []:
            self.globals[name] = _make_host_fn(name)
        self._load_context(setup)
        if setup.get("setup_code"):
            self._run_code(setup["setup_code"])
        # Scaffold snapshot: everything the host installed (context vars plus
        # every prelude-defined tool, which lands in locals via the setup_code
        # exec above). `_restore_scaffold` reasserts these bindings after every
        # exec so model code can't corrupt the environment by rebinding
        # `read_file`, `context`, etc.
        self._scaffold = {k: v for k, v in self.locals.items() if k != "answer"}

    def _load_context(self, setup):
        context = setup.get("context")
        if context is not None:
            self.locals["context_0"] = context
            self.locals["context"] = context

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

    def _native_call(self, name, args_json):
        """Entry point for native tool calls dispatched into the env: call
        the same-named function (usually prelude-defined, living in locals)
        with the JSON-decoded kwargs and print a non-None result so it flows
        through the normal stdout capture/truncation path."""
        fn = self.locals.get(name, self.globals.get(name))
        if not callable(fn):
            raise NameError(
                "no tool function named %r in the REPL environment" % name
            )
        result = fn(**json.loads(args_json))
        if result is not None:
            print(result)

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
            err = "Error: %s" % frame["error"]
            return {"text": err, "summary": err, "stop": "failed", "iterations": 0, "ok": False}
        # The host returns a dict via frame["value"]; return it verbatim,
        # spilling an oversized 'text' field to a tmp file.
        value = frame.get("value", frame.get("text", ""))
        if isinstance(value, dict):
            value["text"] = self._spill(value.get("text", ""))
        return value

    def _restore_scaffold(self):
        """Undo model overwrites of scaffold names so the next cell still works.

        The function scaffold in ``self.globals`` (llm_query, subagent, host
        fns, ...) is copy-protected by construction -- each exec runs against a
        fresh ``dict(self.globals)`` and the copy-back loop skips global names.
        What model code CAN corrupt persistently: prelude tools and context
        vars (they live in ``self.locals``), the ``answer`` dict, and the
        shared ``__builtins__`` dict (mutated in place). All three are
        restored here after every exec.
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
        """Run a code string against the shared namespace, capturing stdout
        and stderr; an execution error is folded into the stderr string so the
        environment survives and the namespace still advances."""
        with self._ns_lock:
            combined = dict(self.globals)
            combined.update(self.locals)
        out = io.StringIO()
        err = io.StringIO()
        real_out, real_err = sys.stdout, sys.stderr
        sys.stdout, sys.stderr = out, err
        try:
            exec(code, combined, combined)
        except BaseException as e:
            err.write("\n%s: %s" % (type(e).__name__, e))
        finally:
            sys.stdout, sys.stderr = real_out, real_err
        with self._ns_lock:
            for key, value in combined.items():
                if key not in self.globals and not key.startswith("_"):
                    self.locals[key] = value
            self._restore_scaffold()
        return out.getvalue(), err.getvalue()

    def execute(self, code):
        stdout, stderr = self._run_code(code)
        stdout, t_out = self._truncate(_clean(stdout))
        stderr, t_err = self._truncate(_clean(stderr))
        final = self.final_answer
        self.final_answer = None
        return {
            "stdout": stdout,
            "stderr": stderr,
            "vars": self._var_names(),
            "final_answer": final,
            "final_summary": self._current_summary(),
            "truncated": t_out or t_err,
        }

    def _current_summary(self):
        """The model-authored report in ``answer['summary']``, if any.

        Read from the live answer dict on every exec (rather than captured on
        ``ready``) so a summary set before a prose finish still reaches the
        host -- the summary is a standing declaration, valid regardless of how
        the run ends. Bounding happens host-side; a spilled value submits its
        full text, same rule as ``_capture_answer``.
        """
        current = self.locals.get("answer")
        if not isinstance(current, dict):
            return None
        summary = current.get("summary")
        if summary is None:
            return None
        if isinstance(summary, SubagentResult):
            summary = summary.full_result
        summary = str(summary)
        return summary if summary.strip() else None


exec_queue = queue.Queue()
env = None


def _worker():
    global env
    while True:
        frame = exec_queue.get()
        if frame is None:
            return
        kind = frame.get("type")
        try:
            if kind == "setup":
                env = Env(frame)
                _send({"type": "ready"})
            elif kind == "exec":
                result = env.execute(frame.get("code") or "")
                result["type"] = "exec_result"
                result["id"] = frame.get("id")
                _send(result)
        except Exception:
            if kind == "setup":
                _send({"type": "ready", "error": traceback.format_exc()})
            else:
                _send({"type": "fatal", "error": traceback.format_exc()})


def main():
    threading.Thread(
        target=_worker, daemon=True
    ).start()
    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        frame = json.loads(line)
        kind = frame.get("type")
        if kind in ("setup", "exec"):
            exec_queue.put(frame)
        elif kind == "subcall_result":
            with _pending_lock:
                entry = _pending.get(frame.get("id"))
            if entry is not None:
                entry[1] = frame
                entry[0].set()
        elif kind == "answer_state":
            ans = env.locals.get("answer") if env is not None else None
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