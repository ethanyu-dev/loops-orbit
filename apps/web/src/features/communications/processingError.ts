// 仅解释服务端稳定分类，历史通用错误不能推断成额度或权限问题。
const ERRORS: Record<string, string> = {
  communication_image_forbidden: '飞书拒绝原图访问（HTTP 401/403），请检查授权及资源权限',
  communication_image_not_found: '飞书原图不存在或已失效（HTTP 404/410）',
  communication_image_rate_limited: '飞书原图读取触发限流（HTTP 429）',
  communication_image_download_timeout: '读取飞书原图超时',
  communication_image_download_failed: '原图下载连接中断或响应读取失败',
  communication_image_unavailable: '飞书原图读取失败，可能已失效或无读取权限',
  communication_image_too_large: '原图超过 8 MiB，无法自动解读',
  communication_image_format: '原图格式不支持，当前支持 PNG、JPEG、GIF 和 WebP',
  communication_unavailable: '处理失败，旧版记录未保留具体原因；下次尝试后更新',
  provider_rejected: '模型服务拒绝请求，请检查额度、权限或模型参数',
  provider_unreachable: '无法连接模型服务或请求超时',
  provider_read_failed: '读取模型响应失败',
  provider_output_limit: '模型输出达到上限，请减少单次整理范围或联系管理员调整预算',
  provider_content_filtered: '模型服务过滤了本次输出，请调整问题后重试',
  provider_incomplete_response: '模型输出未完成，可能达到输出上限',
  provider_empty_response: '模型未返回有效内容',
  provider_invalid_json: '模型服务响应格式无效',
  communication_summary_invalid_quote: '引用无法与对应原文核对',
  communication_summary_unknown_message: '引用指向了本次消息中不存在的记录',
  communication_summary_quote_mismatch: '引用与对应消息的逐字原文不一致',
  communication_summary_invalid_owner: '承诺归属与原始发送者不一致',
  communication_summary_invalid_item: '整理条目的分类、内容或长度不符合要求',
  communication_summary_invalid_format: '模型未返回符合要求的整理条目',
  invalid_communication_summary: '模型未返回有效的整理 JSON',
  communication_summary_too_many_items: '模型返回的整理条目超过数量限制',
  communication_summary_invalid_repair: '模型未返回有效的引用纠正结果',
  communication_summary_timeout: '本次整理超时',
  communication_summary_interrupted: '整理多次中断，已停止自动重试',
  communication_summary_source_inactive: '来源已暂停、正在删除或尚未完成核对',
  communication_summary_retry_unavailable: '当前状态不需要重新整理',
  invalid_image_description: '模型未返回有效图片解读',
};
/** 保留未知分类供定位，但不把未验证的原因显示成结论。 */
export function processingError(code: string) {
  return ERRORS[code] || '处理未完成，请稍后刷新查看';
}
