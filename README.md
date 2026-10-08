# Orbit

[![verify](https://github.com/ethanyu-dev/loops-orbit/actions/workflows/ci.yml/badge.svg)](https://github.com/ethanyu-dev/loops-orbit/actions/workflows/ci.yml)

个人 Agent 服务：Rust 服务端与自研 runtime，Vite + React 网页，飞书单聊入口。PostgreSQL 统一保存会话、访问授权和任务队列，前端静态服务与 Rust API 分别构建、部署到 Railway。

当前版本为 **v0.1.0**，面向个人自托管使用。[下载首版源码](https://github.com/ethanyu-dev/loops-orbit/releases/tag/v0.1.0) · [版本记录](CHANGELOG.md)。飞书沟通采集和主动回访均需单独启用；真实模型、飞书租户和 Railway 环境仍需按下文完成部署验收。

## 首版能力

- 管理员通过 `ADMIN_TOKEN` 登录，聊天、查看历史、管理临时访问链接和查看运行状态。
- 临时链接可以设置 1 分钟至 30 天有效期，并随时撤销。链接只授权访客聊天，不授予管理权限。
- 管理员能查看所有会话；访客只能查看所属链接的会话。同一链接的不同使用者共享访客空间，建议每人单独生成链接。
- 飞书机器人通过加密事件回调接收白名单用户的单聊文本，异步回复。另有独立、默认关闭的用户授权连接，可手动订阅授权范围内单聊和群聊的新消息；群聊资料不会进入机器人回复队列。
- 模型使用 OpenAI 兼容的 **Chat Completions** 协议，自定义 `OPENAI_BASE_URL`、`OPENAI_MODEL`、`OPENAI_API_KEY`。
- runtime 自行实现上下文组装、模型调用、工具校验和工具循环；内置 `current_time` 只读工具，并支持服务端提供的沟通资料查询、读取及提醒管理工具。没有接入现成 Agent 框架。
- 网页允许连续补充、改口和停止回复；同一轮的新输入替换旧生成，并保留尚未回答的用户原文。网页通过持久化快照逐步呈现模型流式输出，刷新后恢复当前会话。
- 较早的会话历史增量整理为摘要，近期内容保留原文；跨会话记忆使用本地 Markdown 原文、中文 BM25 和可选 pgvector。提供单次提醒和需用户开启的轻量回访；不提供通用文件操作、Shell 和联网搜索。

## 控制台外观

控制台采用左侧会话栏与右侧内容区的两栏布局；账号固定在左下角，点击账号可展开个人记忆、提醒、飞书资料、访问链接和状态入口。登录页与工作空间账号菜单可切换浅色、深色主题；首次访问跟随系统外观，手动选择后保存在当前浏览器并跨标签页同步。对话、记忆、提醒、飞书资料、访问链接和状态页使用统一配色。飞书资料页按“月份 → 北京时间日期 → 群聊或联系人日文件”浏览，日期目录和文件列表分别分页，可访问完整历史；名称/日期搜索查找所有日文件，消息内容检索返回最多 6 份相关资料。详情返回保留目录、搜索和页码。订阅管理采用紧凑列表，支持弹窗确认移除全部订阅，默认保留资料，可勾选同时删除；已保留资料可以重新订阅或单独删除。移除会停止同步、历史任务及相关提醒，账号仍保持连接；确认绑定打开弹窗时的全部订阅版本，列表变化时需重新确认。页面提供会话选择与授权帮助，可复制 API 域名的 OAuth 回调地址；飞书侧需登记与服务端 `API_PUBLIC_URL` 一致的地址。

## 工具渐进加载

聊天首轮只发送 `current_time`、`tools_search`、`tools_load`。业务工具的轻量目录留在服务端；发现默认返回五项、最多八项，可按模块筛选或空查询分页浏览。加载成功后，下一轮请求才包含对应 schema 和模块说明。提醒与沟通资料均使用此机制，新模块通过 `tools::Registry` 注册。

每个任务维护独立加载集合，最多八个业务工具，schema 和说明合计最多 32,000 UTF-8 字节（不是精确 token 数）；超过预算移除非本次请求中最久未使用的工具，也可显式 unload。每轮重新生成详细说明，旧说明不会无限累积。发现与加载最多十二次，业务执行最多四十八次，仍受模型轮数和总运行时间约束。模型猜出的未加载名称、同一响应里刚加载的工具都不能执行；加载不会代替宿主的身份、租约与动作授权校验。关闭工具时不发送目录说明或工具 schema。

## 对话风格

Orbit 默认温和、直接、有判断力，根据用户是在交代任务、讨论想法、分享事情还是表达感受来调整回应。日常交流优先短句，复杂请求按需展开；避免固定客套、机械追问和没有依据的附和。用户补充或纠正要求时，跟随最新意图。

人格、表达规则、能力边界和场景示例统一维护在 `crates/agent-runtime/prompts/system.md`，通过 `include_str!` 编译进 runtime，修改后需要重新构建并重启服务。示例只用于指导表达，不作为用户历史。回复结合近期原文、历史摘要与已启用的长期记忆；提醒必须经真实工具保存成功后才确认，轻量回访需要用户明确开启；不承诺永久记忆。提示词能引导表达，但实际效果取决于所配置的模型，协议夹具测试不验证真实模型的对话质量。

## 连续交流与回复恢复

- 网页和飞书的新消息会在会话事务内替换尚未结束的生成；等待约 600 毫秒收集补充后再领取。连续消息保留为独立输入，模型结合它们回答，新的明确修正优先于旧要求。已经完成的回复保留；互不相关的工作可以新建会话。
- 网页“停止回复”只取消点击时的执行及其未完成输入批次。晚到的停止请求不会取消随后新建的执行；取消后的原文仍可查看，但不会作为待处理请求进入上下文。执行器定期检查数据库租约并断开旧请求，不能保证模型供应商同步停止计算或计费。
- 模型支持 SSE 时，正文片段约每 200 毫秒持久化；网页活跃时约每 300 毫秒读取快照，空闲时降低频率。刷新只恢复导航、状态与片段，不重发消息。半截回复不进入正式历史，断流重试会清空旧片段。飞书仍只投递最终完整回复。
- 同批补充最多 30 条、合计 24,000 字符，避免在用户取消前将未完成请求压入摘要。历史按预算增量压缩，摘要覆盖边界与内容一起保存；取消批次和失败回复不进入摘要。摘要失败会重试，保留旧检查点，不静默丢弃历史。摘要调用有额外模型耗时与用量，语义质量取决于模型。
- `crates/agent-runtime/prompts/summary.md` 单独定义摘要规则，保留明确偏好、约束、当前事项、已定结论和用户修正。摘要只是当前会话的背景资料，后续用户原文优先；不作为系统权限或跨用户记忆。

## 长期记忆

原文位于 `MEMORY_DIR/<owner 的 SHA-256>/<UUID>.md`，每条保留一个主题的当前事实。文件包含 JSON front matter 和 Markdown 正文，记录稳定主题、类型、更新时间、可选到期时间以及来源任务。`profile` 是小型常驻偏好档案（每轮约 4,000 字符预算），`project` 和 `episode` 按当前输入及上一条用户消息检索。档案和检索资料合计约 9,000 字符，最多补充 6 条命中；用户本轮明确修正优先。

- **原文与索引**：同目录临时文件同步落盘后原子重命名。Tantivy 在内存按身份维护 BM25 索引，中文使用 jieba 分词，文件哈希变化时重建。每条正文最多 2,000 字符，每身份最多 1,000 条活跃记忆。缓存可丢弃；手改文件时需保留有效的元数据和 UUID 文件名。
- **混合召回**：BM25 和 pgvector 精确余弦检索各取最多 20 个候选，用 RRF 按名次融合。语义候选使用可配置相似度下限，允许零结果。向量行只保存身份、ID、原文哈希、模型版本和向量；返回前再次校验原文，过期、已删除或哈希不符的向量不能返回。地址、模型、维度、显式修订号变化后自动补建。未配置 embedding、供应商失败或语义查询超过 3 秒时使用 BM25。每次后台扫描最多补建 16 条，实际周期取决于抽取和供应商耗时。
- **后台提取**：回复完成事务内记录任务，后台使用独立 `prompts/memory.md` 提取最多 4 条事实。只传已完成批次的用户输入，不传助手回答；返回证据必须是用户原文连续片段。已有主题沿用 key，较旧任务不能覆盖较新事实。失败最多 3 次，页面显示失败数量；部署前历史不自动回填。证据匹配只能验证出处，不能证明语义准确，应通过记忆页面检查修正。
- **修正与遗忘**：网页“个人记忆”支持创建、修正、到期和删除。为防止旧信息从摘要、近期原文和在途提取中复活，手工保存或遗忘会推进当前身份的历史边界、清除旧摘要并停止已有生成。边界前聊天仍可查看，但不再发送给模型或提取器；其他已保存记忆保留。删除文件改成无正文、无主题的墓碑，向量删除；文件中的 `_boundary.json` 用于崩溃恢复。此保守实现会同时失去旧聊天中的其他上下文，需要时重新说明。当前聊天没有直接执行遗忘的工具，请在页面操作。
- **身份隔离**：管理员、每个访客链接、每个飞书 open_id 分别存储，检索前就按身份选原文和向量。管理员查看飞书会话时，该会话仍使用原 owner；记忆页面只管理当前登录身份。尚未做跨渠道身份绑定，飞书档案不会自动成为网页管理员档案。
- **备份与手动编辑**：备份 PostgreSQL 和原文目录，索引可以重建。直接编辑文件会使索引失效，但不会清理旧聊天上下文；可靠遗忘应使用 API/页面。遗忘不会删除聊天记录或部署者自行制作的历史备份。不要删除 `_boundary.json` 或恢复过时原文，否则会破坏遗忘边界。

接口：`GET /api/memories?q=...` 返回记忆和提取/索引状态，`POST /api/memories` 创建，`PUT /api/memories/{id}` 修正，`DELETE /api/memories/{id}` 遗忘。写入字段为 `key`、`kind`、`content`、可选 `expires_at`（RFC3339）；身份、路径、来源和修改时间由服务端确定。

## 提醒与主动回访

- **明确提醒**：在对话里说“明天下午三点提醒我交材料”，或在“提醒与跟进”页面创建。时间不明确时应先澄清；按保存后的绝对时间执行，不受回访静默和冷却限制。默认补发宽限一天，超过截止时间直接过期。支持查询、改期、取消和标记完成。
- **轻量回访**：各身份默认关闭。页面开启，或明确说“允许主动跟进”后，只从之后完成的用户轮次发现一个有原文依据的未完事项，默认两天后判断。发送前重新检查近期交流、相关记忆、是否完成或取消；没有合适内容则延后。默认当地 22:00–08:00 静默，两次回访至少间隔 24 小时，用户最近 15 分钟有交流时暂缓。一次发送后不自动连环追问。
- **存储与恢复**：`followups` 保存任务、版本、来源与记忆依赖；`followup_preferences` 保存身份偏好；`followup_discovery` 和已有 `outbox` 分别处理发现与出站。明确提醒、模型回访、候选发现使用独立 worker。调度租约 90 秒，模型判断超时 35 秒、故障最多三次；飞书延续最多五次投递重试和稳定 UUID。关闭页面不影响调度，关闭服务器后只能在恢复时按有效期补发。
- **通知与连续性**：网页通过未读入口查看实际投递消息，点击“接着聊”返回原会话。主动消息有独立来源，不伪造用户轮次，也不作为用户记忆抽取证据；后续聊天可见事项状态和最近主动消息。飞书原会话的事项使用对应白名单 `open_id` 主动发送；网页与飞书不做跨身份绑定。飞书需具备机器人发送消息权限及用户可达条件，需用实际应用另行验收。
- **更新与遗忘**：取消、改期、关闭回访都会让旧版本和队列失效。记忆修正、遗忘或原文哈希变化会阻止依赖旧原文的发送；手工记忆变更推进历史边界时，也保守取消由旧历史产生的回访。发送前重验授权、来源、版本、上下文序号、静默和冷却。HTTP 已开始时取消不能撤回平台可能接受的消息，接口和页面会提示这一事实。

聊天动作工具和固定指令分别在 `apps/server/prompts/followup_tools.json`、`followup_tools.md`，候选发现和发送判断提示词在 `crates/agent-runtime/prompts/followup_*.md`。`AGENT_TOOLS_ENABLED=false` 时仍可用网页管理与后台调度，聊天不会承诺执行未提供的提醒工具。`FOLLOWUP_TIMEZONE` 默认为 `Asia/Shanghai`，只作为新身份的默认时区。无偏移的网页时间按页面显示时区解释；夏令时不存在或重复的本地时间会拒绝。

首版只支持单次提醒和轻量回访，不包含周期任务、外部进度监控、浏览器关闭后的系统推送或跨渠道身份合并。开启回访会额外调用模型进行发现与判断。模型输出属于建议，最终投递由服务端检查决定。

## 飞书沟通资料

聊天启用工具时，授权身份可主动调用 `communication_search` 按北京时间日期范围、来源及可选字面关键词分页查询，再用 `communication_read` 读取指定版本的摘要或原文。`current_time` 接受 IANA 时区，返回 UTC、当地时间和日期；“今天/昨天/本周”汇总先按 `Asia/Shanghai` 确定日期，再省略关键词列举该范围的资料，避免全历史相关性检索漏掉当天内容。自动相关性检索保留为非完整的背景候选。

查询默认每页 20 条，最多 50 条，单页条目另受 24,000 字节预算限制；结果携带总数、后续偏移和快照。续页必须传回快照，资料变化后重新查询。读取原文时长消息按 2,000 字符分段，可继续分页获取全文；摘要不可用或没有条目时明确回退原文。工具仍只读取启用来源，暂停/移除但保留的文件仅在资料页可浏览；结果分别报告排除、待核对及关键词扫描不可读数量，不能把未命中解释为没有记录。没有摘要或向量也能按日期查询原文，文件损坏明确报错。

工具说明和 schema 在 `apps/server/prompts/communication_tools.*`；模型循环最多 12 轮，最后一轮关闭工具并要求说明未覆盖范围，总运行时间仍限制为 120 秒。工具选择及汇总语义依赖真实模型；分页协议测试不替代供应商验收。

机器人消息入口本身不会自动收集你与其他人的聊天。启用下面的连接后，管理员在“飞书沟通资料”页面授权本人账号，连接后默认自动订阅飞书明确标记的私聊，群聊仍需手动勾选；加载候选本身不产生订阅。私聊后台按页发现，完成一轮后约十分钟复查；失败五分钟后重试。新连接从授权时刻、已有连接从本次迁移时刻采集，不自动回溯此前历史。已有订阅的时间范围及启停状态保持不变，已暂停或移除的私聊不会被自动恢复；移除全部只排除当时所选会话，之后新发现的私聊仍自动加入。候选支持名称搜索、跨分页多选及日期筛选：按北京时间查询该范围内是否存在消息，最多检查 10,000 个会话，可中途停止；失败项明确提示，不能当成没有消息。日期筛选只影响候选列表，不自动导入历史。

采集范围固定为：单聊全部消息；群聊（包含话题群）仅本人发送、明确 @ 本人、或直接回复本人消息。使用授权账号的 open_id 和父消息发送者核对，不根据昵称或模型推测关联，也不把 @所有人算成个人关联。父消息不在当前页时单独读取，无法核对时保留重试，不扩大范围。历史导入通过「新建导入」展开设置，默认近半年，提供近 7 / 30 / 90 / 180 天快捷选择，支持全部启用会话或多选会话与自定北京时间日期，单次最多 366 天。授权账号必须位于 `FEISHU_ALLOWED_USERS` 白名单。

页面按 `/communications/records`、`/communications/sync`、`/communications/sources` 分区；资料详情位于 `/communications/records/:id`，支持直接打开、刷新和浏览器返回，旧查询参数链接会自动跳转。

旧资料会逐日重新核对个人关联范围并提取卡片文字；完成前不参与摘要、图片解读和检索。升级启动时一次性使旧资料关联的提醒及推理上下文失效，聊天记录仍可查看；之后的逐日重处理不会清理升级后新建的聊天上下文。暂停来源也会暂停旧资料重处理，恢复后继续；暂停前的在途响应不能写回。上游读取失败会保留旧快照并稍后重试。

历史任务可逐项或全部取消，停止后续拉取与每日复查；已导入资料保留并继续整理，新消息订阅不受影响。同范围重新提交会从首分页重放，按消息 ID 去重。任务版本阻止取消前的在途响应重新写入。更改默认范围不会修改已有的一年任务。

每个飞书消息页请求 50 条并持续读取后续分页，没有每群每天五条的限制。详情每页也显示 50 条，日文件超过 16 MiB 会明确报错而非静默截断。成员姓名只取上游返回值，按成员分页查找当前消息页的缺名真人（每次最多 20 页）；权限不足、已退群或超出查询边界时保留缺名状态。应用机器人缺少名称时明确标记，不从消息内容猜测真人身份。

- **卡片**：提取交互卡片的标题、文字、Markdown 和布局中的可见正文；不执行按钮、不读取回调参数或模板变量作为正文。纯动作、无可提取文字的卡片明确标注，不能误称为正在排队解析。
- **采集**：独立 worker 以十分钟为同步间隔目标，实际整轮耗时随会话数量和上游延迟增加；有积压时以最多每秒五页推进，空会话不额外查成员，固定窗口分页，只有完整分页后才推进水位。失败约 5 分钟后从当前窗口重放，按消息 ID 去重；十分钟重叠窗口处理边界消息，每日复查已选择时间范围。根据飞书返回的编辑内容或撤回墓碑更新原文，不根据“本次没返回”推断消息被删除。撤回检测有轮询延迟，平台不再返回的记录不能保证自动发现，可主动遗忘整个来源。历史任务有独立游标，不推进增量水位；完成后每日复查该选定窗口，不扩大到未选择的历史日期。暂不展开线程回复、合并转发，不解析文件和语音。
- **本地文件**：正文保存在 `MEMORY_DIR/_communications/<source UUID>/<document UUID>/<hash>.jsonl`，按北京时间自然日归档。旧 UTC 文件由后台完整读取、重分组并事务切换；未变化的资料保留版本，跨日变动的摘要与索引重新生成。每条保留消息/会话 ID、发送者及 ID 类型、毫秒时间、文本、受限内容元数据和本人标记。内容寻址快照先原子落盘，再提交数据库指针；旧版本在提交后回收。摘要为同目录 Markdown，含结构化引文。单消息最多 128 KiB、单日文件最多 16 MiB，超过会报告错误而不静默跳过。数据库指针及文件需配套备份，崩溃留下的未引用文件不会参与检索。
- **图片**：保存消息中的资源键和需管理员登录的查看原图入口，不公开授权链接、不持久化图片二进制。服务端通过用户令牌获取可访问图片，以 base64 图片内容块交给当前 `OPENAI_MODEL`，无需独立 OCR；模型须支持 Chat Completions 的 `image_url`。线上 `deepseek-flash` 支持该能力。图片机器解读单独保存，参与 BM25 和摘要向量检索，不冒充发送者原话。按消息版本复用解读；失败五分钟后重试，不阻塞文字流程。支持 PNG/JPEG/WebP/GIF，单图最多 8 MiB，每条消息前 20 张自动解读，其他图片仍保留查看入口。飞书权限不足、撤回或不支持的格式会明确失败。
- **整理**：独立模型按块归纳决定、本人的承诺、对方的承诺、待确认事项与长期信息候选。逐字验证引用和发送者归属后才保存；出处匹配不等于语义准确。未解析附件或超长文本有覆盖统计。候选不会自动进入个人事实记忆，也不会自行触发通知。
- **检索**：中文 BM25 检索原始文本与图片解读，pgvector 检索摘要，使用既有 embedding 配置和同一个 PostgreSQL。向量使用独立命名空间，保存前与召回后验证版本。未配置向量或供应商故障时 BM25 继续可用。目前面向个人规模，每次从有效文件重建词法索引；大量长期聊天应再增加持久索引与分层归档。会话列表支持按名称搜索及每页 25 个展示，处理统计只计算有资料的来源，避免大量空会话产生逐个数据库查询。历史导入分区展示任务进度，每 5 秒读取全部任务计数和最近推进时间，分别展示拉取与资料处理状态。任务列表支持搜索、状态筛选与展开详情，筛选范围是服务端优先返回的最多 20 项；单项或全部取消均需弹窗确认，保留已有资料和新消息订阅。资料详情通过独立页面查看，读取失败可重试。分页计数从进度功能上线起累计，包含重放和每日复查，不代表唯一消息数。页面展示拉取、整理、索引和图片处理统计；回答使用姓名、时间及查看原文链接，内部 ID 只作关联。资料作为外部证据附加给对话模型，只有管理员及 OAuth 验证的同一飞书账号可召回；这不合并两个身份的个人记忆、历史会话或提醒。
- **跟进与遗忘**：在条目旁选择“据此安排提醒”，修正事项并确认时间后，进入现有提醒/回访调度；回访仍需事先开启。来源版本、原文或摘要变化会停止依赖它的旧事项。暂停来源后停止采集和召回；遗忘删除来源文件、图片解读、历史任务、摘要与向量；断开还删除本地凭证。修正/遗忘会保守停止关联身份旧的推理上下文，已发送聊天记录仍可查看，已开始的外部投递无法撤回。飞书侧应用授权可在飞书设置里另外撤销。

启用步骤：

1. 保持飞书机器人和本地记忆配置，在飞书应用中申请用户身份的 `im:chat:read`、`im:message:readonly`、`im:message.group_msg:get_as_user`、`im:message.p2p_msg:get_as_user`，授权时请求 `offline_access` 以刷新令牌。权限需按租户审批/发布生效，实际可读范围取决于平台授权。
2. 在应用安全设置登记精确回调地址 `https://api-orbit.ethankit.com/api/communications/oauth/callback`，本地开发使用对应的 `API_PUBLIC_URL`。
3. 设置 `FEISHU_SYNC_ENABLED=true` 和独立的 `FEISHU_TOKEN_KEY`（`openssl rand -hex 32`），重启服务。令牌以 AES-256-GCM 加密入库，不写入文件或浏览器存储；丢失/更换密钥需要重新授权。OAuth state 绑定发起会话及短期 HttpOnly 回调 Cookie，一次使用。
4. 以管理员打开“飞书沟通资料”，连接本人账号后手动勾选会话订阅新增沟通；在“整理历史消息”里选择会话及日期补录。可在来源列表暂停、移除，并核对本人标记、原文、摘要、图片及处理进度。

协议依据：[飞书官方 CLI 用户消息读取说明](https://github.com/larksuite/cli/blob/main/skills/lark-im/references/lark-im-chat-messages-list.md)、[会话列表实现](https://github.com/larksuite/cli/blob/main/shortcuts/im/im_chat_list.go)、[用户令牌刷新实现](https://github.com/larksuite/cli/blob/main/internal/auth/uat_client.go)、[DeepSeek 图片输入说明](https://api-docs.deepseek.com/guides/vision/)及[官方 OAuth 文档](https://open.feishu.cn/document/authentication-management/access-token/obtain-oauth-code)。本地测试验证协议、持久化和权限边界；真实租户授权、消息类型覆盖及模型归纳质量需要在配置应用后验收。

## Monorepo

```text
apps/
  server/                  # Axum API、认证、飞书、持久化 worker
    migrations/            # SQLx PostgreSQL 迁移
    src/sql/               # 复杂队列领取和限流 SQL
    tests/                 # 使用真实数据库的隔离集成测试
  web/                     # 独立部署的 Vite + React + TypeScript
    Dockerfile             # Nginx 静态服务，不依赖 Rust 构建
    deploy/                # SPA 路由回退及公开运行配置
crates/
  agent-runtime/           # 独立执行器与工具白名单
    prompts/              # 独立维护对话、摘要和记忆提取提示词
Dockerfile                 # 仅 Rust API 的非 root 运行镜像
compose.yaml               # 仅供本地开发的 PostgreSQL
```

### 前端模块

`apps/web/src/main.tsx` 清除地址中的临时 token、迁移旧链接并挂载 React Router；`App.tsx` 管理身份、共享布局和全局错误。页面与会话选择以 URL 为准，业务数据由各功能模块维护。

```text
apps/web/src/
  config.ts                # 公开 API 源配置，不包含凭据
  api.ts                   # 跨源 Cookie 请求与业务错误提示
  types.ts                 # 服务端响应类型
  components/              # 品牌图形、加载指示、空状态容器
  layout/                  # 侧栏、顶栏、路径和 URL 导航
  features/
    auth/                  # 管理员登录表单
    chat/                  # 聊天页、消息列表、输入框与 useChat
    links/                 # 授权创建、撤销与链接列表
    status/                # 运行状态快照
    memory/                # 记忆搜索、修正和遗忘
    followups/             # 提醒、回访偏好和通知
    communications/        # 飞书用户授权、来源选择、资料核对和检索
  lib/                     # 共用日期展示方法
  styles.css               # 全局样式与响应式规则
```

功能模块自行维护局部状态；聊天的轮询、草稿和幂等重试集中在 `useChat`，消息渲染与输入交互由独立组件负责。侧栏使用可复制和新标签打开的路由链接，不直接请求 API。页面按需加载，加载或渲染失败时保留布局并提供重新加载入口。前端只根据身份控制入口显示，实际授权始终由服务端校验。

## 本地运行

本地 Compose 在原有 `postgres:17-alpine` 基础构建 pgvector 0.8.6，继续使用原来的数据卷，不切换 libc 或 PostgreSQL 大版本。已有数据库升级前应备份；不要运行 `docker compose down -v`。

需要 rustup、Node.js 24 LTS（最低 22.12）和 Docker。`rust-toolchain.toml` 自动选择 Rust 1.98.1，并安装 rustfmt、Clippy；本地、CI 与 Docker 使用同一 Rust 版本。已有 `.env` 时请保留，手动补充缺少的变量，不要覆盖实际凭据。

```sh
# 获取发布版本
git clone https://github.com/ethanyu-dev/loops-orbit.git
cd loops-orbit
git checkout v0.1.0

# 仅首次、且没有 .env 时执行
cp .env.example .env

# 将输出填入 .env 的 ADMIN_TOKEN
openssl rand -hex 32

# 在 .env 填写模型配置后启动数据库
docker compose up -d --build postgres
npm ci

# 终端一：Rust 从根目录的 .env 读取配置，并自动执行数据库迁移
cargo run -p orbit-server

# 终端二：开发网页，默认 http://localhost:5173
npm run dev
```

开发时打开 `http://localhost:5173`，使用 `.env` 中的 `ADMIN_TOKEN` 登录。`PUBLIC_URL=http://localhost:5173` 是浏览器唯一可信源，`API_PUBLIC_URL=http://localhost:8080` 是 API 与 OAuth 回调源。Vite 直接请求独立 Rust 服务，开发阶段也验证 CORS 和 Cookie。`localhost` 与 `127.0.0.1` 不能混用；如修改端口，同步调整相应配置并重启服务。已有 `.env` 需要补充 `API_PUBLIC_URL`，旧的 `WEB_DIST` 已不再使用。

前端本地 API 地址默认 `http://localhost:8080`，可在 `apps/web/.env.local` 设置 `VITE_API_ORIGIN` 覆盖；它是公开配置，禁止写入任何 token。生产容器通过 `API_ORIGIN` 运行时注入地址，无需重新构建。Rust 启动和镜像构建都不需要 Node 或前端产物。

构建产物可独立预览：

```sh
npm run build
# 此时 Rust 的 PUBLIC_URL 也要改为 http://localhost:4173
npm run preview --workspace @orbit/web
```

前端仍是一套 React SPA，但功能页具有独立地址：`/chat`、`/chat/:conversationId`、`/memory`、`/followups`、`/communications`、`/links`、`/status`。路由支持直接打开、刷新和浏览器前进后退；资料详情用 `/communications?communication=<UUID>`。旧的 `/#chat=<UUID>` 和 `/?communication=<UUID>` 自动迁移。管理员页对访客显示权限提示，未知路由显示不存在页面；服务端仍独立验证每次 API 请求的权限。

不要把 `.env` 或任何真实 token 提交到仓库。

## 配置

| 变量 | 说明 |
| --- | --- |
| `DATABASE_URL` | 必填，PostgreSQL 连接串；本地示例使用 55433 端口 |
| `ADMIN_TOKEN` | 必填，至少 32 字节；推荐随机生成 64 个十六进制字符 |
| `PUBLIC_URL` | 必填，前端唯一可信源及访客链接地址，例如 `https://orbit.ethankit.com` |
| `API_PUBLIC_URL` | 必填，Rust API 的公开源；生产使用 `https://api-orbit.ethankit.com`，OAuth 回调使用此地址 |
| `API_ORIGIN` | 仅前端容器：浏览器访问的 API 源，默认 `https://api-orbit.ethankit.com`；同时用于公开运行配置和 CSP |
| `VITE_API_ORIGIN` | 仅前端本地开发/自定义静态构建：覆盖默认本地 API 地址；构建时注入，官方生产镜像使用运行时 `API_ORIGIN` |
| `OPENAI_BASE_URL` | 必填，API 根路径，例如 `https://api.openai.com/v1`；程序追加 `/chat/completions` |
| `OPENAI_MODEL` | 必填，代理实际支持的模型名称 |
| `OPENAI_API_KEY` | 必填，只存在于服务端 |
| `AGENT_STREAM_ENABLED` | 默认 `true`；仅支持非流式请求的代理可设为 `false` |
| `AGENT_TOOLS_ENABLED` | 默认 `true`；不支持工具协议的代理可设为 `false` |
| `PORT` | 默认 `8080`；部署时使用 Railway 注入值 |
| `WORKER_CONCURRENCY` | 每实例并发任务数，默认 `2`，范围 `1–16` |
| `MEMORY_ENABLED` | 默认 `true`；关闭后不读取或自动写入记忆 |
| `MEMORY_DIR` | 默认 `memory`，容器 `/app/memory`；生产必须挂载持久卷，UID 10001 需要写权限 |
| `MEMORY_AUTO_EXTRACT` | 默认 `true`；关闭后仅手工记录，自动提取消耗额外模型调用 |
| `EMBEDDING_BASE_URL` / `EMBEDDING_API_KEY` | 独立 embedding 地址和密钥；程序追加 `/embeddings` |
| `EMBEDDING_MODEL` / `EMBEDDING_DIMENSIONS` | 模型名与实际输出维度；配置模型时必须完整配置其他 embedding 变量，维度只验证、不发送给供应商 |
| `EMBEDDING_REVISION` | 默认 `1`；同名模型升级时递增，触发向量重建 |
| `MEMORY_MIN_SIMILARITY` | 默认 `0.55`，语义余弦相似度下限，需按实际语料校准 |
| `FOLLOWUP_TIMEZONE` | 默认 `Asia/Shanghai`；新身份的提醒时区，之后可在页面修改 |
| `FEISHU_SYNC_ENABLED` | 默认 `false`；启用飞书用户授权采集，要求配置飞书及本地记忆 |
| `FEISHU_TOKEN_KEY` | 采集启用时必填，64 位十六进制独立密钥；需与数据库及文件一同安全备份 |
| `RUST_LOG` | 默认启用应用与 runtime 的 info 级别 JSON 日志 |

模型请求不跟随重定向；如果代理返回重定向，请填写最终 API 根路径。仅更换 model、base URL、API key 无需修改代码，但需要重启服务。上游应支持 `messages`、`max_tokens` 与非流式摘要请求；流式回复采用 Chat Completions SSE 协议，返回完整 JSON 的代理也能接收。若代理拒绝 `stream: true`，设置 `AGENT_STREAM_ENABLED=false`。开启工具时还需支持 `tools` 和 `tool_calls`。

## 飞书接入

1. 在飞书开放平台创建企业自建应用，开启机器人能力，将自己加入应用可用范围。
2. 配置接收单聊消息和发送消息所需权限（通常为 `im:message.p2p_msg:readonly`、`im:message:send_as_bot`），订阅 `im.message.receive_v1`。以平台应用控制台要求的权限为准，发布应用版本。
3. 为服务设置以下变量，`FEISHU_ALLOWED_USERS` 使用 **open_id**，多用户以英文逗号分隔。启用飞书时五项都必须提供：

```dotenv
FEISHU_APP_ID=cli_xxx
FEISHU_APP_SECRET=...
FEISHU_VERIFICATION_TOKEN=...
FEISHU_ENCRYPT_KEY=...
FEISHU_ALLOWED_USERS=ou_xxx
```

4. 在事件订阅中选择发送至开发者服务器，配置 Encrypt Key，并将请求地址设为 `https://api-orbit.ethankit.com/api/channels/feishu/events`。正式事件校验原始请求签名、五分钟时间窗口、解密后的 verification token、应用 ID 和用户白名单。URL challenge 兼容无签名请求，但必须通过 verification token 校验且不会入队。
5. 完成 URL challenge 校验后，向机器人发送单聊文本。回调仅写入任务队列，模型执行和回复发送在后台完成；控制台可查看对应会话及运行结果。

协议实现参考飞书官方 [事件处理](https://github.com/larksuite/node-sdk/blob/main/dispatcher/request-handle.ts)、[AES 解密](https://github.com/larksuite/node-sdk/blob/main/utils/aes-cipher.ts) 和 [回复消息接口](https://open.feishu.cn/document/server-docs/im-v1/message/reply)。没有飞书凭据时入口关闭，其余功能照常运行。

飞书资料页的处理统计由后台分批核对文件和索引，页面只读取数据库快照，不在每次刷新时扫描全部原文。新增或变更资料在复核前显示“统计更新中”；统计可能稍有延迟，读取失败时保留上次结果并支持重试，后台采集独立运行。

## Railway 部署

同一仓库创建两个服务，构建上下文均为仓库根目录。前端与 API 使用不同镜像、变量和健康检查；API 的发布监听排除前端文件，前端发布无需重建 Rust。下表设置分别保存在 Railway 的各服务中。不要添加仓库根目录的 `railway.toml` 或 `railway.json`：旧版 Config as Code 已弃用，根配置还会覆盖同仓库其他服务的构建与健康检查。下表的前端域名为示例，可替换为实际使用的 `ethankit.com` 子域名。

| 设置 | 前端服务 | Rust API 服务 |
| --- | --- | --- |
| 自定义域名 | `orbit.ethankit.com` | `api-orbit.ethankit.com` |
| Dockerfile | `apps/web/Dockerfile` | `Dockerfile` |
| 构建变量 | `RAILWAY_DOCKERFILE_PATH=apps/web/Dockerfile` | `RAILWAY_DOCKERFILE_PATH=Dockerfile` |
| 变更监听 | `apps/web/**`、`package.json`、`package-lock.json` | `apps/server/**`、`crates/**`、`Cargo.toml`、`Cargo.lock`、`rust-toolchain.toml`、`Dockerfile` |
| 公开地址变量 | `API_ORIGIN=https://api-orbit.ethankit.com` | `PUBLIC_URL=https://orbit.ethankit.com`、`API_PUBLIC_URL=https://api-orbit.ethankit.com` |
| 健康检查 | `/health/live`，只检查静态服务 | `/health/ready`，检查数据库 |
| 健康检查超时 | 30 秒 | 120 秒 |
| 重启策略 | `ON_FAILURE`，最多 10 次 | `ON_FAILURE`，最多 10 次 |
| 持久卷 | 无 | `/app/memory`，UID 10001 可写 |

1. 添加支持 pgvector 的 PostgreSQL 服务（仅 BM25 时可用普通 PostgreSQL）。仅在 API 服务中配置 `DATABASE_URL=${{Postgres.DATABASE_URL}}`（按实际服务名调整）、随机 `ADMIN_TOKEN` 和三个 `OPENAI_*` 变量；不要将密钥复制到前端服务。
2. 为两个服务分别绑定域名并启用 HTTPS，填入上表地址变量。两个服务分别使用平台注入的 `PORT`。本项目继续使用 `SameSite=Strict`，前后端必须使用同一站点的 HTTPS 子域名；不要将临时 Railway 域名与 `ethankit.com` API 混用。
3. API 配置 **一个副本**，挂载持久卷并安排旧实例退出后再启动新实例。配置 embedding 时，数据库用户需有安装 `vector` 扩展的权限，或由管理员预先安装；启动自动迁移数据库。前端可以独立重启和扩容。
4. 飞书 OAuth 改为 `https://api-orbit.ethankit.com/api/communications/oauth/callback`，机器人事件回调改为 `https://api-orbit.ethankit.com/api/channels/feishu/events`。授权完成后 API 跳回前端 `/communications`。API 域名变化后原主机的登录 Cookie 不会迁移，需要重新登录。
5. 从原单服务切换时，先准备前端服务与 API 域名，再更新后端配置和飞书回调、切换前端域名。最终验收跨域登录/退出、访客链接、功能页刷新与返回、飞书重新授权。开启数据库和记忆目录备份并演练恢复。

前端 Nginx 对 HTML 和运行配置禁用缓存，对带哈希的构建资源长期缓存；深层路由回退到前端入口，缺失静态资源和误发到前端的 `/api/*` 返回 404。Rust 所有未知路径返回 JSON 404，不再回落到网页。前端健康检查独立，因此后端重启期间页面仍可访问，但聊天和资料请求会暂时失败。

本地分别构建：

```sh
docker build -t orbit-api:local .
docker build -f apps/web/Dockerfile -t orbit-web:local .
# 已有本地 Rust API 时，浏览器继续使用 localhost 的同站点 Cookie。
docker run --rm -p 5173:8080 -e API_ORIGIN=http://localhost:8080 orbit-web:local
```

部署配置依据 Railway [Dockerfile 构建](https://docs.railway.com/builds/dockerfiles)、[旧版配置弃用说明](https://docs.railway.com/config-as-code)与[健康检查](https://docs.railway.com/deployments/healthchecks)。跨域配置使用 [tower-http CORS](https://docs.rs/tower-http/latest/tower_http/cors/struct.CorsLayer.html)，静态回退使用 [Nginx try_files](https://nginx.org/en/docs/http/ngx_http_core_module.html#try_files)。Railway 部署健康检查不替代持续监控，请另设外部探测。

### 可用性边界

- 启用文件记忆时仅支持一个应用实例，并使用目录文件锁和 PostgreSQL 会话级 advisory lock 拒绝第二个写入者；持锁连接断开后实例终止。关闭 `MEMORY_ENABLED` 后，会话与队列仍支持共享 PostgreSQL 的多副本。
- worker 通过 `FOR UPDATE SKIP LOCKED` 领取任务，同会话只处理当前有效执行，不同会话并行。被后续消息替换的旧执行失去租约，任何片段、完成结果和失败写回都必须匹配有效租约。
- 单次 agent 运行最多 120 秒，租约为 180 秒。实例崩溃后，其他 worker 可以在租约到期后重新领取；写回必须匹配新的租约令牌，旧 worker 无法覆盖结果。
- 连接/读取故障、429、5xx 进行最多三次有界执行尝试；永久协议错误直接失败。工具循环最多十二轮（最后一轮保留给回答），提醒工具通过服务端宿主验证真实身份、用户原文证据和运行租约；写入动作与操作结果同事务保存。
- 飞书回调通过事件 ID 和消息 ID 去重。回复保存在独立 outbox 中，最多尝试五次，发送时使用稳定 UUID。
- 数据库提交保证一次结果入库；上游调用采用至少一次执行语义。进程在模型调用成功、结果落库前崩溃，恢复时可能再次调用和计费。飞书最终去重范围由平台 UUID 机制决定，不承诺跨任意故障的端到端 exactly-once。
- SIGTERM 后停止领取新任务并等待当前执行完成，最长等待 130 秒；若平台更早强制终止，仍通过租约恢复。
- **文件记忆方案不提供应用多副本 HA。** 数据库和持久卷是关键依赖，生产需要匹配自己的 HA、备份和恢复方案。跨地域灾备、自动模型切换、跨地域压测未包含在首版中。

### 可观测性

- `/health/live`：进程活性，不依赖数据库。
- `/health/ready`：数据库就绪；模型供应商故障不触发所有应用实例重启。
- `/api/admin/status`：管理员控制台使用的全局任务、飞书投递状态与运行配置。
- `/metrics`：Prometheus 文本，需 `Authorization: Bearer <ADMIN_TOKEN>`；`orbit_runs` 与 `orbit_deliveries` 按状态统计数据库中的全局记录数量。多个副本会读取同一计数，采集时不要对副本求和。
- JSON 日志包含 HTTP request_id、状态、耗时，以及 run_id、conversation_id、执行次数和错误分类；不会记录 token、Cookie、API key、对话正文或供应商错误响应体。

建议对就绪失败、队列持续增长、失败任务增长和飞书发送失败设置外部告警。失败任务和完整对话可在控制台定位；首版不自动删除对话，数据保留与容量需由部署者管理。

## Linear 连接与 issue 工具

管理员从工作空间菜单打开「外部连接」，通过 Linear OAuth 绑定自己的账号。部署前在 Linear 创建 OAuth 应用，回调地址填写 `API_PUBLIC_URL/api/linear/oauth/callback`，例如本地 `http://localhost:8080/api/linear/oauth/callback`。服务端配置：

```dotenv
LINEAR_CLIENT_ID=你的应用ID
LINEAR_CLIENT_SECRET=你的应用密钥
LINEAR_TOKEN_KEY=独立生成的64位十六进制密钥
LINEAR_WORKSPACE_SLUG=pplabs
```

`LINEAR_TOKEN_KEY` 可使用 `openssl rand -hex 32` 生成，必须与数据库一起妥善保管；直接替换会使已有连接无法解密，需要重新授权。`LINEAR_WORKSPACE_SLUG` 可留空；配置为 `pplabs` 时拒绝连接其他工作空间。不设置 `LINEAR_CLIENT_ID` 则禁用整个能力。数据库迁移 `0015_linear.sql` 随 API 启动执行。前后端生产地址继续遵守前述同站点部署要求。

默认申请 read；勾选「允许按我的明确指令更新 issues」才申请 write，最终以供应商实际授予的 scopes 为准。连接绑定实际 Linear 用户和工作空间，只对网页管理员开放，访客、飞书聊天和后台任务不继承此授权。切换账号或工作空间前需断开旧连接。OAuth state 同时绑定浏览器、原管理员会话和 PKCE，凭证以 AES-GCM 加密保存；刷新通过数据库行锁串行处理。断开禁用后续工具调用并尝试向供应商撤销令牌，不删除历史回答，也不能撤回已派发的修改。

连接后可直接问「分配给我的 issues 有哪些」，或明确要求「把 ENG-123 的优先级改为高」。四个工具通过渐进式目录发现、加载，不会在每轮默认发送全部定义：

| 工具 | 能力与边界 |
| --- | --- |
| `linear_issue_list` | 默认分配给绑定账号本人；团队、状态和标题字面筛选，最多 50 条一页，返回 `has_more` 和游标。 |
| `linear_issue_get` | 读取详情和更新时间，长描述每次最多 6000 字符，续读需要相同版本。 |
| `linear_team_metadata` | 分页查询团队、状态或成员，获取修改需要的真实 ID。 |
| `linear_issue_update` | 修改标题、完整描述、优先级、状态或负责人；省略字段保持原样，负责人 null 表示清空。 |

本人列表对应授权账号的 assignee 条件，不复刻 Linear 网页保存的筛选和排序偏好。首版不支持创建、删除、评论或任意 GraphQL。提示词要求明确用户指令，服务端校验真实输入片段、当前任务租约、连接代次、权限和字段白名单；片段校验不能独立证明自然语言授权语义。更新前对比 `updatedAt` 仅为尽力冲突检测，不能保证外部原子条件更新。

每次更新派发前保存操作记录，同一任务、连接代次、issue 和补丁不会重复发送。平台响应丢失或进程中断时返回 `unknown`，不自动重试写入；只有验证成功响应才返回 `confirmed`。再次读取可以确认当前状态，但不能证明此前请求是否执行成功。网络调用有超时及响应体上限，HTTP 200 的 GraphQL errors 同样按失败处理，HTTP 400 的 `RATELIMITED` 按限流处理。

协议依据 Linear 官方 [OAuth 文档](https://linear.app/developers/oauth-2-0-authentication)、[GraphQL 文档](https://linear.app/developers/graphql)和[限流说明](https://linear.app/developers/rate-limiting)。集成测试使用真实 PostgreSQL 与本地 OAuth/GraphQL 夹具，覆盖浏览器绑定、回调重放、刷新串行化与失效、凭证篡改、分页、更新去重、未知结果和断开期间的响应围栏；不代表真实 Linear 账号、授权页或模型语义验收。

## 认证与临时链接

管理员 token 只用于兑换随机会话；浏览器通过 API 主机专属的 `HttpOnly; SameSite=Strict` Cookie 访问 API，HTTPS 环境附带 `Secure`；Cookie 不设置父域 `Domain`。前端请求显式使用 `credentials: include`，后端仅对 `PUBLIC_URL` 返回带凭据的 CORS 响应。管理员会话最长七天，根 token 轮换后旧管理员会话立即失效。

链接格式为 `https://域名/#token=...`，fragment 不会作为 HTTP URL 发给服务器。网页加载时先清除地址中的 token，再通过 POST 兑换会话。数据库仅保存 SHA-256 摘要；链接原文只在创建成功时显示一次。

每次请求都检查授权到期和撤销状态。访客 Cookie 有效期不超过链接期限和七天中的较短者；链接有效期超过七天时可重新打开链接登录。撤销阻止后续访问，但已入队的模型任务仍可能执行完成；撤销不会删除历史。

浏览器写操作必须携带与 `PUBLIC_URL` 完全一致的 Origin。认证端点共享每分钟 30 次数据库配额；消息和会话创建按身份限流。脚本调试 API 时也需要设置 Origin。

## 格式、Lint 与依赖维护

Rust 使用官方 [rustfmt 与 Clippy](https://rust-lang.org/tools/)。`rustfmt.toml` 只使用 stable 配置，统一 Rust 2024 风格及 100 列排版。Clippy 启用默认 `all` 规则，并禁止遗留 `dbg!`、`todo!`、`unimplemented!`；CI 将所有警告视为错误。两个 crate 通过 workspace 继承规则，同时禁止本项目编写 unsafe 代码。没有整体启用 `pedantic` 或 `restriction`，避免为风格偏好引入大量无效告警。

```sh
npm run format:rust        # cargo fmt --all
npm run format:rust:check  # 仅检查 Rust 排版
npm run lint:rust          # 全 workspace、所有 target/feature，包含测试代码
npm run format            # 前端 Prettier
```

rustfmt 不负责整理 SQL 字符串和自定义宏内部结构。复杂 SQL 放在 `src/sql/`，通过 `include_str!` 引入；JSON 和 tracing/select 宏手动分行。优先使用命名响应结构体和职责单一的方法，减少位置元组、深层嵌套及一行承载多个操作。

v0.1.0 的主要构建版本如下，完整版本以发布标签内的锁文件为准：

| 组件 | 当前版本 |
| --- | --- |
| Rust / Edition | 1.98.1 / 2024 |
| Axum / Tokio | 0.8.9 / 1.53.1 |
| SQLx / reqwest | 0.9.0 / 0.13.5 |
| tower-http | 0.7.1 |
| React / React DOM | 19.3.0 |
| Vite / React 插件 | 8.3.1 / 6.1.1 |
| TypeScript | 7.0.2 |
| Prettier | 3.9.9 |

Rust 直接依赖统一定义在根 `Cargo.toml`；`Cargo.lock`、`package-lock.json` 固定实际构建版本。传递依赖遵循上游兼容约束，不强行替换其 major 版本。PostgreSQL 保留 17，数据库大版本升级需单独安排数据迁移。后续升级应重新查询注册表、阅读迁移说明、更新锁文件并执行下面的检查；`cargo update` 本身只更新已有兼容范围，不能证明所有依赖都是最新稳定版。

## 验证

```sh
npm ci
npm run build
npm run test:web
npm run format:check
npm run format:rust:check
npm run lint:rust
cargo test --workspace --locked

# 必须显式提供测试数据库；每个 case 使用独立 schema，结束后清理。
# 建议使用独立测试数据库，不复用个人聊天数据库。
TEST_DATABASE_URL=postgres://orbit:orbit_test@localhost:55434/orbit_test npm run test:integration

docker build -t orbit-api:local .
docker build -f apps/web/Dockerfile -t orbit-web:local .
```

单元测试验证工具白名单、参数校验、本地 HTTP 工具往返，以及 SSE 跨字节分片、工具参数拼接和断流拒绝。集成测试使用真实 PostgreSQL 和本地 HTTP 模型夹具，验证认证、授权过期/撤销、访客隔离、幂等、并发领取、租约恢复、连续输入与取消、流式快照恢复、摘要边界与失败恢复、会话隔离和飞书加密事件去重。新增记忆测试覆盖真实文件、中文 BM25、真实 pgvector、旧向量排除、模型版本/维度、抽取证据校验和遗忘围栏。跟进测试覆盖单次提醒、改期与取消、过期、租约恢复、通知已读、时区/静默/冷却、工具证据与来源、回访发现、在途用户输入和遗忘、文件直接编辑、身份隔离与授权撤销，以及本地飞书协议的稳定 UUID 重试。embedding、回访判断与提取输出使用夹具，不验证真实模型语义质量；这些测试也不等同于飞书或 Railway 生产验收。CI 运行编译、格式检查、Clippy 和上述测试。

首版共 4 个运行时测试、41 个数据库集成测试。沟通采集测试另外覆盖 OAuth 浏览器绑定与重放拒绝、刷新串行化、密文篡改、分页失败恢复、采集不产生聊天任务、本人及访客检索隔离、暂停的在途围栏、伪造摘要证据拒绝、来源修正取消提醒和断开清理。CI 还分别构建前端和 API 镜像。独立部署边界测试覆盖 CORS 预检与错误响应、来源拒绝、API 的 JSON 404；前端测试覆盖旧会话和资料地址迁移。这些测试不替代生产 DNS、TLS 和飞书租户的验收。
