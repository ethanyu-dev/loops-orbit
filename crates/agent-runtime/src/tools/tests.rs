use super::*;
use serde_json::json;
use std::sync::atomic::{AtomicUsize, Ordering};

/// 五百个本地工具夹具只统计业务调用，不访问第三方服务。
struct Fixture {
    /// 用于证明未加载名称不会触发执行。
    calls: AtomicUsize,
}
impl Host for Fixture {
    fn instructions(&self) -> String {
        "业务说明".into()
    }
    fn definitions(&self) -> Vec<Value> {
        (0..500).map(|i|json!({"type":"function","function":{"name":format!("fixture_{i:03}"),"description":"业务查询","parameters":{"type":"object","properties":{}}}})).collect()
    }
    fn catalog(&self) -> Vec<Descriptor> {
        (0..500)
            .map(|i| Descriptor {
                name: format!("fixture_{i:03}"),
                provider: "fixture".into(),
                description: "查询本人任务".into(),
                effect: "read".into(),
                keywords: "issue assigned 待办".into(),
            })
            .collect()
    }
    fn execute<'a>(
        &'a self,
        _: &'a str,
        _: Value,
    ) -> Pin<Box<dyn Future<Output = Value> + Send + 'a>> {
        Box::pin(async move {
            self.calls.fetch_add(1, Ordering::SeqCst);
            json!({"ok":true})
        })
    }
}

// 验证大目录有界发现、分页、LRU 移除和说明退出；不验证真实模型的召回选择质量。
#[test]
fn bounded_discovery_loading_and_eviction() {
    let host = Fixture {
        calls: AtomicUsize::new(0),
    };
    let mut session = Session::default();
    assert!(session.definitions(Some(&host)).is_empty());
    let results = session.manage(Some(&host), "tools_search", json!({"query":"待办"}));
    assert_eq!(results["total"], 500);
    assert_eq!(results["items"].as_array().unwrap().len(), 5);
    assert!(!results.to_string().contains("parameters"));
    let next = session.manage(Some(&host), "tools_search", json!({"offset":5,"limit":8}));
    assert_eq!(next["items"][0]["name"], "fixture_005");
    let names: Vec<_> = (0..8).map(|i| format!("fixture_{i:03}")).collect();
    session.manage(Some(&host), "tools_load", json!({"names":names}));
    session.business("fixture_000");
    let result = session.manage(Some(&host), "tools_load", json!({"names":["fixture_008"]}));
    assert_eq!(result["removed"], json!(["fixture_001"]));
    assert_eq!(session.definitions(Some(&host)).len(), 8);
    let before = session.names().to_vec();
    assert_eq!(
        session.manage(Some(&host), "tools_load", json!({"names":["missing"]}))["error"],
        "tool_unavailable"
    );
    assert_eq!(session.names(), before);
    session.manage(
        Some(&host),
        "tools_load",
        json!({"names":[],"unload":before}),
    );
    assert!(host.instructions_for(session.names()).is_empty());
}

// 本地 HTTP 夹具验证首轮只有三个工具、加载同轮不能越权和重新移除后的拒绝；不模拟真实模型规划。
#[tokio::test]
async fn per_request_whitelist_prevents_hidden_execution() {
    use axum::{Json, Router, routing::post};
    let app=Router::new().route("/v1/chat/completions",post(|Json(body):Json<Value>|async move {
        let results:Vec<Value>=body["messages"].as_array().unwrap().iter().filter(|m|m["role"]=="tool").map(|m|serde_json::from_str(m["content"].as_str().unwrap()).unwrap()).collect();
        let call=|id:&str,name:&str,args:Value|json!({"id":id,"type":"function","function":{"name":name,"arguments":args.to_string()}});
        let message=match results.len() {
            0=>{assert_eq!(body["tools"].as_array().unwrap().len(),3);assert!(!body["messages"][0]["content"].as_str().unwrap().contains("业务说明"));json!({"tool_calls":[call("load","tools_load",json!({"names":["fixture_000"]})),call("hidden","fixture_000",json!({}))]})},
            2=>{assert_eq!(results[1]["error"],"tool_not_loaded");assert_eq!(body["tools"].as_array().unwrap().len(),4);json!({"tool_calls":[call("ok","fixture_000",json!({})),call("unload","tools_load",json!({"names":[],"unload":["fixture_000"]}))]})},
            4=>{assert_eq!(results[2]["ok"],true);assert_eq!(body["tools"].as_array().unwrap().len(),3);assert!(!body["messages"][0]["content"].as_str().unwrap().contains("业务说明"));json!({"tool_calls":[call("removed","fixture_000",json!({}))]})},
            5=>{assert_eq!(results[4]["error"],"tool_not_loaded");json!({"content":"完成"})},
            _=>panic!("意外轮次"),
        };
        Json(json!({"choices":[{"message":message}]}))
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let runtime = crate::Runtime::new(crate::ModelConfig {
        base_url: format!("http://{address}/v1"),
        api_key: "fixture".into(),
        model: "fixture".into(),
        tools_enabled: true,
        stream_enabled: false,
    })
    .unwrap();
    let host = Fixture {
        calls: AtomicUsize::new(0),
    };
    assert_eq!(
        runtime
            .run_with_tools(&[], None, Some(&host))
            .await
            .unwrap(),
        "完成"
    );
    assert_eq!(host.calls.load(Ordering::SeqCst), 1);
    server.abort();
}
