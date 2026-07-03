//! Host-side glue for Lua-registered overlays (WS1.6): builds the
//! [`OverlaySpec`] whose closures round-trip through the Lua thread, so a
//! Lua overlay gets the same protocol builtin overlays use — payloads into
//! `init`, palette-aware rendering (draw ops or legacy text lines),
//! Lua-driven key/scroll handling, and outcomes up to and including
//! `Complete(CommandOutput)` and typed-payload responses.
//!
//! Overlay *state* lives in the Lua VM, keyed by a host-generated instance
//! id; the Rust-side [`OverlayState`] box holds only the id, the channel,
//! and (for typed payloads like the model picker) the [`OverlayResponder`],
//! whose reply-on-drop contract survives unchanged — a Lua picker dismissed
//! without a `respond` still answers `Ok(None)`.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::SyncSender;
use std::time::Duration;

use phi_ext::{
    DrawContext, DrawRect, HelpEntry, HelpPayload, ModelPickerPayload, OverlayKey, OverlayKeyCode,
    OverlayOutcome, OverlayPalette, OverlayPayload, OverlayResponder, OverlaySpec, OverlayState,
    Rgb,
};
use serde_json::{Value, json};

use super::convert::parse_command_output;
use super::thread::LuaMsg;

/// Round-trip budget for one overlay call into the Lua thread. Render runs
/// per frame, so this only bounds a wedged VM, it doesn't pace anything.
const OVERLAY_CALL_TIMEOUT: Duration = Duration::from_secs(5);

static NEXT_INSTANCE: AtomicU64 = AtomicU64::new(1);

/// Which optional Lua functions the overlay registration defined, read once
/// at register time so the spec can fall back where Lua stays silent.
pub(super) struct LuaOverlayFns {
    pub(super) has_handle_key: bool,
    pub(super) has_handle_scroll: bool,
}

/// The Rust-side state box for an open Lua overlay instance.
struct LuaOverlayInstance {
    instance: u64,
    tx: Arc<SyncSender<LuaMsg>>,
    /// Response channel extracted from a typed open payload (model picker).
    /// Fired by the `{ respond = ... }` outcome; drop-fires `Ok(None)`.
    responder: Option<OverlayResponder>,
}

impl Drop for LuaOverlayInstance {
    fn drop(&mut self) {
        // Free the Lua-side state; best-effort (the VM thread may be gone).
        let _ = self.tx.send(LuaMsg::OverlayDrop {
            instance: self.instance,
        });
    }
}

/// One synchronous round-trip into the Lua thread.
fn lua_call(
    tx: &SyncSender<LuaMsg>,
    build: impl FnOnce(std::sync::mpsc::SyncSender<anyhow::Result<Option<String>>>) -> LuaMsg,
) -> Option<String> {
    let (rtx, rrx) = std::sync::mpsc::sync_channel(1);
    if tx.send(build(rtx)).is_err() {
        return None;
    }
    match rrx.recv_timeout(OVERLAY_CALL_TIMEOUT) {
        Ok(Ok(json)) => json,
        _ => None,
    }
}

/// Convert the host's open payload for the Lua side: generic JSON payloads
/// pass through; the typed model-picker/help payloads become tagged tables
/// (their response channel stays host-side). Unknown payload types are
/// dropped — the overlay opens as if payload-less.
fn payload_to_json(payload: Option<OverlayPayload>) -> (Option<String>, Option<OverlayResponder>) {
    let Some(payload) = payload else {
        return (None, None);
    };
    let payload = match payload.downcast::<Value>() {
        Ok(value) => return (Some(value.to_string()), None),
        Err(other) => other,
    };
    let payload = match payload.downcast::<ModelPickerPayload>() {
        Ok(picker) => {
            let json = json!({
                "kind": "model",
                "root": picker.root,
                "current": picker.current,
                "models": picker.models,
            });
            return (Some(json.to_string()), Some(picker.responder));
        }
        Err(other) => other,
    };
    match payload.downcast::<HelpPayload>() {
        Ok(help) => {
            let entries: Vec<Value> = help
                .entries
                .iter()
                .map(|entry| match entry {
                    HelpEntry::Heading(heading) => json!({ "heading": heading }),
                    HelpEntry::Item { keys, description } => {
                        json!({ "keys": keys, "description": description })
                    }
                    HelpEntry::Blank => json!({}),
                })
                .collect();
            (
                Some(json!({ "kind": "help", "entries": entries }).to_string()),
                None,
            )
        }
        Err(_) => (None, None),
    }
}

