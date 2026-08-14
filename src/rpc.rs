use std::{collections::HashMap, path::Path};

use anyhow::{anyhow, bail};
use cln_plugin::Plugin;
use serde_json::{Map, Value, json};
use tokio::fs;

use crate::{
    CLNADDRESS_USERS_FILENAME, PluginState,
    structs::{UserMetadata, validate_user},
};

const USER_ADD_FIELDS: &[&str] = &[
    "user",
    "is_email",
    "description",
    "min_sendable_msat",
    "max_sendable_msat",
    "comment_allowed",
    "nostr_enabled",
];

pub async fn user_add(
    plugin: Plugin<PluginState>,
    args: serde_json::Value,
) -> Result<serde_json::Value, anyhow::Error> {
    let (user, metadata) = parse_user_add_args(&args)?;
    metadata.validate(
        plugin.state().min_sendable_msat,
        plugin.state().max_sendable_msat,
    )?;

    let result;
    let users_clone;
    {
        let mut users = plugin.state().users.lock();
        result = users.insert(user.clone(), metadata.clone());
        users_clone = users.clone();
    }
    save_users(&plugin.state().plugin_dir, users_clone).await?;
    let mut mode = if result.is_some() {
        json!({"mode":"updated"})
    } else {
        json!({"mode":"added"})
    };

    mode.as_object_mut()
        .unwrap()
        .extend(json!({"user":user}).as_object().unwrap().clone());
    mode.as_object_mut()
        .unwrap()
        .extend(json!(metadata).as_object().unwrap().clone());

    Ok(mode)
}

pub fn parse_user_add_args(
    args: &serde_json::Value,
) -> Result<(String, UserMetadata), anyhow::Error> {
    let (user, metadata) = match args {
        Value::String(s) => (s.clone(), UserMetadata::default()),
        Value::Number(n) => (n.to_string(), UserMetadata::default()),
        Value::Array(values) => parse_legacy_user_array(values)?,
        Value::Object(map) => parse_user_object(map)?,
        _ => return Err(anyhow!("Not a valid input type")),
    };

    validate_user(&user)?;
    Ok((user, metadata))
}

fn parse_legacy_user_array(values: &[Value]) -> Result<(String, UserMetadata), anyhow::Error> {
    if values.is_empty() {
        return Err(anyhow!("Empty array input"));
    }
    if values.len() > 3 {
        bail!(
            "positional clnaddress-adduser supports only `user [is_email] [description]`; use named parameters for per-address limits, comments, or Nostr settings"
        );
    }

    let user = value_to_string(&values[0], "user")?;
    let is_email = values
        .get(1)
        .map(|value| value_to_bool(value, "is_email"))
        .transpose()?;
    let description = values
        .get(2)
        .map(|value| value_to_string(value, "description"))
        .transpose()?;

    Ok((
        user,
        UserMetadata {
            is_email,
            description,
            ..UserMetadata::default()
        },
    ))
}

fn parse_user_object(map: &Map<String, Value>) -> Result<(String, UserMetadata), anyhow::Error> {
    for key in map.keys() {
        if !USER_ADD_FIELDS.contains(&key.as_str()) {
            bail!("unknown clnaddress-adduser field `{key}`");
        }
    }

    let user = value_to_string(
        map.get("user")
            .ok_or_else(|| anyhow!("`user` field not found in object"))?,
        "user",
    )?;

    Ok((
        user,
        UserMetadata {
            is_email: optional_bool(map, "is_email")?,
            description: optional_string(map, "description")?,
            min_sendable_msat: optional_u64(map, "min_sendable_msat")?,
            max_sendable_msat: optional_u64(map, "max_sendable_msat")?,
            comment_allowed: optional_u64(map, "comment_allowed")?,
            nostr_enabled: optional_bool(map, "nostr_enabled")?,
        },
    ))
}

fn optional_bool(map: &Map<String, Value>, field: &str) -> Result<Option<bool>, anyhow::Error> {
    map.get(field)
        .map(|value| value_to_bool(value, field))
        .transpose()
}

fn optional_string(
    map: &Map<String, Value>,
    field: &str,
) -> Result<Option<String>, anyhow::Error> {
    map.get(field)
        .map(|value| value_to_string(value, field))
        .transpose()
}

fn optional_u64(map: &Map<String, Value>, field: &str) -> Result<Option<u64>, anyhow::Error> {
    map.get(field)
        .map(|value| value_to_u64(value, field))
        .transpose()
}

fn value_to_bool(value: &Value, field: &str) -> Result<bool, anyhow::Error> {
    match value {
        Value::Bool(value) => Ok(*value),
        Value::String(value) => value
            .parse::<bool>()
            .map_err(|_| anyhow!("`{field}` must be true or false")),
        _ => Err(anyhow!("`{field}` has invalid type")),
    }
}

fn value_to_string(value: &Value, field: &str) -> Result<String, anyhow::Error> {
    match value {
        Value::String(value) => Ok(value.clone()),
        Value::Number(value) => Ok(value.to_string()),
        _ => Err(anyhow!("`{field}` has invalid type")),
    }
}

