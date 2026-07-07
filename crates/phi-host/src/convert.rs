//! JSON ⇄ Lua value conversion. JSON is the boundary format for everything
//! crossing the bridge (DESIGN.md read/write path: plain tables in, plain
//! tables out — no live host references).

pub(crate) fn json_to_lua(lua: &mlua::Lua, val: &serde_json::Value) -> mlua::Result<mlua::Value> {
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

/// A table with sequential integer keys 1..=len becomes a JSON array;
/// everything else becomes an object. An empty table is an object.
fn is_array_table(t: &mlua::Table) -> mlua::Result<bool> {
    let len = t.raw_len();
    if len == 0 {
        return Ok(false);
    }
    let mut count = 0;
    for pair in t.pairs::<mlua::Value, mlua::Value>() {
        let (k, _) = pair?;
        match k {
            mlua::Value::Integer(i) if i >= 1 && (i as usize) <= len => count += 1,
            _ => return Ok(false),
        }
    }
    Ok(count == len)
}

pub(crate) fn lua_to_json(val: mlua::Value) -> mlua::Result<serde_json::Value> {
    match val {
        mlua::Value::Nil => Ok(serde_json::Value::Null),
        mlua::Value::Boolean(b) => Ok(serde_json::Value::Bool(b)),
        mlua::Value::Integer(i) => Ok(serde_json::Value::Number(i.into())),
        mlua::Value::Number(f) => Ok(serde_json::Value::Number(
            serde_json::Number::from_f64(f).unwrap_or_else(|| serde_json::Number::from(0)),
        )),
        mlua::Value::String(s) => Ok(serde_json::Value::String(s.to_str()?.to_owned())),
        mlua::Value::Table(t) => {
            if is_array_table(&t)? {
                let mut arr = Vec::with_capacity(t.raw_len());
                for i in 1..=t.raw_len() {
                    arr.push(lua_to_json(t.get(i)?)?);
                }
                return Ok(serde_json::Value::Array(arr));
            }
            let mut map = serde_json::Map::new();
            for pair in t.pairs::<mlua::Value, mlua::Value>() {
                let (k, v) = pair?;
                let key = match k {
                    mlua::Value::String(s) => s.to_str()?.to_owned(),
                    mlua::Value::Integer(i) => i.to_string(),
                    _ => continue,
                };
                map.insert(key, lua_to_json(v)?);
            }
            Ok(serde_json::Value::Object(map))
        }
        // Functions, userdata, threads: not representable at the boundary.
        _ => Ok(serde_json::Value::Null),
    }
}
