use serde_json::Value;
use std::{future::Future, pin::Pin};

/// 固定工具宿主契约，实际身份、授权、幂等和租约校验由宿主完成。
pub trait Host: Send + Sync {
    /// 本轮真实可用能力说明，不包含用户可修改的指令。
    fn instructions(&self) -> String;
    /// 服务端固定工具 schema，不接受模型动态注册。
    fn definitions(&self) -> Vec<Value>;
    /// 每次动作返回可供下一轮模型读取的真实结果。
    fn execute<'a>(
        &'a self,
        name: &'a str,
        args: Value,
    ) -> Pin<Box<dyn Future<Output = Value> + Send + 'a>>;
}
