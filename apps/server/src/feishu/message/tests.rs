use super::*;
use axum::{
    Json, Router,
    extract::State,
    http::{HeaderMap, StatusCode},
    routing::post,
};
use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
};

/// 测试消息只使用合成内容；不会访问真实账号、飞书或模型。
fn outgoing(content: &str, delegated: bool) -> Outgoing<'_> {
    Outgoing {
        id: Uuid::nil(),
        content,
        receiver: Some("ou_fixture"),
        delegated,
    }
}

// 验证截图所需 Markdown 结构和两种身份的展示标识；不代替真实飞书客户端视觉验收。
#[test]
fn card_preserves_markdown_and_separates_attribution() {
    let markdown = "先确认 **环境**。\n\n1. 选择服务\n2. 设置 `image_tag`\n\n```yaml\nimage: stable\n```\n\n[参考文档](https://example.com)";
    for delegated in [false, true] {
        let text = if delegated {
            format!("{AGENT_PREFIX}{markdown}")
        } else {
            markdown.into()
        };
        let body = bounded_request(&outgoing(&text, delegated), Format::Card).unwrap();
        assert_eq!(body["msg_type"], "interactive");
        let card: Value = serde_json::from_str(body["content"].as_str().unwrap()).unwrap();
        assert_eq!(card["schema"], "2.0");
        assert!(card.get("header").is_none());
        let elements = card["body"]["elements"].as_array().unwrap();
        assert_eq!(elements.last().unwrap()["content"], markdown);
        assert_eq!(elements.len(), if delegated { 2 } else { 1 });
        if delegated {
            assert_eq!(elements[0]["content"], AGENT_PREFIX.trim_end());
            assert_eq!(elements[0]["text_size"], "notation");
            assert!(
                card["config"]["summary"]["content"]
                    .as_str()
                    .unwrap()
                    .starts_with(AGENT_PREFIX.trim_end())
            );
        }
    }
}

// 验证未授权的扩展标签与图片不会变为通知/资源组件；代码中的原始语法应保持原样。
#[test]
fn rendering_neutralizes_platform_tags_but_preserves_code() {
    let source = "**说明** <at id=all>大家</at>\n\n![图示](https://example.com/a.png)\n\n`<at id=all>`\n\n```html\n<at id=all>\n```\n\n<https://example.com>";
    let rendered = display_markdown(source);
    assert!(rendered.contains("&lt;at id=all&gt;大家&lt;/at&gt;"));
    assert!(rendered.contains("[图示](https://example.com/a.png)"));
    assert!(!rendered.contains("![图示]"));
    assert!(rendered.contains("`<at id=all>`"));
    assert!(rendered.contains("```html\n<at id=all>\n```"));
    assert!(rendered.ends_with("<https://example.com>"));
}

// 验证完整请求按序列化字节限长、超限退为原文且保留标识；不验证平台内部样式展开算法。
#[test]
fn size_limit_accounts_for_utf8_and_double_json_escaping() {
    let unicode = "中文🪐".repeat(1500);
    assert_eq!(
        bounded_request(&outgoing(&unicode, false), Format::Card).unwrap()["msg_type"],
        "interactive"
    );
    let escaped = format!("{AGENT_PREFIX}{}", "\0".repeat(6000));
    let body = bounded_request(&outgoing(&escaped, true), Format::Card).unwrap();
    assert_eq!(body["msg_type"], "text");
    let content: Value = serde_json::from_str(body["content"].as_str().unwrap()).unwrap();
    assert_eq!(content["text"], escaped);
    assert!(body.to_string().len() <= TEXT_REQUEST_BYTES);
    assert!(bounded_request(&outgoing(&"长".repeat(60_000), false), Format::Card).is_err());
}

/// 本地供应商脚本记录实际 HTTP 请求，按指定顺序返回结果，不伪装成真实平台。
struct Fixture {
    /// 待返回的 HTTP 状态和原始正文，用于区分明确拒绝、未知结果和解析失败。
    responses: VecDeque<(StatusCode, String)>,
    /// 请求及凭证标识均为夹具数据，检查回退未切换身份、目标或幂等键。
    requests: Vec<Value>,
}

