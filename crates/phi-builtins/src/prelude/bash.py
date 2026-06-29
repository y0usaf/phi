def bash(cmd, timeout=120):
    try:
        proc = _subprocess.run(
            cmd, shell=True, capture_output=True, text=True, timeout=timeout
        )
    except _subprocess.TimeoutExpired:
        return "Error: command timed out after %ss" % timeout
    out = proc.stdout
    if proc.stderr:
        out += ("\n" if out else "") + proc.stderr
    if proc.returncode != 0:
        out += ("\n" if out else "") + "[exit code %d]" % proc.returncode
    return _prelude_truncate(out) if out else "(no output)"


@_tool
