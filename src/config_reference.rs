use std::collections::BTreeMap;

use facet::{Def, Facet, Shape, Type, UserType};
use serde_json::{Map, Value};

use crate::config::AppConfig;

pub fn generate_config_reference() -> Value {
    let mut entries = BTreeMap::new();
    collect_paths(AppConfig::SHAPE, &mut Vec::new(), &mut entries);
    for (key, default, environment, example) in [
        (
            "enabled",
            serde_json::json!(false),
            "CODE_INTERPRETER_MCP_PROMETHEUS_ENABLED",
            serde_json::json!(true),
        ),
        (
            "host",
            serde_json::json!("127.0.0.1"),
            "CODE_INTERPRETER_MCP_PROMETHEUS_HOST",
            serde_json::json!("0.0.0.0"),
        ),
        (
            "port",
            serde_json::json!(3131),
            "CODE_INTERPRETER_MCP_PROMETHEUS_PORT",
            serde_json::json!(3131),
        ),
    ] {
        entries.insert(
            format!("integrations.prometheus.{key}"),
            serde_json::json!({"default": default, "environment": environment, "example": example}),
        );
    }
    serde_json::to_value(entries).expect("config reference is serializable")
}

fn collect_paths(
    shape: &'static Shape,
    path: &mut Vec<String>,
    entries: &mut BTreeMap<String, Value>,
) {
    match &shape.def {
        Def::Option(option) => collect_paths(option.t, path, entries),
        Def::List(list) => {
            path.push("[]".to_owned());
            collect_paths(list.t, path, entries);
            path.pop();
        }
        Def::Array(array) => {
            path.push("[]".to_owned());
            collect_paths(array.t, path, entries);
            path.pop();
        }
        Def::Map(map) => {
            path.push("<key>".to_owned());
            collect_paths(map.v, path, entries);
            path.pop();
        }
        _ => match shape.ty {
            Type::User(UserType::Struct(struct_type)) => {
                for field in struct_type.fields {
                    path.push(field.rename.unwrap_or(field.name).to_owned());
                    collect_paths(field.shape.get(), path, entries);
                    path.pop();
                }
            }
            _ if !path.is_empty() => {
                entries.insert(path.join("."), Value::Object(Map::new()));
            }
            _ => {}
        },
    }
}