/// 运行一次生产发送器并回收监听任务；不依赖数据库或外部凭证。
async fn send_script(
    responses: Vec<(StatusCode, String)>,
    delegated: bool,
) -> (Result<Value, ()>, Vec<Value>) {
    let fixture = Arc::new(Mutex::new(Fixture {
        responses: responses.into(),
        requests: vec![],
    }));
    let app = Router::new()
        .route(
            "/reply",
            post(
                |State(f): State<Arc<Mutex<Fixture>>>,
                 headers: HeaderMap,
                 Json(body): Json<Value>| async move {
                    assert_eq!(headers["authorization"], "Bearer fixture-token");
                    let mut f = f.lock().unwrap();
                    f.requests.push(body);
                    f.responses
                        .pop_front()
                        .unwrap_or((StatusCode::INTERNAL_SERVER_ERROR, "unexpected retry".into()))
                },
            ),
        )
        .with_state(fixture.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url =
        reqwest::Url::parse(&format!("http://{}/reply", listener.local_addr().unwrap())).unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let result = send(
        &reqwest::Client::new(),
        url,
        "fixture-token",
        outgoing("可以，先 **注册账号**。", delegated),
    )
    .await;
    server.abort();
    let requests = fixture.lock().unwrap().requests.clone();
    (result, requests)
}

// 验证明确卡片创建失败才发送富文本，两个身份使用相同策略；不证明租户已授予卡片发送权限。
#[tokio::test]
async fn explicit_card_rejection_falls_back_once_with_same_identity_and_uuid() {
    for delegated in [false, true] {
        let (result, requests) = send_script(
            vec![
                (StatusCode::BAD_REQUEST, json!({"code":230099}).to_string()),
                (
                    StatusCode::OK,
                    json!({"code":0,"data":{"message_id":"om_fixture"}}).to_string(),
                ),
            ],
            delegated,
        )
        .await;
        assert!(result.is_ok());
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0]["msg_type"], "interactive");
        assert_eq!(requests[1]["msg_type"], "post");
        assert_eq!(requests[0]["uuid"], requests[1]["uuid"]);
        assert_eq!(requests[1]["receive_id"], "ou_fixture");
        let post: Value = serde_json::from_str(requests[1]["content"].as_str().unwrap()).unwrap();
        let rows = post["zh_cn"]["content"].as_array().unwrap();
        assert_eq!(rows.last().unwrap()[0]["tag"], "md");
        assert_eq!(rows.last().unwrap()[0]["text"], "可以，先 **注册账号**。");
        if delegated {
            assert_eq!(rows[0][0]["text"], AGENT_PREFIX.trim_end());
        }
    }
}

// 验证权限、限流、服务故障、未知/无效结果不会另发兜底消息；不改变机器人原有队列重试策略。
#[tokio::test]
async fn other_errors_and_ambiguous_results_never_send_fallback() {
    for (status, body) in [
        (StatusCode::BAD_REQUEST, json!({"code":230027}).to_string()),
        (StatusCode::BAD_REQUEST, json!({"code":230020}).to_string()),
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            json!({"code":230099}).to_string(),
        ),
        (StatusCode::OK, json!({"code":230049}).to_string()),
        (StatusCode::OK, json!({"code":0}).to_string()),
        (StatusCode::OK, "not json".into()),
    ] {
        let (result, requests) = send_script(vec![(status, body)], true).await;
        assert!(result.is_err());
        assert_eq!(requests.len(), 1);
    }
}

// 验证成功后只有一次发送、正文不附人工拼接引用；引用原消息由调用方的 reply URL 保留。
#[tokio::test]
async fn accepted_card_does_not_send_duplicate_fallback() {
    let (result, requests) = send_script(
        vec![(
            StatusCode::OK,
            json!({"code":0,"data":{"message_id":"om_fixture"}}).to_string(),
        )],
        false,
    )
    .await;
    assert!(result.is_ok());
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0]["msg_type"], "interactive");
}

// 验证纯文本降级也不会重新激活 @ 标签；这是发送载荷检查，不断言所有客户端的转义显示细节。
#[test]
fn plain_fallback_escapes_platform_mentions() {
    let body = request(
        &outgoing("<at user_id=\"all\">所有人</at>", true),
        Format::Text,
    );
    let content: Value = serde_json::from_str(body["content"].as_str().unwrap()).unwrap();
    let text = content["text"].as_str().unwrap();
    assert!(text.starts_with(AGENT_PREFIX));
    assert!(!text.contains("<at"));
    assert!(text.contains("&lt;at"));
}

// 验证连接已接受但迟迟不返回时，总超时不会触发第二次投递；不访问真实飞书。
#[tokio::test]
async fn timeout_does_not_send_a_second_message() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let received = Arc::new(AtomicUsize::new(0));
    let counter = received.clone();
    let app = Router::new().route(
        "/reply",
        post(move || {
            let counter = counter.clone();
            async move {
                counter.fetch_add(1, Ordering::SeqCst);
                tokio::time::sleep(SEND_TIMEOUT + Duration::from_secs(1)).await;
                Json(json!({"code":0,"data":{"message_id":"om_delayed"}}))
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url =
        reqwest::Url::parse(&format!("http://{}/reply", listener.local_addr().unwrap())).unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    assert!(
        send(
            &reqwest::Client::new(),
            url,
            "fixture-token",
            outgoing("你好。", true)
        )
        .await
        .is_err()
    );
    assert_eq!(received.load(Ordering::SeqCst), 1);
    server.abort();
}
