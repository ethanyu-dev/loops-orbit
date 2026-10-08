use crate::{
    AppState,
    auth::Identity,
    error::{ApiError, ApiResult},
};
use axum::{
    Json,
    extract::{Query, State},
    http::StatusCode,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

// 日期目录和文件列表独立分页，不再受状态快照最近一百份资料的限制。
const DAY_PAGE_SIZE: usize = 60;
const FILE_PAGE_SIZE: i64 = 50;
const MAX_QUERY_LENGTH: usize = 2000;

/// 日期游标按已有北京时间日字符串比较，新日期加入不会挤动后续分页。
#[derive(Deserialize)]
pub(super) struct DayQuery {
    /// 仅返回此日期之前的目录；空值从最新目录开始。
    before: Option<String>,
}

/// 日期目录仅聚合元数据，不打开原文或持有采集锁。
#[derive(Serialize, sqlx::FromRow)]
pub(super) struct DayFolder {
    /// 来源已经按北京时间归档的自然日。
    day: String,
    /// 当天各个会话生成的资料数量。
    count: i64,
}

/// 日期按需加载；前端用最后一个日期继续读取较早目录。
#[derive(Serialize)]
pub(super) struct DayPage {
    /// 当前页日期，最新优先。
    items: Vec<DayFolder>,
    /// 有下一页时为本页最后一天。
    next_before: Option<String>,
}

/// 日期参数必须使用完整日历日期，避免错误游标或不一致的字符串排序。
fn validate_day(day: Option<&str>) -> ApiResult<()> {
    if let Some(day) = day
        && (day.len() != 10 || chrono::NaiveDate::parse_from_str(day, "%Y-%m-%d").is_err())
    {
        return Err(ApiError(
            StatusCode::BAD_REQUEST,
            "invalid_communication_library",
        ));
    }
    Ok(())
}

/// 独立目录分页允许读取全部历史，而无需把全部文件加载进浏览器。
pub(super) async fn days(
    State(state): State<AppState>,
    identity: Identity,
    Query(input): Query<DayQuery>,
) -> ApiResult<Json<DayPage>> {
    identity.require_admin()?;
    validate_day(input.before.as_deref())?;
    let mut items: Vec<DayFolder> =
        sqlx::query_as(include_str!("../sql/communication_library_days.sql"))
            .bind(&identity.owner)
            .bind(input.before)
            .bind((DAY_PAGE_SIZE + 1) as i64)
            .fetch_all(&state.pool)
            .await?;
    let more = items.len() > DAY_PAGE_SIZE;
    items.truncate(DAY_PAGE_SIZE);
    let next_before = more.then(|| items.last().expect("有下一页时本页非空").day.clone());
    Ok(Json(DayPage { items, next_before }))
}

/// 文件名称搜索遍历完整历史；正文检索继续由已有的身份隔离检索接口负责。
#[derive(Deserialize)]
pub(super) struct FileQuery {
    /// 选中的日期；为空时搜索全部日期。
    day: Option<String>,
    /// 按会话名称或日期进行字面子串查找，不解释 SQL 通配符。
    #[serde(default)]
    q: String,
    /// 从零开始的文件偏移。
    #[serde(default)]
    offset: u32,
}

/// 列表只暴露展示所需的字段，不返回原文文件路径和访问凭证。
#[derive(Serialize, sqlx::FromRow)]
pub(super) struct LibraryFile {
    /// 跳转现有详情页的稳定标识。
    id: Uuid,
    /// 所属群聊或联系人。
    source_id: Uuid,
    /// 文件展示名称取自订阅会话。
    source_label: String,
    /// 暂停的来源仍可浏览已有资料，但不参与内容检索。
    source_enabled: bool,
    /// 已移除的订阅仍可保留文件。
    source_subscribed: bool,
    /// 北京时间归档日期。
    day: String,
    /// 文件版本。
    version: i64,
    /// 为零时仍在核对旧资料。
    extraction_version: i64,
    /// 已生成整理内容的版本指纹。
    summary_hash: Option<String>,
    /// 整理故障分类，不包含原始上游响应。
    summary_error: Option<String>,
}

/// 文件分页数量由后端决定，避免客户端请求无界增长。
#[derive(Serialize)]
pub(super) struct FilePage {
    /// 当前页文件，日期倒序、会话名称正序。
    items: Vec<LibraryFile>,
    /// 当前日期和关键词下的总数。
    total: i64,
    /// 有下一页时的文件偏移。
    next_offset: Option<i64>,
}

/// 只查询已授权身份的元数据，搜索不会触发模型调用或飞书消息读取。
pub(super) async fn files(
    State(state): State<AppState>,
    identity: Identity,
    Query(input): Query<FileQuery>,
) -> ApiResult<Json<FilePage>> {
    identity.require_admin()?;
    validate_day(input.day.as_deref())?;
    if input.q.chars().count() > MAX_QUERY_LENGTH {
        return Err(ApiError(
            StatusCode::BAD_REQUEST,
            "invalid_communication_library",
        ));
    }
    let query = input.q.trim();
    // 总数和当前页使用同一个数据库快照，避免后台导入使翻页提示自相矛盾。
    let mut transaction = state.pool.begin().await?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY")
        .execute(&mut *transaction)
        .await?;
    let total: i64 = sqlx::query_scalar(include_str!("../sql/communication_library_count.sql"))
        .bind(&identity.owner)
        .bind(&input.day)
        .bind(query)
        .fetch_one(&mut *transaction)
        .await?;
    let items: Vec<LibraryFile> =
        sqlx::query_as(include_str!("../sql/communication_library_files.sql"))
            .bind(&identity.owner)
            .bind(&input.day)
            .bind(query)
            .bind(FILE_PAGE_SIZE)
            .bind(i64::from(input.offset))
            .fetch_all(&mut *transaction)
            .await?;
    transaction.commit().await?;
    let next = i64::from(input.offset) + items.len() as i64;
    Ok(Json(FilePage {
        items,
        total,
        next_offset: (next < total).then_some(next),
    }))
}
