import ReactMarkdown from 'react-markdown';
import remarkGfm from 'remark-gfm';

/** 完整与流式回复共用安全 Markdown 渲染，不加载远程图片或执行原始 HTML。 */
export function Answer({ content }: { /** 模型正文，始终按不可信文本处理。 */ content: string }) {
  return (
    <div className="markdown">
      <ReactMarkdown
        remarkPlugins={[remarkGfm]}
        skipHtml
        components={{
          a: ({ children, href }) => (
            <a href={href} target="_blank" rel="noopener noreferrer">
              {children}
            </a>
          ),
          img: ({ alt }) => <span>[图片：{alt}]</span>,
        }}
      >
        {content}
      </ReactMarkdown>
    </div>
  );
}
