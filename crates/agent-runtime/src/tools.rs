mod session;
pub use session::Session;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{future::Future, pin::Pin, sync::Arc};

/// 发现目录只有轻量描述；完整 schema 和操作说明只在选中后读取。
#[derive(Clone, Deserialize, Serialize)]
pub struct Descriptor {
    /// 全局唯一且稳定的调用名称。
    pub name: String,
    /// 所属业务模块，用于筛选和路由。
    pub provider: String,
    /// 不包含参数 schema 的简短职责说明。
    pub description: String,
    /// read 或 write，只描述效果，不代表已取得执行授权。
    pub effect: String,
    /// 中英文别名与常见意图，服务端检索使用，不发送给模型。
    #[serde(default, skip_serializing)]
    pub keywords: String,
}

/// 固定工具宿主契约；加载只改变可见性，实际权限始终由宿主检查。
pub trait Host: Send + Sync {
    /// 该业务模块的详细说明，仅在有工具被加载时注入。
    fn instructions(&self) -> String;
    /// 服务端本地 schema 集合，不能整体发送给模型。
    fn definitions(&self) -> Vec<Value>;
    /// 轻量目录；兼容旧宿主，正式业务模块应覆写以独立维护目录。
    fn catalog(&self) -> Vec<Descriptor> {
        self.definitions()
            .iter()
            .filter_map(|definition| {
                let function = &definition["function"];
                Some(Descriptor {
                    name: function["name"].as_str()?.into(),
                    provider: "legacy".into(),
                    description: function["description"].as_str().unwrap_or("").into(),
                    effect: "write".into(),
                    keywords: String::new(),
                })
            })
            .collect()
    }
    /// 按名称延迟取 schema；模型不能注册新的实现。
    fn definition(&self, name: &str) -> Option<Value> {
        self.definitions()
            .into_iter()
            .find(|tool| tool["function"]["name"] == name)
    }
    /// 每轮重新生成已加载模块的说明，移除工具后旧说明也退出系统提示。
    fn instructions_for(&self, names: &[String]) -> String {
        if names.is_empty() {
            String::new()
        } else {
            self.instructions()
        }
    }
    /// 工具结果只来自本次真实执行，不能以发现或加载成功替代。
    fn execute<'a>(
        &'a self,
        name: &'a str,
        args: Value,
    ) -> Pin<Box<dyn Future<Output = Value> + Send + 'a>>;
}

/// 组合宿主按注册目录路由，新增业务模块不需要扩充中央名称分支。
pub struct Registry<'a> {
    /// 已经过当前身份筛选的业务模块。
    providers: Vec<Arc<dyn Host + 'a>>,
}
impl<'a> Registry<'a> {
    /// 重名和保留名称属于代码配置错误，启动本轮时立即拒绝。
    pub fn new(providers: Vec<Arc<dyn Host + 'a>>) -> Self {
        let mut names = std::collections::HashSet::new();
        for provider in &providers {
            for tool in provider.catalog() {
                assert!(
                    !matches!(
                        tool.name.as_str(),
                        "current_time" | "tools_search" | "tools_load"
                    ) && names.insert(tool.name),
                    "重复或保留工具名称"
                );
            }
        }
        Self { providers }
    }
    /// 路由只使用服务端注册元数据。
    fn provider(&self, name: &str) -> Option<&Arc<dyn Host + 'a>> {
        self.providers
            .iter()
            .find(|provider| provider.catalog().iter().any(|tool| tool.name == name))
    }
}
impl Host for Registry<'_> {
    fn instructions(&self) -> String {
        String::new()
    }
    fn definitions(&self) -> Vec<Value> {
        self.providers
            .iter()
            .flat_map(|p| p.definitions())
            .collect()
    }
    fn catalog(&self) -> Vec<Descriptor> {
        self.providers.iter().flat_map(|p| p.catalog()).collect()
    }
    fn definition(&self, name: &str) -> Option<Value> {
        self.provider(name)?.definition(name)
    }
    fn instructions_for(&self, names: &[String]) -> String {
        self.providers
            .iter()
            .filter_map(|provider| {
                let selected: Vec<_> = names
                    .iter()
                    .filter(|name| provider.catalog().iter().any(|tool| &tool.name == *name))
                    .cloned()
                    .collect();
                (!selected.is_empty()).then(|| provider.instructions_for(&selected))
            })
            .collect::<Vec<_>>()
            .join("\n\n")
    }
    fn execute<'a>(
        &'a self,
        name: &'a str,
        args: Value,
    ) -> Pin<Box<dyn Future<Output = Value> + Send + 'a>> {
        Box::pin(async move {
            match self.provider(name) {
                Some(provider) => provider.execute(name, args).await,
                None => serde_json::json!({"error":"unknown_tool"}),
            }
        })
    }
}

#[cfg(test)]
mod tests;
