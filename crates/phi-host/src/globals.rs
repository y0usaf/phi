//! The `phi.*` globals — the canonical extension API. Registration state
//! lives Lua-side in a registry-held table (handlers stay Lua functions);
//! name/description metadata is mirrored into [`Meta`] so the host can
//! answer queries without a VM round-trip.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use crate::{CommandInfo, ProviderInfo, ProviderModelInfo, ToolInfo};

/// Key of the registration table in the Lua named registry (not a global,
/// so extension code can't clobber it by accident).
pub(crate) const REGISTRY_KEY: &str = "phi-host";

/// Host-side mirror of registration metadata.
#[derive(Debug, Default)]
pub(crate) struct Meta {
    pub commands: Vec<CommandInfo>,
    /// Tools in registration order — Lua-registered and native entries
    /// share this one list (doctrine 01: one registry, two handler homes).
    pub tools: Vec<ToolInfo>,
    /// Event name → number of subscribed handlers.
    pub events: BTreeMap<String, usize>,
    /// Providers in registration order — extension-registered model
    /// providers that graft onto the `phi_ai::Registry`.
    pub providers: Vec<ProviderInfo>,
}

/// The registration table: `{ commands = {}, tools = {}, events = {}, source = "" }`.
pub(crate) fn registry_table(lua: &mlua::Lua) -> mlua::Result<mlua::Table> {
    lua.named_registry_value::<mlua::Table>(REGISTRY_KEY)
}

/// Source key of the extension currently being loaded (set by the load
/// path so registrations are attributable).
pub(crate) fn current_source(lua: &mlua::Lua) -> String {
    registry_table(lua)
        .and_then(|t| t.get::<String>("source"))
        .unwrap_or_else(|_| "<unknown>".to_owned())
}

