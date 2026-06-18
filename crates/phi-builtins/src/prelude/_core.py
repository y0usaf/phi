import subprocess as _subprocess
import os as _os
import re as _re
import json as _json
import zlib as _zlib
import fnmatch as _fnmatch
import inspect as _inspect
import time as _time
import difflib as _difflib
from concurrent.futures import ThreadPoolExecutor as _ThreadPoolExecutor

_PRELUDE_MAX_LINES = 2000
_PRELUDE_MAX_BYTES = 50_000
_PRELUDE_SKIP_DIRS = (".git", "node_modules", "target", "__pycache__", ".venv", "dist", "build")

_tool_calls = [0]


def _tool(fn):
    """Emit driver tool_event frames around each call so the host TUI can
    render the call nested inside its ```repl``` block, live."""

    def _wrapped(*args, **kwargs):
        _tool_calls[0] += 1
        cid = _tool_calls[0]
        try:
            params = dict(_inspect.signature(fn).bind(*args, **kwargs).arguments)
        except TypeError:
            params = {}
        __phi_tool_event__("start", fn.__name__, cid, {"args": params})
        t0 = _time.time()
        try:
            result = fn(*args, **kwargs)
        except BaseException as e:
            __phi_tool_event__(
                "end",
                fn.__name__,
                cid,
                {"error": "%s: %s" % (type(e).__name__, e), "duration": _time.time() - t0},
            )
            raise
        error = result if isinstance(result, str) and result.startswith("Error:") else None
        __phi_tool_event__(
            "end",
            fn.__name__,
            cid,
            {
                "result": result if isinstance(result, str) else repr(result),
                "error": error,
                "duration": _time.time() - t0,
            },
        )
        return result

    _wrapped.__name__ = fn.__name__
    _wrapped.__doc__ = fn.__doc__
    return _wrapped


def _prelude_truncate(text):
    lines = text.split("\n")
    if len(lines) > _PRELUDE_MAX_LINES:
        text = "\n".join(lines[:_PRELUDE_MAX_LINES])
        text += "\n[truncated: %d more lines]" % (len(lines) - _PRELUDE_MAX_LINES)
    if len(text) > _PRELUDE_MAX_BYTES:
        text = text[:_PRELUDE_MAX_BYTES] + "\n[truncated at %d bytes]" % _PRELUDE_MAX_BYTES
    return text


def _format_unified_diff(path, before, after):
    before_lines = before.splitlines(True)
    after_lines = after.splitlines(True)
    diff = "".join(
        _difflib.unified_diff(
            before_lines,
            after_lines,
            fromfile=path,
            tofile=path,
            lineterm="",
        )
    )
    return _prelude_truncate(diff) if diff else "(no diff)"


@_tool
