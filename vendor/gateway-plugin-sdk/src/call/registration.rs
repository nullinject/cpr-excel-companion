//! 插件注册只回传规范化的能力声明

use serde::{Deserialize, Serialize};

use crate::Contributions;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Registration {
    #[serde(deserialize_with = "crate::capability::deserialize_contributions")]
    pub contributes: Contributions,
}
