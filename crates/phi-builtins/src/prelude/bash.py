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
    if not out:
        return "(no output)"
    # Pi-style spillover: if the output exceeds the byte limit, spill the
    # full output to a tmp file and return a truncated tail plus a path
    # annotation. This keeps REPL memory bounded while preserving a route
    # back to the complete data. The tmp file is ephemeral — the OS cleans
    # it up on process exit or reboot.
    if len(out) > _PRELUDE_MAX_BYTES:
        return _spill_to_tmp(out)
    return _prelude_truncate(out)


@_tool
