use super::*;

/// 只提供固定原文，不连接飞书或真实模型。
fn message(id: &str, text: &str) -> Message {
    Message {
        message_id: id.into(),
        chat_id: "chat".into(),
        sender_id: "sender".into(),
        sender_name: "成员".into(),
        sender_id_type: "open_id".into(),
        sender_type: "user".into(),
        is_me: false,
        create_time: 1,
        update_time: 1,
        message_type: "text".into(),
        deleted: false,
        text: text.into(),
        payload: Value::Null,
    }
}
/// 候选带伪造身份字段，用于证明服务器始终覆盖这些字段。
fn candidate(id: &str, quote: &str) -> Value {
    json!({"kind":"fact_candidate","text":"待核对信息","message_id":id,"quote":quote,
        "sender_id":"forged","create_time":99,"is_me":true})
}

// 验证多行、链接、标点和星号必须逐字保留；不验证真实模型的输出概率或语义蕴含。
#[test]
fn exact_quotes_preserve_format_and_server_identity() {
    let text = "第一行\n[邮箱](mailto:fixture@example.test)\nsk_fixture****END，待确认";
    let raw = vec![message("m1", text)];
    let item = checked(candidate("m1", text), &raw).unwrap();
    assert_eq!(item.quote, text);
    assert_eq!(item.sender_id, "sender");
    assert_eq!(item.create_time, 1);
    assert!(!item.is_me);
    for altered in [
        text.replace('\n', " "),
        text.replace("****", "***"),
        text.replace('，', ","),
        "邮箱 sk_fixture****END".into(),
    ] {
        assert_eq!(
            checked(candidate("m1", &altered), &raw).err().unwrap().1,
            "communication_summary_quote_mismatch"
        );
    }
}

// 验证错 ID、跨消息拼接、已撤回及承诺归属分别被拒绝；不把匹配通过等同于语义正确。
#[test]
fn evidence_errors_distinguish_message_quote_and_owner() {
    let mut raw = vec![message("m1", "第一段"), message("m2", "第二段")];
    assert_eq!(
        checked(candidate("missing", "第一段"), &raw)
            .err()
            .unwrap()
            .1,
        "communication_summary_unknown_message"
    );
    assert_eq!(
        checked(candidate("m1", "第一段第二段"), &raw)
            .err()
            .unwrap()
            .1,
        "communication_summary_quote_mismatch"
    );
    let mut wrong_owner = candidate("m1", "第一段");
    wrong_owner["kind"] = json!("my_commitment");
    assert_eq!(
        checked(wrong_owner, &raw).err().unwrap().1,
        "communication_summary_invalid_owner"
    );
    raw[0].deleted = true;
    assert_eq!(
        checked(candidate("m1", "第一段"), &raw).err().unwrap().1,
        "communication_summary_unknown_message"
    );
}

// 旧版完整摘要缺失覆盖计数字段仍可读取；不模拟损坏文件或数据库迁移。
#[test]
fn legacy_summary_defaults_coverage_counts() {
    let summary: Summary = serde_json::from_value(
        json!({"raw_hash":"hash","message_count":0,"unsupported_count":0,"items":[]}),
    )
    .unwrap();
    assert_eq!(summary.rejected_count, 0);
    assert_eq!(summary.failed_chunk_count, 0);
}
