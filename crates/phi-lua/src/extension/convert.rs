use anyhow::Result;

pub(super) fn json_to_lua(lua: &mlua::Lua, val: &serde_json::Value) -> mlua::Result<mlua::Value> {
    match val {
        serde_json::Value::Object(map) => {
            let table = lua.create_table()?;
            for (k, v) in map {
                table.set(k.as_str(), json_to_lua(lua, v)?)?;
            }
            Ok(mlua::Value::Table(table))
        }
        serde_json::Value::Array(arr) => {
            let table = lua.create_table()?;
            for (i, v) in arr.iter().enumerate() {
                table.set(i + 1, json_to_lua(lua, v)?)?;
            }
            Ok(mlua::Value::Table(table))
        }
        serde_json::Value::String(s) => {
            Ok(mlua::Value::String(lua.create_string(s)?))
        }
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Ok(mlua::Value::Integer(i))
            } else {
                Ok(mlua::Value::Number(n.as_f64().unwrap_or(0.0)))
            }
        }
        serde_json::Value::Bool(b) => Ok(mlua::Value::Boolean(*b)),
        serde_json::Value::Null => Ok(mlua::Value::Nil),
    }
}

pub(super) fn lua_value_to_json_string(val: mlua::Value) -> Result<Option<String>> {
    match val {
        mlua::Value::Nil => Ok(None),
        mlua::Value::Table(t) => {
            let mut map = serde_json::Map::new();
            for pair in t.pairs::<mlua::Value, mlua::Value>() {
                let (k, v) = pair?;
                let key = match k {
                    mlua::Value::String(s) => s.to_str()?.to_owned(),
                    mlua::Value::Integer(i) => i.to_string(),
                    _ => continue,
                };
                let json_val = lua_val_to_json(v)?;
                map.insert(key, json_val);
            }
            if map.is_empty() {
                Ok(None)
            } else {
                Ok(Some(serde_json::to_string(&serde_json::Value::Object(map))?))
            }
        }
        _ => Ok(None),
    }
}

pub(super) fn lua_val_to_json(val: mlua::Value) -> Result<serde_json::Value> {
    match val {
        mlua::Value::Nil => Ok(serde_json::Value::Null),
        mlua::Value::Boolean(b) => Ok(serde_json::Value::Bool(b)),
        mlua::Value::Integer(i) => Ok(serde_json::Value::Number(i.into())),
        mlua::Value::Number(f) => Ok(serde_json::Value::Number(
            serde_json::Number::from_f64(f).unwrap_or(serde_json::Number::from(0)),
        )),
        mlua::Value::String(s) => Ok(serde_json::Value::String(s.to_str()?.to_owned())),
        mlua::Value::Table(t) => {
            let mut map = serde_json::Map::new();
            for pair in t.pairs::<mlua::Value, mlua::Value>() {
                let (k, v) = pair?;
                let key = match k {
                    mlua::Value::String(s) => s.to_str()?.to_owned(),
                    mlua::Value::Integer(i) => i.to_string(),
                    _ => continue,
                };
                map.insert(key, lua_val_to_json(v)?);
            }
            Ok(serde_json::Value::Object(map))
        }
        _ => Ok(serde_json::Value::Null),
    }
}

// ---------------------------------------------------------------------------
// decode_event_return
// ---------------------------------------------------------------------------
