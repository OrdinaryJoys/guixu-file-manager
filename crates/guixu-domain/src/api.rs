use serde::{Deserialize, Serialize};

pub const API_VERSION: u32 = 1;

/// 桌面壳和核心之间的最小启动契约。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeInfo {
    pub api_version: u32,
    pub name: String,
    pub version: String,
    pub offline: bool,
}

impl RuntimeInfo {
    pub fn current() -> Self {
        Self {
            api_version: API_VERSION,
            name: "归序".to_owned(),
            version: std::option_env!("GUIXU_VERSION")
                .unwrap_or(env!("CARGO_PKG_VERSION"))
                .to_owned(),
            offline: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum CommandResult<T> {
    Ok { data: T },
    Error { code: String, message: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_contract_is_versioned_and_offline() {
        let runtime = RuntimeInfo::current();
        assert_eq!(runtime.api_version, 1);
        assert_eq!(runtime.name, "归序");
        assert!(runtime.offline);
    }

    #[test]
    fn error_contract_has_stable_tagged_shape() {
        let value: CommandResult<()> = CommandResult::Error {
            code: "conflict".to_owned(),
            message: "目标已存在".to_owned(),
        };
        let json = serde_json::to_value(value).expect("serialize command result");
        assert_eq!(json["status"], "error");
        assert_eq!(json["code"], "conflict");
    }
}