/// Serialize an [`OverlayKey`] for the Lua side: named keys use `code`
/// directly; characters use `code = "char"` + `char` so `"g"` the key name
/// space stays unambiguous.
fn key_to_json(key: &OverlayKey) -> String {
    let code = match key.code {
        OverlayKeyCode::Char(_) => "char".to_owned(),
        OverlayKeyCode::Enter => "enter".to_owned(),
        OverlayKeyCode::Esc => "esc".to_owned(),
        OverlayKeyCode::Backspace => "backspace".to_owned(),
        OverlayKeyCode::Tab => "tab".to_owned(),
        OverlayKeyCode::BackTab => "backtab".to_owned(),
        OverlayKeyCode::Up => "up".to_owned(),
        OverlayKeyCode::Down => "down".to_owned(),
        OverlayKeyCode::Left => "left".to_owned(),
        OverlayKeyCode::Right => "right".to_owned(),
        OverlayKeyCode::Home => "home".to_owned(),
        OverlayKeyCode::End => "end".to_owned(),
        OverlayKeyCode::PageUp => "pageup".to_owned(),
        OverlayKeyCode::PageDown => "pagedown".to_owned(),
        OverlayKeyCode::Delete => "delete".to_owned(),
        OverlayKeyCode::F(n) => format!("f{n}"),
        OverlayKeyCode::Other => "other".to_owned(),
    };
    let mut obj = json!({
        "code": code,
        "ctrl": key.modifiers.control,
        "shift": key.modifiers.shift,
        "alt": key.modifiers.alt,
    });
    if let OverlayKeyCode::Char(c) = key.code {
        obj["char"] = Value::String(c.to_string());
    }
    obj.to_string()
}

fn palette_json(palette: &OverlayPalette) -> Value {
    fn rgb(c: Rgb) -> Value {
        json!([c.0, c.1, c.2])
    }
    json!({
        "fg": rgb(palette.fg),
        "bg": rgb(palette.bg),
        "muted": rgb(palette.muted),
        "border": rgb(palette.border),
        "accent": rgb(palette.accent),
        "accent_2": rgb(palette.accent_2),
        "surface_raised": rgb(palette.surface_raised),
        "statusbar_bg": rgb(palette.statusbar_bg),
    })
}

/// Decode a colour value from a draw op: an `[r, g, b]` array or a palette
/// role name (`"accent"`).
fn color(value: &Value, palette: &OverlayPalette) -> Option<Rgb> {
    match value {
        Value::Array(parts) if parts.len() == 3 => {
            let channel = |i: usize| parts[i].as_u64().map(|v| v.min(255) as u8);
            Some(Rgb(channel(0)?, channel(1)?, channel(2)?))
        }
        Value::String(role) => Some(match role.as_str() {
            "fg" => palette.fg,
            "bg" => palette.bg,
            "muted" => palette.muted,
            "border" => palette.border,
            "accent" => palette.accent,
            "accent_2" => palette.accent_2,
            "surface_raised" => palette.surface_raised,
            "statusbar_bg" => palette.statusbar_bg,
            _ => return None,
        }),
        _ => None,
    }
}

fn int(value: &Value) -> i32 {
    value.as_i64().unwrap_or(0) as i32
}