/// Create the registration table and install the `phi` global.
///
/// Surface (grown only alongside an exercising example — DESIGN.md):
/// - `phi.register_command{ name, handler, description? }`
/// - `phi.register_tool{ name, description, parameters?, handler }`
/// - `phi.register_provider(name, config)` — graft a custom model provider.
/// - `phi.on(event, fn)` — open string vocabulary, no closed enum.
/// - `phi.commands()` — snapshot of registered commands
///   (`{ name, description?, source }` each), registration order.
pub(crate) fn install(lua: &mlua::Lua, meta: Arc<Mutex<Meta>>) -> mlua::Result<()> {
    let registry = lua.create_table()?;
    registry.set("commands", lua.create_table()?)?;
    registry.set("tools", lua.create_table()?)?;
    registry.set("events", lua.create_table()?)?;
    registry.set("providers", lua.create_table()?)?;
    registry.set("source", "<host>")?;
    lua.set_named_registry_value(REGISTRY_KEY, registry)?;

    let phi = lua.create_table()?;

    let register_command = {
        let meta = Arc::clone(&meta);
        lua.create_function(move |lua, spec: mlua::Table| {
            let name: String = spec
                .get::<Option<String>>("name")?
                .filter(|n| !n.trim().is_empty())
                .ok_or_else(|| {
                    mlua::Error::runtime("register_command: 'name' must be a non-empty string")
                })?;
            let handler: mlua::Function = spec
                .get::<Option<mlua::Function>>("handler")?
                .ok_or_else(|| {
                    mlua::Error::runtime(format!(
                        "register_command('{name}'): 'handler' must be a function"
                    ))
                })?;
            let description: Option<String> = spec.get("description")?;
            let source = current_source(lua);

            let commands: mlua::Table = registry_table(lua)?.get("commands")?;
            if let Some(existing) = commands.get::<Option<mlua::Table>>(name.as_str())? {
                let prior: String = existing
                    .get("source")
                    .unwrap_or_else(|_| "<unknown>".to_owned());
                return Err(mlua::Error::runtime(format!(
                    "register_command('{name}'): already registered by {prior}"
                )));
            }
            let entry = lua.create_table()?;
            entry.set("handler", handler)?;
            entry.set("description", description.clone())?;
            entry.set("source", source.clone())?;
            commands.set(name.as_str(), entry)?;

            let mut meta = meta.lock().expect("meta lock");
            meta.commands.push(CommandInfo {
                name,
                description,
                source,
            });
            Ok(())
        })?
    };
    phi.set("register_command", register_command)?;

    let register_tool = {
        let meta = Arc::clone(&meta);
        lua.create_function(move |lua, spec: mlua::Table| {
            let name: String = spec
                .get::<Option<String>>("name")?
                .filter(|n| !n.trim().is_empty())
                .ok_or_else(|| {
                    mlua::Error::runtime("register_tool: 'name' must be a non-empty string")
                })?;
            let description: String = spec
                .get::<Option<String>>("description")?
                .filter(|d| !d.trim().is_empty())
                .ok_or_else(|| {
                    mlua::Error::runtime(format!(
                        "register_tool('{name}'): 'description' must be a non-empty string \
                         (the model reads it)"
                    ))
                })?;
            let handler: mlua::Function = spec
                .get::<Option<mlua::Function>>("handler")?
                .ok_or_else(|| {
                    mlua::Error::runtime(format!(
                        "register_tool('{name}'): 'handler' must be a function"
                    ))
                })?;
            // JSON Schema for the arguments, as a plain table. Defaults to
            // an object schema with no declared properties.
            let input_schema = match spec.get::<Option<mlua::Table>>("parameters")? {
                Some(t) => crate::convert::lua_to_json(mlua::Value::Table(t))?,
                None => serde_json::json!({ "type": "object", "properties": {} }),
            };
            let source = current_source(lua);

            let mut meta = meta.lock().expect("meta lock");
            if let Some(prior) = meta.tools.iter().find(|t| t.name == name) {
                return Err(mlua::Error::runtime(format!(
                    "register_tool('{name}'): already registered by {}",
                    prior.source
                )));
            }

            let tools: mlua::Table = registry_table(lua)?.get("tools")?;
            let entry = lua.create_table()?;
            entry.set("handler", handler)?;
            entry.set("source", source.clone())?;
            tools.set(name.as_str(), entry)?;

            meta.tools.push(ToolInfo {
                name,
                description,
                input_schema,
                source,
            });
            Ok(())
        })?
    };
    phi.set("register_tool", register_tool)?;

    // phi.register_provider(name, config)
    // config: { base_url = "...", api_key = "...", api = "...",
    //            models = { { id = "...", name = "...", reasoning = bool,
    //                         context_window = n, max_tokens = n }, ... } }
    // Registers a custom model provider. The host grafts its models onto
    // the `phi_ai::Registry` (via `Registry::extended`); an inline api_key
    // becomes a runtime-only auth override (never persisted).
    let register_provider = {
        let meta = Arc::clone(&meta);
        lua.create_function(move |lua, (name, config): (String, mlua::Table)| {
            let name = name.trim().to_owned();
            if name.is_empty() {
                return Err(mlua::Error::runtime(
                    "register_provider: 'name' must be a non-empty string",
                ));
            }
            let base_url: Option<String> = config.get("base_url").ok();
            let api_key: Option<String> = config.get("api_key").ok();
            let api: Option<String> = config.get("api").ok();
            let source = current_source(lua);

            // Collect models from the config table. Sequence iteration, not
            // `pairs`: catalog order is registration order (deterministic
            // listings in --list-models and /model).
            let mut models = Vec::new();
            if let Some(mt) = config.get::<Option<mlua::Table>>("models")? {
                for m in mt.sequence_values::<mlua::Table>() {
                    let m = m?;
                    let id: String = m.get("id").unwrap_or_default();
                    let mname: String = m.get("name").unwrap_or_default();
                    let reasoning: bool = m.get("reasoning").unwrap_or(false);
                    let context_window: u64 = m.get("context_window").unwrap_or(0);
                    let max_tokens: u64 = m.get("max_tokens").unwrap_or(0);
                    let input: Vec<String> = m.get("input").unwrap_or_default();
                    let cost = m
                        .get::<Option<mlua::Value>>("cost")?
                        .map(crate::convert::lua_to_json)
                        .transpose()?;
                    let headers: std::collections::BTreeMap<String, String> =
                        m.get("headers").unwrap_or_default();
                    let thinking_levels = parse_thinking_levels(&m)?;
                    let adaptive_thinking: bool = m.get("adaptive_thinking").unwrap_or(false);
                    let reasoning_control = parse_reasoning_control(&m)?;
                    models.push(ProviderModelInfo {
                        id,
                        name: mname,
                        reasoning,
                        context_window,
                        max_tokens,
                        input,
                        cost,
                        headers,
                        thinking_levels,
                        adaptive_thinking,
                        reasoning_control,
                    });
                }
            }

            // Duplicate provider names replace the prior registration, so
            // a re-load after editing an extension updates rather than errors.
            // (Contrast with tools/commands, where a duplicate is a load error.)
            let providers: mlua::Table = registry_table(lua)?.get("providers")?;
            providers.set(name.as_str(), config)?;
            let mut meta = meta.lock().expect("meta lock");
            if let Some(idx) = meta.providers.iter().position(|p| p.name == name) {
                meta.providers[idx] = ProviderInfo {
                    name: name.clone(),
                    base_url,
                    api_key,
                    api,
                    source,
                    models,
                };
            } else {
                meta.providers.push(ProviderInfo {
                    name,
                    base_url,
                    api_key,
                    api,
                    source,
                    models,
                });
            }
            Ok(())
        })?
    };
    phi.set("register_provider", register_provider)?;

    let on = {
        let meta = Arc::clone(&meta);
        lua.create_function(move |lua, (event, handler): (String, mlua::Function)| {
            if event.trim().is_empty() {
                return Err(mlua::Error::runtime(
                    "on: event name must be a non-empty string",
                ));
            }
            let events: mlua::Table = registry_table(lua)?.get("events")?;
            let list: mlua::Table = match events.get::<Option<mlua::Table>>(event.as_str())? {
                Some(list) => list,
                None => {
                    let list = lua.create_table()?;
                    events.set(event.as_str(), &list)?;
                    list
                }
            };
            let entry = lua.create_table()?;
            entry.set("fn", handler)?;
            entry.set("source", current_source(lua))?;
            list.push(entry)?;

            let mut meta = meta.lock().expect("meta lock");
            *meta.events.entry(event).or_insert(0) += 1;
            Ok(())
        })?
    };
    phi.set("on", on)?;

    let commands = {
        let meta = Arc::clone(&meta);
        lua.create_function(move |lua, ()| {
            let meta = meta.lock().expect("meta lock");
            let list = lua.create_table()?;
            for (i, cmd) in meta.commands.iter().enumerate() {
                let entry = lua.create_table()?;
                entry.set("name", cmd.name.as_str())?;
                entry.set("description", cmd.description.as_deref())?;
                entry.set("source", cmd.source.as_str())?;
                list.set(i + 1, entry)?;
            }
            Ok(list)
        })?
    };
    phi.set("commands", commands)?;

    lua.globals().set("phi", phi)?;
    Ok(())
}

