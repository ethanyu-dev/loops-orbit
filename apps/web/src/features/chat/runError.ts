// 只解释已知分类；旧记录没有原始结束原因，不能直接认定为输出超限。
const RUN_ERRORS: Record<string, string> = {
  provider_stream_transfer_limit: '模型流传输超过限制，请稍后重试或联系管理员检查响应大小。',
  provider_stream_event_limit: '模型返回了过大的流事件，请联系管理员检查模型服务。',
  provider_stream_content_limit: '模型回复或工具参数过大，请缩小请求范围后重试。',
  provider_output_limit: '模型输出达到上限。请缩小问题范围后重试，或联系管理员调整输出预算。',
  provider_content_filtered: '模型服务过滤了本次输出。请调整问题后重试。',
  provider_incomplete_response:
    '模型响应未正常结束。请稍后重试；管理员可查看服务日志中的结束原因。',
};

/** 失败原因转成可行动的提示，未知错误保留通用文案及页面上的诊断码。 */
export function runError(code: string | null) {
  return (code && RUN_ERRORS[code]) || '这次运行未完成。你可以重新发送消息。';
}
