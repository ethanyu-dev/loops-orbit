use super::Host;
use serde::Deserialize;
use serde_json::{Value, json};

// 工具数和 UTF-8 字节预算分别防止大目录和超长 schema 挤满请求；不把字节估计冒充精确 token。
const MAX_LOADED: usize = 8;
const MAX_SCHEMA_BYTES: usize = 32000;
const MAX_DISCOVERY_CALLS: usize = 12;
const MAX_BUSINESS_CALLS: usize = 48;
const MAX_SEARCH_RESULTS: usize = 8;
const DEFAULT_SEARCH_RESULTS: usize = 5;
/// 同一次用户任务的工具可见状态，不在跨身份缓存中保存授权。
#[derive(Default)]
pub struct Session {
    /// 从最久未用到最近使用排序，用于确定性移除。
    loaded: Vec<String>,
    /// 发现与加载单独计数，避免耗尽业务调用预算。
    discovery_calls: usize,
    /// 时间与业务工具的有界执行次数。
    business_calls: usize,
}
/// 可分页浏览目录，也可按模块或中英文意图筛选。
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Search {
    /// 空查询用于目录浏览，防止词法召回未命中时无路可查。
    #[serde(default)]
    query: String,
    /// 模块名称精确筛选。
    provider: Option<String>,
    /// 排名后的记录偏移。
    #[serde(default)]
    offset: usize,
    /// 单次最多八个候选，不返回全量目录。
    limit: Option<usize>,
}
/// 加载与显式释放原子生效；失败时保留原集合。
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Load {
    /// 要启用的真实工具名称。
    names: Vec<String>,
    /// 可提前释放不再需要的工具。
    #[serde(default)]
    unload: Vec<String>,
}
impl Session {
    /// 当前集合只用于生成下一轮请求，执行时另取不可变的本轮快照。
    pub fn names(&self) -> &[String] {
        &self.loaded
    }
    /// 每轮只读取已加载 schema。
    pub fn definitions(&self, host: Option<&dyn Host>) -> Vec<Value> {
        host.map(|host| {
            self.loaded
                .iter()
                .filter_map(|name| host.definition(name))
                .collect()
        })
        .unwrap_or_default()
    }
    /// 基础工具始终常驻，业务工具必须出现在本轮实际公布的集合。
    pub fn visible(name: &str, advertised: &[String]) -> bool {
        matches!(name, "current_time" | "tools_search" | "tools_load")
            || advertised.iter().any(|n| n == name)
    }
    /// 只有成功通过可见性检查的调用才计数，未加载调用也不会获得执行权限。
    pub fn business(&mut self, name: &str) -> bool {
        if self.business_calls >= MAX_BUSINESS_CALLS {
            return false;
        }
        self.business_calls += 1;
        if let Some(index) = self.loaded.iter().position(|n| n == name) {
            let name = self.loaded.remove(index);
            self.loaded.push(name);
        }
        true
    }
    /// 发现和加载没有外部副作用，全部输出为结构化状态而非完整 schema。
    pub fn manage(&mut self, host: Option<&dyn Host>, name: &str, args: Value) -> Value {
        if self.discovery_calls >= MAX_DISCOVERY_CALLS {
            return json!({"error":"tool_discovery_budget_exhausted"});
        }
        self.discovery_calls += 1;
        match name {
            "tools_search" => self.search(host, args),
            "tools_load" => self.load(host, args),
            _ => json!({"error":"unknown_tool"}),
        }
    }
    /// 词法检索使用别名、完整词和中文双字片段；空查询提供有界浏览兜底。
    fn search(&self, host: Option<&dyn Host>, args: Value) -> Value {
        let Ok(input) = serde_json::from_value::<Search>(args) else {
            return json!({"error":"invalid_arguments"});
        };
        let limit = input.limit.unwrap_or(DEFAULT_SEARCH_RESULTS);
        if input.query.chars().count() > 200 || limit == 0 || limit > MAX_SEARCH_RESULTS {
            return json!({"error":"invalid_arguments"});
        }
        let query = input.query.trim().to_lowercase();
        let mut terms: Vec<String> = query
            .split(|c: char| !c.is_alphanumeric())
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .collect();
        terms.extend(
            query
                .split(|c: char| !c.is_ascii_alphanumeric() && c != '_')
                .filter(|word| !word.is_empty())
                .map(str::to_owned),
        );
        let chars: Vec<char> = query.chars().collect();
        terms.extend(
            chars
                .windows(2)
                .filter(|w| w.iter().all(|c| !c.is_ascii()))
                .map(|w| w.iter().collect::<String>()),
        );
        let mut found: Vec<_> = host
            .map(|h| h.catalog())
            .unwrap_or_default()
            .into_iter()
            .filter(|tool| input.provider.as_ref().is_none_or(|p| p == &tool.provider))
            .filter_map(|tool| {
                let text = format!(
                    "{} {} {} {}",
                    tool.name, tool.provider, tool.description, tool.keywords
                )
                .to_lowercase();
                let score = terms
                    .iter()
                    .filter(|term| text.contains(term.as_str()))
                    .count();
                (query.is_empty() || score > 0).then_some((score, tool))
            })
            .collect();
        found.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.name.cmp(&b.1.name)));
        let total = found.len();
        if input.offset > total {
            return json!({"error":"invalid_arguments"});
        }
        let items: Vec<_> = found
            .into_iter()
            .skip(input.offset)
            .take(limit)
            .map(|(_, tool)| tool)
            .collect();
        let next = input.offset + items.len();
        json!({"items":items,"total":total,"next_offset":(next<total).then_some(next),"has_more":next<total,"loaded":self.loaded})
    }
    /// 预算包括 schema 和模块说明；只淘汰非本次请求工具，无法容纳时整体失败。
    fn load(&mut self, host: Option<&dyn Host>, args: Value) -> Value {
        let Ok(input) = serde_json::from_value::<Load>(args) else {
            return json!({"error":"invalid_arguments"});
        };
        let Some(host) = host else {
            return json!({"error":"tool_unavailable"});
        };
        if input.names.len() > MAX_LOADED
            || input.unload.len() > MAX_LOADED
            || input.names.iter().any(|name| input.unload.contains(name))
        {
            return json!({"error":"invalid_arguments"});
        }
        let catalog = host.catalog();
        if input.names.iter().any(|name| {
            !catalog.iter().any(|tool| &tool.name == name) || host.definition(name).is_none()
        }) {
            return json!({"error":"tool_unavailable"});
        }
        let mut next: Vec<String> = self
            .loaded
            .iter()
            .filter(|name| !input.unload.contains(name) && !input.names.contains(name))
            .cloned()
            .collect();
        for name in &input.names {
            if !next.contains(name) {
                next.push(name.clone());
            }
        }
        loop {
            let bytes = next
                .iter()
                .filter_map(|name| host.definition(name))
                .map(|definition| definition.to_string().len())
                .sum::<usize>()
                + host.instructions_for(&next).len();
            if next.len() <= MAX_LOADED && bytes <= MAX_SCHEMA_BYTES {
                break;
            }
            let Some(index) = next.iter().position(|name| !input.names.contains(name)) else {
                return json!({"error":"tool_schema_budget_exceeded"});
            };
            next.remove(index);
        }
        let removed: Vec<_> = self
            .loaded
            .iter()
            .filter(|name| !next.contains(name))
            .cloned()
            .collect();
        self.loaded = next;
        json!({"loaded":self.loaded,"removed":removed,"available_from":"next_model_request"})
    }
}