/// Parse a model config's optional `thinking_levels` table (level name →
/// provider value, mirroring the builtin catalog in
/// `phi-app/src/builtins/providers.lua`) into a raw JSON map. Level names
/// are validated against the `--thinking` vocabulary so a typo fails at
/// load, not silently at request time.
fn parse_thinking_levels(m: &mlua::Table) -> mlua::Result<Option<serde_json::Value>> {
    let Some(tl) = m.get::<Option<mlua::Table>>("thinking_levels")? else {
        return Ok(None);
    };
    let mut map = serde_json::Map::new();
    for pair in tl.pairs::<String, mlua::Value>() {
        let (level, val) = pair?;
        validate_thinking_level(&level, "thinking_levels")?;
        map.insert(level, crate::convert::lua_to_json(val)?);
    }
    Ok(Some(serde_json::Value::Object(map)))
}

/// Reject unknown level names so a typo doesn't silently drop a level.
/// The vocabulary is the same set as `--thinking`.
fn validate_thinking_level(level: &str, context: &str) -> mlua::Result<()> {
    if !matches!(
        level,
        "off" | "minimal" | "low" | "medium" | "high" | "xhigh"
    ) {
        return Err(mlua::Error::runtime(format!(
            "register_provider: {context}: unknown thinking level '{level}'"
        )));
    }
    Ok(())
}

/// Parse a model config's optional `reasoning_control` table into the
/// raw JSON shape carried by `ProviderModelInfo`. The Lua form:
///
/// ```lua
/// reasoning_control = {
///   field = "chat_template_kwargs.enable_thinking",
///   values = { off = false, high = true },
/// }
/// ```
///
/// `field` is required; `values` defaults to an empty map (every level
/// keeps the model's upstream default — a no-op declaration). The raw JSON
/// is typed into `phi_ai::ReasoningControl` at graft time (`phi_app`), so
/// phi-host stays LM-agnostic.
fn parse_reasoning_control(m: &mlua::Table) -> mlua::Result<Option<serde_json::Value>> {
    let Some(rc) = m.get::<Option<mlua::Table>>("reasoning_control")? else {
        return Ok(None);
    };
    let field: String = rc.get("field")?;
    let mut values = serde_json::Map::new();
    if let Some(vt) = rc.get::<Option<mlua::Table>>("values")? {
        for pair in vt.pairs::<String, mlua::Value>() {
            let (level, val) = pair?;
            validate_thinking_level(&level, "reasoning_control.values")?;
            values.insert(level, crate::convert::lua_to_json(val)?);
        }
    }
    Ok(Some(
        serde_json::json!({ "field": field, "values": values }),
    ))
}