fn value_to_u64(value: &Value, field: &str) -> Result<u64, anyhow::Error> {
    match value {
        Value::Number(value) => value
            .as_u64()
            .ok_or_else(|| anyhow!("`{field}` must be a non-negative integer")),
        Value::String(value) => value
            .parse::<u64>()
            .map_err(|_| anyhow!("`{field}` must be a non-negative integer")),
        _ => Err(anyhow!("`{field}` has invalid type")),
    }
}

pub async fn user_del(
    plugin: Plugin<PluginState>,
    args: serde_json::Value,
) -> Result<serde_json::Value, anyhow::Error> {
    let user = parse_required_user_selector(args)?;
    let result;
    let users_clone;
    {
        let mut users = plugin.state().users.lock();
        result = users.remove(&user);
        users_clone = users.clone();
    }
    if let Some(res) = result {
        save_users(&plugin.state().plugin_dir, users_clone).await?;
        let mut mode = json!({"mode":"deleted"});

        mode.as_object_mut()
            .unwrap()
            .extend(json!({"user":user}).as_object().unwrap().clone());
        mode.as_object_mut()
            .unwrap()
            .extend(json!(res).as_object().unwrap().clone());

        Ok(mode)
    } else {
        Err(anyhow!("User not found"))
    }
}

pub async fn user_list(
    plugin: Plugin<PluginState>,
    args: serde_json::Value,
) -> Result<serde_json::Value, anyhow::Error> {
    let mut users = plugin.state().users.lock().clone();
    let user = parse_optional_user_selector(args)?;

    if let Some(user) = user {
        users.retain(|candidate, _| candidate == &user);
        if users.is_empty() {
            return Err(anyhow!("User `{user}` not found!"));
        }
    }

    let array: Vec<serde_json::Value> = users
        .into_iter()
        .map(|(key, data)| {
            let data_value = serde_json::to_value(&data).unwrap_or(Value::Null);
            if let Value::Object(mut map) = data_value {
                map.insert("user".to_string(), Value::String(key));
                Value::Object(map)
            } else {
                json!({"user": key})
            }
        })
        .collect();

    Ok(Value::Array(array))
}

fn parse_required_user_selector(args: Value) -> Result<String, anyhow::Error> {
    parse_optional_user_selector(args)?.ok_or_else(|| anyhow!("user is required"))
}

fn parse_optional_user_selector(args: Value) -> Result<Option<String>, anyhow::Error> {
    let user = match args {
        Value::String(value) => Some(value),
        Value::Number(value) => Some(value.to_string()),
        Value::Array(values) => {
            if values.is_empty() {
                None
            } else if values.len() == 1 {
                Some(value_to_string(&values[0], "user")?)
            } else {
                return Err(anyhow!("expected zero or one user argument"));
            }
        }
        Value::Object(map) => {
            for key in map.keys() {
                if key != "user" {
                    bail!("unknown field `{key}`");
                }
            }
            map.get("user")
                .map(|value| value_to_string(value, "user"))
                .transpose()?
        }
        Value::Null => None,
        _ => return Err(anyhow!("Not a valid input type")),
    };

    if let Some(user) = &user {
        validate_user(user)?;
    }
    Ok(user)
}

pub async fn save_users(
    path: &Path,
    users: HashMap<String, UserMetadata>,
) -> Result<(), anyhow::Error> {
    let serialized = serde_json::to_string(&users)?;
    fs::write(path.join(CLNADDRESS_USERS_FILENAME), serialized).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_positional_user_add_remains_compatible() {
        let (user, metadata) = parse_user_add_args(&json!([
            "herd",
            true,
            "Lightning Goats"
        ]))
        .unwrap();
        assert_eq!(user, "herd");
        assert_eq!(metadata.is_email, Some(true));
        assert_eq!(metadata.description.as_deref(), Some("Lightning Goats"));
        assert_eq!(metadata.comment_allowed, None);
    }

    #[test]
    fn named_user_add_parses_rich_settings() {
        let (user, metadata) = parse_user_add_args(&json!({
            "user": "herd",
            "description": "Feed the Lightning Goats",
            "min_sendable_msat": 1000,
            "max_sendable_msat": 10_000_000,
            "comment_allowed": 250,
            "nostr_enabled": true
        }))
        .unwrap();
        assert_eq!(user, "herd");
        assert_eq!(metadata.min_sendable_msat, Some(1000));
        assert_eq!(metadata.max_sendable_msat, Some(10_000_000));
        assert_eq!(metadata.comment_allowed, Some(250));
        assert_eq!(metadata.nostr_enabled, Some(true));
    }

    #[test]
    fn named_user_add_rejects_unknown_fields_and_noncanonical_users() {
        assert!(
            parse_user_add_args(&json!({"user":"herd","comment_allowd":250})).is_err()
        );
        assert!(parse_user_add_args(&json!({"user":"Herd"})).is_err());
    }

    #[test]
    fn positional_extensions_are_rejected_in_favor_of_named_args() {
        assert!(parse_user_add_args(&json!(["herd", true, "desc", 1000])).is_err());
    }
}
