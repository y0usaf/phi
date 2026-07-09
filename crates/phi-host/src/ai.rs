//! Provider-stream and cancellation mechanisms exposed to Lua (WS4.1).

use mlua::{AnyUserData, Function, Lua, Table, UserData, UserDataMethods, Value};
use phi_ai::protocols::SimpleStreamOptions;
use phi_ai::transport::AbortSignal;
use phi_ai_types::{
    AssistantContent, AssistantMessage, AssistantMessageEvent, AssistantRole, Context, Model,
    StopReason, TextContent, Usage, now_ms,
};

use crate::convert::{json_to_lua, lua_to_json};

#[derive(Clone, Debug)]
pub(crate) struct LuaAbortSignal(pub(crate) AbortSignal);

pub(crate) fn signal_userdata(lua: &Lua, signal: AbortSignal) -> mlua::Result<AnyUserData> {
    lua.create_userdata(LuaAbortSignal(signal))
}

impl UserData for LuaAbortSignal {
    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_method("is_aborted", |_, this, ()| Ok(this.0.is_aborted()));
        methods.add_method("abort", |_, this, ()| {
            this.0.abort();
            Ok(())
        });
        methods.add_async_method("wait", |_, this, ()| async move {
            this.0.aborted().await;
            Ok(())
        });
    }
}

pub(crate) fn install(lua: &Lua, pi: &Table) -> mlua::Result<()> {
    pi.set(
        "abort_signal",
        lua.create_function(|lua, ()| lua.create_userdata(LuaAbortSignal(AbortSignal::new())))?,
    )?;

    let ai = lua.create_table()?;
    ai.set(
        "stream_simple",
        lua.create_async_function(
            |lua, (model, context, options, on_event): (Value, Value, Option<Table>, Function)| async move {
                let model: Model = from_lua_json(model, "model")?;
                let context = context_from_lua(context)?;
                let options = stream_options(options)?;
                let signal = options.base.signal.clone();
                let stream = match phi_ai::registry::stream_simple(&model, &context, Some(options)) {
                    Ok(stream) => stream,
                    Err(error) => {
                        let message = failure_message(
                            &model,
                            if signal.as_ref().is_some_and(AbortSignal::is_aborted) {
                                StopReason::Aborted
                            } else {
                                StopReason::Error
                            },
                            error.to_string(),
                        );
                        let event = AssistantMessageEvent::Error {
                            reason: message.stop_reason,
                            error: message.clone(),
                        };
                        call_event(&lua, &on_event, &event).await?;
                        return to_lua_json(&lua, &message);
                    }
                };

                while let Some(event) = stream.next().await {
                    call_event(&lua, &on_event, &event).await?;
                }
                match stream.result().await {
                    Some(message) => to_lua_json(&lua, &message),
                    None => {
                        let message = failure_message(
                            &model,
                            StopReason::Error,
                            "event stream completed without a result".to_owned(),
                        );
                        to_lua_json(&lua, &message)
                    }
                }
            },
        )?,
    )?;
    pi.set("ai", ai)?;
    Ok(())
}

fn stream_options(options: Option<Table>) -> mlua::Result<SimpleStreamOptions> {
    let mut result = SimpleStreamOptions::default();
    let Some(options) = options else {
        return Ok(result);
    };
    result.base.api_key = options.get("apiKey")?;
    result.base.session_id = options.get("sessionId")?;
    result.base.max_retries = options.get("maxRetries")?;
    result.base.max_retry_delay_ms = options.get("maxRetryDelayMs")?;
    result.base.timeout_ms = options.get("timeoutMs")?;
    if let Some(signal) = options.get::<Option<AnyUserData>>("signal")? {
        result.base.signal = Some(signal.borrow::<LuaAbortSignal>()?.0.clone());
    }
    Ok(result)
}

fn context_from_lua(value: Value) -> mlua::Result<Context> {
    let mut json = lua_to_json(value).map_err(|error| mlua::Error::runtime(error.to_string()))?;
    // Lua has one empty-table value for both `{}` and `[]`; these Context
    // fields are arrays by contract, so normalize empty maps at this typed edge.
    if let Some(object) = json.as_object_mut() {
        for key in ["messages", "tools"] {
            if object
                .get(key)
                .is_some_and(|value| value.as_object().is_some_and(|map| map.is_empty()))
            {
                object.insert(key.to_owned(), serde_json::Value::Array(Vec::new()));
            }
        }
    }
    serde_json::from_value(json)
        .map_err(|error| mlua::Error::runtime(format!("invalid context: {error}")))
}

fn from_lua_json<T: serde::de::DeserializeOwned>(value: Value, label: &str) -> mlua::Result<T> {
    let json = lua_to_json(value).map_err(|error| mlua::Error::runtime(error.to_string()))?;
    serde_json::from_value(json)
        .map_err(|error| mlua::Error::runtime(format!("invalid {label}: {error}")))
}

fn to_lua_json<T: serde::Serialize>(lua: &Lua, value: &T) -> mlua::Result<Value> {
    let json =
        serde_json::to_value(value).map_err(|error| mlua::Error::runtime(error.to_string()))?;
    json_to_lua(lua, &json)
}

async fn call_event(
    lua: &Lua,
    callback: &Function,
    event: &AssistantMessageEvent,
) -> mlua::Result<()> {
    let value = to_lua_json(lua, event)?;
    callback.call_async::<()>(value).await
}

fn failure_message(model: &Model, reason: StopReason, error: String) -> AssistantMessage {
    AssistantMessage {
        role: AssistantRole::Assistant,
        content: vec![AssistantContent::Text(TextContent::new(""))],
        api: model.api.clone(),
        provider: model.provider.clone(),
        model: model.id.clone(),
        response_model: None,
        response_id: None,
        diagnostics: None,
        usage: Usage::default(),
        stop_reason: reason,
        error_message: Some(error),
        timestamp: now_ms(),
    }
}