/// Replay Lua draw ops onto the host's [`DrawContext`]. Missing colours
/// default to the palette's fg/bg, so `{ op = "text", col, row, text }` is
/// a complete op.
fn replay_draw_ops(ctx: &mut dyn DrawContext, ops: &[Value], palette: &OverlayPalette) {
    for op in ops {
        let fg = op.get("fg").and_then(|v| color(v, palette));
        let bg = op.get("bg").and_then(|v| color(v, palette));
        match op.get("op").and_then(Value::as_str) {
            Some("text") => {
                let text = op.get("text").and_then(Value::as_str).unwrap_or("");
                let col = int(op.get("col").unwrap_or(&Value::Null));
                let row = int(op.get("row").unwrap_or(&Value::Null));
                let max_cols = op
                    .get("max_cols")
                    .map(int)
                    .unwrap_or_else(|| ctx.size().cols - col);
                let fg = fg.unwrap_or(palette.fg);
                let bg = bg.unwrap_or(palette.bg);
                if op.get("bold").and_then(Value::as_bool).unwrap_or(false) {
                    ctx.put_text_bold(col, row, max_cols, text, fg, bg);
                } else {
                    ctx.put_text(col, row, max_cols, text, fg, bg);
                }
            }
            Some("fill") => {
                let rect = DrawRect::new(
                    int(op.get("col").unwrap_or(&Value::Null)),
                    int(op.get("row").unwrap_or(&Value::Null)),
                    int(op.get("cols").unwrap_or(&Value::Null)),
                    int(op.get("rows").unwrap_or(&Value::Null)),
                );
                ctx.fill_rect(rect, fg.unwrap_or(palette.fg), bg.unwrap_or(palette.bg));
            }
            Some("box") => {
                let rect = DrawRect::new(
                    int(op.get("col").unwrap_or(&Value::Null)),
                    int(op.get("row").unwrap_or(&Value::Null)),
                    int(op.get("cols").unwrap_or(&Value::Null)),
                    int(op.get("rows").unwrap_or(&Value::Null)),
                );
                let border = op
                    .get("border")
                    .and_then(|v| color(v, palette))
                    .unwrap_or(palette.border);
                ctx.draw_box(
                    rect,
                    fg.unwrap_or(palette.fg),
                    bg.unwrap_or(palette.surface_raised),
                    border,
                );
            }
            Some("clear") => ctx.clear(bg.unwrap_or(palette.bg)),
            Some("scrollbar") => {
                let get = |k: &str| int(op.get(k).unwrap_or(&Value::Null));
                let getu = |k: &str| get(k).max(0) as usize;
                ctx.render_scrollbar(
                    get("col"),
                    get("row"),
                    get("rows"),
                    getu("visible"),
                    getu("total"),
                    getu("scroll"),
                    fg.unwrap_or(palette.border),
                    bg.unwrap_or(palette.bg),
                    "╎",
                    op.get("thumb_fg")
                        .and_then(|v| color(v, palette))
                        .unwrap_or(palette.accent_2),
                    "┃",
                )
            }
            _ => {}
        }
    }
}

/// Draw legacy line-mode output: a centered bordered box of plain text lines
/// (the pre-WS1.6 fixed renderer, now themed by the host palette).
fn draw_legacy_lines(ctx: &mut dyn DrawContext, lines: &[String], palette: &OverlayPalette) {
    let size = ctx.size();
    // borders + title + blank + footer around the lines (the pre-WS1.6
    // renderer budgeted one row short and clipped the last line).
    let line_count = lines.len() as i32 + 5;
    let width = size.cols.clamp(20, 80);
    let height = size.rows.min(line_count).max(6);
    let rect = DrawRect::new(
        (size.cols - width) / 2,
        (size.rows - height) / 2,
        width,
        height,
    );

    ctx.draw_box(rect, palette.fg, palette.surface_raised, palette.border);
    let inner = rect.inset(2, 1);
    ctx.put_text_bold(
        inner.col,
        inner.row,
        inner.cols,
        " Overlay ",
        palette.accent,
        palette.surface_raised,
    );
    for (i, line) in lines.iter().enumerate() {
        let y = inner.row + 2 + i as i32;
        if y < rect.row + rect.rows - 2 {
            ctx.put_text(
                inner.col,
                y,
                inner.cols,
                line,
                palette.fg,
                palette.surface_raised,
            );
        }
    }
    ctx.put_text(
        inner.col,
        rect.row + rect.rows - 2,
        inner.cols,
        "Esc/Enter/q to close",
        palette.muted,
        palette.surface_raised,
    );
}

/// Decode a Lua key outcome. `respond` fires the typed payload's response
/// channel and closes; unknown shapes are ignored (key consumed, no change)
/// so an author typo can't wedge or spam the host.
fn decode_outcome(
    json: Option<String>,
    responder: &mut Option<OverlayResponder>,
) -> OverlayOutcome {
    let Some(json) = json else {
        return OverlayOutcome::None;
    };
    let Ok(value) = serde_json::from_str::<Value>(&json) else {
        return OverlayOutcome::None;
    };
    match &value {
        Value::String(s) => match s.as_str() {
            "close" => OverlayOutcome::Close,
            _ => OverlayOutcome::None,
        },
        Value::Object(obj) => {
            if let Some(submit) = obj.get("submit").and_then(Value::as_str) {
                return OverlayOutcome::Submit(submit.to_owned());
            }
            if let Some(respond) = obj.get("respond").and_then(Value::as_str) {
                if let Some(responder) = responder.as_mut() {
                    responder.respond(Ok(Some(respond.to_owned())));
                }
                return OverlayOutcome::Close;
            }
            if obj.contains_key("message") || obj.contains_key("actions") {
                return match parse_command_output(&json) {
                    Ok(output) => OverlayOutcome::Complete(output),
                    Err(_) => OverlayOutcome::None,
                };
            }
            OverlayOutcome::None
        }
        _ => OverlayOutcome::None,
    }
}

