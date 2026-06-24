import os
os.chdir("/home/y0usaf/Dev/phi")
f = "crates/phi-session/src/tui.rs"
s = open(f).read()
old = 'const PANEL_REPL: &str = "'
new = '#[allow(dead_code)]
const PANEL_REPL: &str = "'
s = s.replace(old, new, 1)
old = 'const USER_BORDER: &str = "'
new = '#[allow(dead_code)]
const USER_BORDER: &str = "'
s = s.replace(old, new, 1)
old = 'const ASSISTANT_BORDER: &str = "'
new = '#[allow(dead_code)]
const ASSISTANT_BORDER: &str = "'
s = s.replace(old, new, 1)
open(f, "w").write(s)
print("DONE")
