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
        serde_json::Value::String(s) => Ok(mlua::Value::String(lua.create_string(s)?)),
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

/// Check if a Lua table is an array (sequential integer keys starting from 1).
fn is_array_table(t: &mlua::Table) -> Result<bool> {
    let len = t.raw_len();
    if len == 0 {
        return Ok(false);
    }
    // Verify all keys are integers 1..=len
    let mut count = 0;
    for pair in t.pairs::<mlua::Value, mlua::Value>() {
        let (k, _) = pair?;
        match k {
            mlua::Value::Integer(i) if i >= 1 && i as usize <= len => {
                count += 1;
            }
            _ => return Ok(false),
        }
    }
    Ok(count == len as usize)
}

pub(super) fn lua_value_to_json_string(val: mlua::Value) -> Result<Option<String>> {
    match val {
        mlua::Value::Nil => Ok(None),
        mlua::Value::Table(t) => {
            let json_val = lua_val_to_json(mlua::Value::Table(t))?;
            match json_val {
                serde_json::Value::Null => Ok(None),
                other => Ok(Some(serde_json::to_string(&other)?)),
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
            // Check if this is an array-style table (sequential integer keys)
            if is_array_table(&t)? {
                let mut arr = Vec::new();
                for i in 1..=t.raw_len() {
                    let v: mlua::Value = t.get(i)?;
                    arr.push(lua_val_to_json(v)?);
                }
                return Ok(serde_json::Value::Array(arr));
            }
            // Otherwise treat as a map
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
