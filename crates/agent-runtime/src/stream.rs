use crate::{Failure, MAX_RESPONSE_BYTES, MAX_TOOL_CALLS, failure};
use serde_json::{Value, json};
use tokio::sync::watch;

/// 按字节缓存 SSE 行，网络分块可以落在 UTF-8 字符或事件的任意位置。
#[derive(Default)]
pub(crate) struct Decoder {
    /// 未出现换行的尾部字节。
    buffer: Vec<u8>,
    /// 当前事件的 data 行，多个 data 行以换行拼接。
    data: Vec<String>,
    /// 累积的回复正文，仅发布模型真实生成的内容。
    content: String,
    /// 按索引拼接的工具名称与参数。
    calls: Vec<Value>,
    /// 上游已明确发送结束事件或 finish_reason。
    finished: bool,
    /// 收到 [DONE] 后即可结束读取，不依赖代理关闭连接。
    done: bool,
    /// 整个响应的有界字节计数。
    received: usize,
    /// 在结束事件后仍读取 usage；不保存原始响应或推理正文。
    pub(crate) metadata: crate::response::Metadata,
}
impl Decoder {
    /// 逐行处理完整事件，不把网络截断误判为成功回复。
    pub(crate) fn push(
        &mut self,
        bytes: &[u8],
        progress: Option<&watch::Sender<String>>,
    ) -> Result<(), Failure> {
        self.received += bytes.len();
        if self.received > MAX_RESPONSE_BYTES {
            return Err(failure("provider_response_too_large", false));
        }
        self.buffer.extend_from_slice(bytes);
        while let Some(end) = self.buffer.iter().position(|b| *b == b'\n') {
            let line = self.buffer.drain(..=end).collect::<Vec<_>>();
            let line = std::str::from_utf8(&line)
                .map_err(|_| failure("provider_invalid_stream", false))?;
            let line = line.trim_end_matches(['\r', '\n']);
            if line.is_empty() {
                self.event(progress)?;
            } else if let Some(data) = line.strip_prefix("data:") {
                self.data
                    .push(data.strip_prefix(' ').unwrap_or(data).to_owned());
            }
        }
        Ok(())
    }

    /// 消费一个 SSE 事件，工具参数只能拼接，不能提前执行尚未完整的 JSON。
    fn event(&mut self, progress: Option<&watch::Sender<String>>) -> Result<(), Failure> {
        if self.data.is_empty() {
            return Ok(());
        }
        let data = std::mem::take(&mut self.data).join("\n");
        if data == "[DONE]" {
            self.finished = true;
            self.done = true;
            return Ok(());
        }
        let value: Value =
            serde_json::from_str(&data).map_err(|_| failure("provider_invalid_stream", false))?;
        if value.get("error").is_some() {
            return Err(failure("provider_stream_error", true));
        }
        self.metadata.observe(&value);
        let Some(choice) = value["choices"]
            .as_array()
            .and_then(|choices| choices.first())
        else {
            return Ok(());
        };
        if choice["finish_reason"].is_string() {
            self.finished = true;
        }
        // 失败事件之后只收集诊断信息，不发布半截内容，也不组装可执行的工具调用。
        if self.metadata.validate().is_err() {
            return Ok(());
        }
        let delta = &choice["delta"];
        if let Some(text) = delta["content"].as_str() {
            self.content.push_str(text);
            if let Some(progress) = progress {
                progress.send_replace(self.content.clone());
            }
        }
        if let Some(calls) = delta["tool_calls"].as_array() {
            for call in calls {
                let index = call["index"]
                    .as_u64()
                    .filter(|i| *i < MAX_TOOL_CALLS as u64)
                    .ok_or(failure("invalid_tool_calls", false))?
                    as usize;
                while self.calls.len() <= index {
                    self.calls.push(
                        json!({"id":"", "type":"function", "function":{"name":"", "arguments":""}}),
                    );
                }
                for path in ["/id", "/function/name", "/function/arguments"] {
                    if let Some(fragment) = call.pointer(path).and_then(Value::as_str) {
                        let target = self.calls[index].pointer_mut(path).unwrap();
                        *target = Value::String(format!("{}{fragment}", target.as_str().unwrap()));
                    }
                }
            }
        }
        Ok(())
    }

    /// 用量事件位于 finish_reason 之后、[DONE] 之前，结束时已完成收集。
    pub(crate) fn is_done(&self) -> bool {
        self.done
    }

    /// 结束必须有协议标志；半截回复交给重试逻辑，不进入正式历史。
    pub(crate) fn finish(self) -> Result<Value, Failure> {
        self.metadata.validate()?;
        if !self.finished {
            return Err(failure("provider_stream_interrupted", true));
        }
        Ok(json!({"content": self.content, "tool_calls": self.calls}))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // 验证跨 UTF-8 字节分片、CRLF、注释和工具参数拼接；不覆盖真实代理的实现差异。
    #[test]
    fn fragmented_text_and_tools() {
        let input = concat!(
            ": ping\r\n\r\n",
            "data: {\"choices\":[{\"delta\":{\"content\":\"你好\"}}]}\r\n\r\n",
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"function\":{\"name\":\"current_time\",\"arguments\":\"{\"}}]}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"}\"}}]},\"finish_reason\":\"tool_calls\"}]}\n\n",
            "data: [DONE]\n\n"
        );
        let (tx, rx) = watch::channel(String::new());
        let mut decoder = Decoder::default();
        for byte in input.as_bytes() {
            decoder.push(&[*byte], Some(&tx)).unwrap();
        }
        assert_eq!(*rx.borrow(), "你好");
        let answer = decoder.finish().unwrap();
        assert_eq!(answer["tool_calls"][0]["function"]["arguments"], "{}");
        assert_eq!(answer["tool_calls"][0]["function"]["name"], "current_time");
    }

    // 验证断流和输出限长不会作为完整答案提交；不验证网络重连策略。
    #[test]
    fn rejects_unfinished_streams() {
        let mut decoder = Decoder::default();
        decoder
            .push(
                b"data: {\"choices\":[{\"delta\":{\"content\":\"partial\"}}]}\n\n",
                None,
            )
            .unwrap();
        assert_eq!(
            decoder.finish().unwrap_err().code,
            "provider_stream_interrupted"
        );
        let mut decoder = Decoder::default();
        decoder
            .push(
                b"data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"length\"}]}\n\n",
                None,
            )
            .unwrap();
        assert_eq!(decoder.finish().unwrap_err().code, "provider_output_limit");
    }
}