/// Build the [`OverlaySpec`] for one Lua-registered overlay.
pub(super) fn build_overlay_spec(
    name: String,
    description: String,
    toggle_key: String,
    fns: LuaOverlayFns,
    tx: Arc<SyncSender<LuaMsg>>,
) -> OverlaySpec {
    let init_state: phi_ext::OverlayInitFn = {
        let tx = Arc::clone(&tx);
        let name = name.clone();
        Arc::new(move |payload| {
            let instance = NEXT_INSTANCE.fetch_add(1, Ordering::Relaxed);
            let (payload_json, responder) = payload_to_json(payload);
            let _ = lua_call(&tx, |reply| LuaMsg::OverlayInit {
                name: name.clone(),
                instance,
                payload_json,
                reply,
            });
            Box::new(LuaOverlayInstance {
                instance,
                tx: Arc::clone(&tx),
                responder,
            }) as OverlayState
        })
    };

    let render: phi_ext::OverlayRenderFn = {
        let tx = Arc::clone(&tx);
        let name = name.clone();
        Arc::new(move |ctx: &mut dyn DrawContext, state: &mut OverlayState| {
            let Some(inst) = state.downcast_ref::<LuaOverlayInstance>() else {
                return;
            };
            let size = ctx.size();
            let palette = ctx.palette();
            let ctx_json = json!({
                "cols": size.cols,
                "rows": size.rows,
                "palette": palette_json(&palette),
            })
            .to_string();
            let reply = lua_call(&tx, |reply| LuaMsg::RenderOverlay {
                name: name.clone(),
                instance: inst.instance,
                ctx_json,
                reply,
            });
            let Some(reply) = reply else {
                draw_legacy_lines(ctx, &["(render failed)".to_owned()], &palette);
                return;
            };
            let Ok(value) = serde_json::from_str::<Value>(&reply) else {
                return;
            };
            // `{ ops = {...} }`, a bare op array, or legacy plain lines.
            let ops = value.get("ops").and_then(Value::as_array).cloned();
            let items = ops
                .or_else(|| value.as_array().cloned())
                .unwrap_or_default();
            if items.iter().all(Value::is_string) {
                let lines: Vec<String> = items
                    .iter()
                    .filter_map(|v| v.as_str().map(str::to_owned))
                    .collect();
                draw_legacy_lines(ctx, &lines, &palette);
            } else {
                replay_draw_ops(ctx, &items, &palette);
            }
        })
    };

    let handle_key: phi_ext::OverlayKeyFn = if fns.has_handle_key {
        let tx = Arc::clone(&tx);
        let name = name.clone();
        Arc::new(
            move |state: &mut OverlayState, key: &OverlayKey, cols, rows| {
                let Some(inst) = state.downcast_mut::<LuaOverlayInstance>() else {
                    return OverlayOutcome::Close;
                };
                let reply = lua_call(&tx, |reply| LuaMsg::OverlayKey {
                    name: name.clone(),
                    instance: inst.instance,
                    key_json: key_to_json(key),
                    cols,
                    rows,
                    reply,
                });
                decode_outcome(reply, &mut inst.responder)
            },
        )
    } else {
        // No Lua handler: the pre-WS1.6 close-keys behavior.
        Arc::new(
            |_state: &mut OverlayState, key: &OverlayKey, _cols, _rows| match key.code {
                OverlayKeyCode::Esc | OverlayKeyCode::Enter => OverlayOutcome::Close,
                OverlayKeyCode::Char('q') | OverlayKeyCode::Char('Q') if !key.modifiers.control => {
                    OverlayOutcome::Close
                }
                _ => OverlayOutcome::None,
            },
        )
    };

    let mut spec = OverlaySpec::new(name.clone(), description, init_state, render, handle_key)
        .with_toggle_key(toggle_key);
    if fns.has_handle_scroll {
        let tx = Arc::clone(&tx);
        spec = spec.with_scroll(Arc::new(
            move |state: &mut OverlayState, delta, cols, rows| {
                let Some(inst) = state.downcast_ref::<LuaOverlayInstance>() else {
                    return;
                };
                let _ = lua_call(&tx, |reply| LuaMsg::OverlayScroll {
                    name: name.clone(),
                    instance: inst.instance,
                    delta,
                    cols,
                    rows,
                    reply,
                });
            },
        ));
    }
    spec
}
