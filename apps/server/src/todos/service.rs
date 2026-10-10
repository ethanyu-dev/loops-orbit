use super::{Content, Create, ScheduleInput, Source, TODO_COLUMNS, Todo, Update, identity};
use crate::{
    AppState,
    error::{ApiError, ApiResult},
};
use axum::http::StatusCode;
use chrono::{DateTime, Duration, Utc};
use serde_json::{Value, json};
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

/// 校验用户可编辑字段的预算，状态和身份另行验证。
fn validate(content: &Content) -> ApiResult<()> {
    if content.title.trim().is_empty()
        || content.title.chars().count() > 500
        || [
            &content.objective,
            &content.completion_criteria,
            &content.next_action,
            &content.waiting_on,
        ]
        .iter()
        .any(|v| v.chars().count() > super::MAX_TEXT)
    {
        return Err(ApiError(StatusCode::BAD_REQUEST, "invalid_todo"));
    }
    Ok(())
}
/// 查询分页限定本人空间，字面搜索不将百分号当通配授权。
pub async fn list(
    state: &AppState,
    actor: &str,
    query: &str,
    offset: i64,
    view: &str,
) -> ApiResult<Value> {
    identity::require_owner(state, actor).await?;
    if query.chars().count() > 500 || !(0..=10000).contains(&offset) {
        return Err(ApiError(StatusCode::BAD_REQUEST, "invalid_arguments"));
    }
    if !matches!(view, "active" | "scheduled" | "ended" | "all") {
        return Err(ApiError(StatusCode::BAD_REQUEST, "invalid_arguments"));
    }
    let filter = " AND ($3='all' OR ($3='active' AND status NOT IN('completed','cancelled')) OR ($3='ended' AND status IN('completed','cancelled')) OR ($3='scheduled' AND status NOT IN('completed','cancelled') AND EXISTS(SELECT 1 FROM todo_schedules s WHERE s.todo_id=todos.id AND (s.status='enabled' OR EXISTS(SELECT 1 FROM followups f WHERE f.todo_schedule_id=s.id AND f.status IN('scheduled','checking','queued'))))))";
    let rows: Vec<Todo> = sqlx::query_as(sqlx::AssertSqlSafe(format!("SELECT {TODO_COLUMNS} FROM todos WHERE owner='admin' AND ($1='' OR strpos(lower(title||' '||objective),lower($1))>0){filter} ORDER BY (status IN('completed','cancelled')),updated_at DESC,id LIMIT 50 OFFSET $2")))
        .bind(query).bind(offset).bind(view).fetch_all(&state.pool).await?;
    let total: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT count(*) FROM todos WHERE owner='admin' AND ($1='' OR strpos(lower(title||' '||objective),lower($1))>0) AND $2::bigint>=0{filter}")))
        .bind(query).bind(offset).bind(view).fetch_one(&state.pool).await?;
    Ok(json!({"items":rows,"total":total,"offset":offset,"has_more":offset+50<total}))
}
/// 详情只返回有界历史；关联资源不在此接口自动展开正文。
pub async fn detail(state: &AppState, actor: &str, id: Uuid) -> ApiResult<Value> {
    identity::require_owner(state, actor).await?;
    let item: Todo = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT {TODO_COLUMNS} FROM todos WHERE id=$1 AND owner='admin'"
    )))
    .bind(id)
    .fetch_optional(&state.pool)
    .await?
    .ok_or(ApiError(StatusCode::NOT_FOUND, "todo_not_found"))?;
    let schedules: Vec<Value> = sqlx::query_scalar("SELECT to_jsonb(s) || jsonb_build_object('channel',c.channel) FROM todo_schedules s JOIN conversations c ON c.id=s.conversation_id WHERE s.todo_id=$1 ORDER BY s.created_at,s.id")
        .bind(id).fetch_all(&state.pool).await?;
    let runs: Vec<Value> = sqlx::query_scalar("SELECT to_jsonb(r) FROM todo_runs r WHERE todo_id=$1 ORDER BY created_at DESC,id LIMIT 100")
        .bind(id).fetch_all(&state.pool).await?;
    let events: Vec<Value> = sqlx::query_scalar(
        "SELECT to_jsonb(e) FROM todo_events e WHERE todo_id=$1 ORDER BY seq DESC LIMIT 100",
    )
    .bind(id)
    .fetch_all(&state.pool)
    .await?;
    let links: Vec<Value> = sqlx::query_scalar(
        "SELECT to_jsonb(l) FROM todo_links l WHERE todo_id=$1 ORDER BY id LIMIT 100",
    )
    .bind(id)
    .fetch_all(&state.pool)
    .await?;
    Ok(json!({"item":item,"schedules":schedules,"runs":runs,"events":events,"links":links}))
}
/// 本人写操作串行检查配额和幂等；模型动作同时验证真实会话和租约。
async fn begin<'a>(
    state: &'a AppState,
    actor: &str,
    source: Option<&Source<'_>>,
) -> ApiResult<Transaction<'a, Postgres>> {
    identity::require_owner(state, actor).await?;
    let mut tx = state.pool.begin().await?;
    sqlx::query("SELECT pg_advisory_xact_lock(hashtext('orbit:todos:admin'))")
        .execute(&mut *tx)
        .await?;
    identity::lock(&mut tx, actor, source.and_then(|s| s.identity_version)).await?;
    if let Some(source) = source {
        sqlx::query("SELECT id FROM conversations WHERE id=$1 FOR UPDATE")
            .bind(source.job.conversation_id)
            .execute(&mut *tx)
            .await?;
        let active: bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM runs r JOIN conversations c ON c.id=r.conversation_id WHERE r.id=$1 AND r.lease_token=$2 AND r.status='running' AND c.owner=$3)")
            .bind(source.job.id).bind(source.job.lease_token).bind(actor).fetch_one(&mut *tx).await?;
        if !active {
            return Err(ApiError(StatusCode::CONFLICT, "run_superseded"));
        }
    }
    Ok(tx)
}
/// 幂等返回原始动作结果；同一个键不能悄悄提交不同内容。
async fn previous(
    tx: &mut Transaction<'_, Postgres>,
    key: &str,
    hash: &str,
) -> ApiResult<Option<Value>> {
    let old: Option<(String, Value)> = sqlx::query_as(
        "SELECT request_hash,result FROM todo_operations WHERE owner='admin' AND operation_key=$1",
    )
    .bind(key)
    .fetch_optional(&mut **tx)
    .await?;
    if let Some((old_hash, result)) = old {
        if old_hash != hash {
            return Err(ApiError(StatusCode::CONFLICT, "todo_idempotency_conflict"));
        }
        return Ok(Some(result));
    }
    Ok(None)
}
/// 结果与事件同事务提交，重试不新增第二个事项或安排。
async fn record(
    tx: &mut Transaction<'_, Postgres>,
    key: &str,
    hash: &str,
    result: &Value,
) -> ApiResult<()> {
    sqlx::query("INSERT INTO todo_operations(owner,operation_key,request_hash,result) VALUES('admin',$1,$2,$3)")
        .bind(key).bind(hash).bind(result).execute(&mut **tx).await?;
    Ok(())
}
/// 所有显式修改保留来源，不把自动判断伪装成用户确认。
pub async fn event(
    tx: &mut Transaction<'_, Postgres>,
    id: Uuid,
    actor: &str,
    kind: &str,
    detail: Value,
) -> ApiResult<()> {
    sqlx::query("INSERT INTO todo_events(todo_id,actor,kind,detail) VALUES($1,$2,$3,$4)")
        .bind(id)
        .bind(actor)
        .bind(kind)
        .bind(detail)
        .execute(&mut **tx)
        .await?;
    Ok(())
}
/// 创建事项与可选初始安排为一个原子动作。
pub async fn create(
    state: &AppState,
    actor: &str,
    input: &Create,
    source: Option<&Source<'_>>,
) -> ApiResult<Value> {
    validate(&input.content)?;
    let key = source
        .map(|s| format!("{}:{}", s.job.id, s.operation_key))
        .unwrap_or_else(|| input.idempotency_key.to_string());
    // 模型工具的 UUID 由宿主稳定生成，因此重放时哈希不随模型改变。
    let hash = crate::auth::hash(&serde_json::to_string(input).expect("可序列化待办"));
    let mut tx = begin(state, actor, source).await?;
    if let Some(result) = previous(&mut tx, &key, &hash).await? {
        return Ok(result);
    }
    let count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM todos WHERE owner='admin' AND status NOT IN('completed','cancelled')",
    )
    .fetch_one(&mut *tx)
    .await?;
    if count >= super::MAX_TODOS {
        return Err(ApiError(StatusCode::CONFLICT, "todo_limit"));
    }
    let id = Uuid::new_v4();
    let c = &input.content;
    sqlx::query("INSERT INTO todos(id,owner,title,objective,completion_criteria,next_action,waiting_on,due_at,origin_key,request_hash) VALUES($1,'admin',$2,$3,$4,$5,$6,$7,$8,$9)")
        .bind(id).bind(c.title.trim()).bind(&c.objective).bind(&c.completion_criteria).bind(&c.next_action).bind(&c.waiting_on).bind(c.due_at).bind(&key).bind(&hash).execute(&mut *tx).await?;
    if let Some(source) = source {
        sqlx::query(
            "INSERT INTO todo_links(id,todo_id,kind,resource_id) VALUES($1,$2,'conversation',$3)",
        )
        .bind(Uuid::new_v4())
        .bind(id)
        .bind(source.job.conversation_id.to_string())
        .execute(&mut *tx)
        .await?;
    }
    if let Some(schedule) = &input.schedule {
        insert_schedule(&mut tx, actor, id, schedule, source).await?;
    }
    let result = json!({"id":id,"version":1,"status":"active"});
    event(
        &mut tx,
        id,
        actor,
        "created",
        json!({"content":c,"source_run_id":source.map(|s|s.job.id)}),
    )
    .await?;
    record(&mut tx, &key, &hash, &result).await?;
    tx.commit().await?;
    Ok(result)
}
/// 完成/取消会停止全部未来安排；重新打开只改变业务状态。
pub async fn update(
    state: &AppState,
    actor: &str,
    id: Uuid,
    input: &Update,
    source: Option<&Source<'_>>,
) -> ApiResult<Value> {
    validate(&input.content)?;
    if !matches!(
        input.status.as_str(),
        "active" | "waiting_external" | "needs_user" | "completed" | "cancelled"
    ) {
        return Err(ApiError(StatusCode::BAD_REQUEST, "invalid_todo"));
    }
    let mut tx = begin(state, actor, source).await?;
    let key = source.map(|s| format!("{}:{}", s.job.id, s.operation_key));
    let hash = crate::auth::hash(&format!(
        "{id}:{}",
        serde_json::to_string(input).expect("更新可序列化")
    ));
    if let Some(key) = &key
        && let Some(result) = previous(&mut tx, key, &hash).await?
    {
        return Ok(result);
    }
    let c = &input.content;
    let changed=sqlx::query("UPDATE todos SET title=$3,objective=$4,completion_criteria=$5,next_action=$6,waiting_on=$7,due_at=$8,status=$9,version=version+1,updated_at=now(),closed_at=CASE WHEN $9 IN('completed','cancelled') THEN now() ELSE NULL END WHERE id=$1 AND owner='admin' AND version=$2")
        .bind(id).bind(input.version).bind(c.title.trim()).bind(&c.objective).bind(&c.completion_criteria).bind(&c.next_action).bind(&c.waiting_on).bind(c.due_at).bind(&input.status).execute(&mut *tx).await?;
    if changed.rows_affected() == 0 {
        return Err(ApiError(StatusCode::CONFLICT, "todo_version_conflict"));
    }
    let mut started = false;
    if matches!(input.status.as_str(), "completed" | "cancelled") {
        let ids:Vec<Uuid>=sqlx::query_scalar("UPDATE todo_schedules SET status='ended',version=version+1,updated_at=now() WHERE todo_id=$1 RETURNING id").bind(id).fetch_all(&mut *tx).await?;
        for schedule in ids {
            started |= cancel_pending(&mut tx, schedule).await?;
        }
    }
    event(&mut tx,id,actor,"updated",json!({"version":input.version+1,"status":input.status,"content":c,"source_run_id":source.map(|s|s.job.id)})).await?;
    let result = json!({"id":id,"version":input.version+1,"delivery_may_have_started":started});
    if let Some(key) = key {
        record(&mut tx, &key, &hash, &result).await?;
    }
    tx.commit().await?;
    Ok(result)
}
/// 停止尚未确认投递的期次；已经进入 HTTP 的请求不能撤回。
pub async fn cancel_pending(tx: &mut Transaction<'_, Postgres>, schedule: Uuid) -> ApiResult<bool> {
    let started:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM outbox o JOIN followups f ON f.id=o.followup_id WHERE f.todo_schedule_id=$1 AND o.dispatch_started_at IS NOT NULL)")
        .bind(schedule).fetch_one(&mut **tx).await?;
    sqlx::query("UPDATE outbox SET status='cancelled',lease_token=NULL WHERE followup_id IN(SELECT id FROM followups WHERE todo_schedule_id=$1) AND status IN('queued','running')")
        .bind(schedule).execute(&mut **tx).await?;
    sqlx::query("UPDATE followups SET status='cancelled',version=version+1,lease_token=NULL,error='todo_schedule_changed' WHERE todo_schedule_id=$1 AND status IN('scheduled','checking','queued')")
        .bind(schedule).execute(&mut **tx).await?;
    Ok(started)
}
/// 确认投递目标对应本人身份；模型不能传接收人或任意会话 ID。
async fn destination(
    tx: &mut Transaction<'_, Postgres>,
    actor: &str,
    channel: &str,
    source: Option<&Source<'_>>,
) -> ApiResult<(Uuid, String, Option<i64>)> {
    let (owner, version) = match channel {
        "web" => ("admin".to_owned(), None),
        "feishu" => {
            let rows: Vec<(String,i64)>=sqlx::query_as("SELECT owner,version FROM personal_identities WHERE enabled AND principal='admin' ORDER BY owner FOR SHARE").fetch_all(&mut **tx).await?;
            let chosen = rows
                .iter()
                .find(|(o, _)| o == actor)
                .or_else(|| if rows.len() == 1 { rows.first() } else { None });
            let (owner, version) = chosen.cloned().ok_or(ApiError(
                StatusCode::CONFLICT,
                "todo_feishu_identity_required",
            ))?;
            (owner, Some(version))
        }
        _ => return Err(ApiError(StatusCode::BAD_REQUEST, "invalid_channel")),
    };
    if let Some(source) = source {
        let matches: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM conversations WHERE id=$1 AND owner=$2 AND channel=$3)",
        )
        .bind(source.job.conversation_id)
        .bind(&owner)
        .bind(channel)
        .fetch_one(&mut **tx)
        .await?;
        if matches {
            return Ok((source.job.conversation_id, owner, version));
        }
    }
    let external = if channel == "feishu" {
        owner.clone()
    } else {
        "todos:admin".into()
    };
    let id:Uuid=sqlx::query_scalar("INSERT INTO conversations(id,owner,channel,external_key,title) VALUES($1,$2,$3,$4,'待办事项') ON CONFLICT(external_key) DO UPDATE SET external_key=excluded.external_key RETURNING id")
        .bind(Uuid::new_v4()).bind(&owner).bind(channel).bind(external).fetch_one(&mut **tx).await?;
    Ok((id, owner, version))
}
/// 安排输入一次验证，固定锚点使月末和夏令时计算可重复。
fn validate_schedule(input: &ScheduleInput) -> ApiResult<DateTime<Utc>> {
    let due = crate::followups::policy::parse_time(&input.next_run_at, &input.timezone)?;
    if due <= Utc::now()
        || due > Utc::now() + Duration::days(366)
        || !matches!(input.kind.as_str(), "reminder" | "checkin" | "execute")
        || !matches!(
            input.recurrence.as_str(),
            "once" | "daily" | "weekdays" | "weekly" | "monthly"
        )
        || !matches!(input.missed_policy.as_str(), "skip" | "latest")
        || !(1..=10080).contains(&input.grace_minutes)
        || input.instruction.chars().count() > super::MAX_TEXT
        || (input.kind == "execute" && input.instruction.trim().is_empty())
        || input.ends_at.is_some_and(|end| end <= due)
        || input.timezone.parse::<chrono_tz::Tz>().is_err()
    {
        return Err(ApiError(StatusCode::BAD_REQUEST, "invalid_todo_schedule"));
    }
    Ok(due)
}
/// 新安排只保存授权的处理方式，后续由独立调度器生成期次。
async fn insert_schedule(
    tx: &mut Transaction<'_, Postgres>,
    actor: &str,
    todo: Uuid,
    input: &ScheduleInput,
    source: Option<&Source<'_>>,
) -> ApiResult<Uuid> {
    let due = validate_schedule(input)?;
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM todo_schedules WHERE todo_id=$1")
        .bind(todo)
        .fetch_one(&mut **tx)
        .await?;
    if count >= super::MAX_SCHEDULES {
        return Err(ApiError(StatusCode::CONFLICT, "todo_schedule_limit"));
    }
    let (conversation, owner, version) = destination(tx, actor, &input.channel, source).await?;
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO todo_schedules(id,todo_id,kind,next_run_at,anchor_at,timezone,recurrence,ends_at,missed_policy,grace_minutes,instruction,conversation_id,delivery_owner,binding_version) VALUES($1,$2,$3,$4,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13)")
        .bind(id).bind(todo).bind(&input.kind).bind(due).bind(&input.timezone).bind(&input.recurrence).bind(input.ends_at).bind(&input.missed_policy).bind(input.grace_minutes).bind(&input.instruction).bind(conversation).bind(owner).bind(version).execute(&mut **tx).await?;
    event(
        tx,
        todo,
        actor,
        "schedule_created",
        json!({"id":id,"schedule":input}),
    )
    .await?;
    Ok(id)
}
/// 新增/编辑安排以待办和安排双版本拒绝旧确认，暂停无需提供新的时间。
pub async fn schedule(
    state: &AppState,
    actor: &str,
    todo: Uuid,
    input: &Value,
    source: Option<&Source<'_>>,
) -> ApiResult<Value> {
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Change {
        todo_version: i64,
        id: Option<Uuid>,
        version: Option<i64>,
        status: String,
        schedule: Option<ScheduleInput>,
        idempotency_key: Uuid,
    }
    let change: Change = serde_json::from_value(input.clone())
        .map_err(|_| ApiError(StatusCode::BAD_REQUEST, "invalid_arguments"))?;
    if !matches!(change.status.as_str(), "enabled" | "paused" | "ended") {
        return Err(ApiError(StatusCode::BAD_REQUEST, "invalid_arguments"));
    }
    let mut tx = begin(state, actor, source).await?;
    let key = source
        .map(|s| format!("{}:{}", s.job.id, s.operation_key))
        .unwrap_or_else(|| change.idempotency_key.to_string());
    let hash = crate::auth::hash(&format!("{todo}:{input}"));
    if let Some(result) = previous(&mut tx, &key, &hash).await? {
        return Ok(result);
    }
    let valid:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM todos WHERE id=$1 AND owner='admin' AND version=$2 AND status NOT IN('completed','cancelled'))")
        .bind(todo).bind(change.todo_version).fetch_one(&mut *tx).await?;
    if !valid {
        return Err(ApiError(StatusCode::CONFLICT, "todo_version_conflict"));
    }
    let mut started = false;
    let id = if let Some(id) = change.id {
        let found: Option<i64> = sqlx::query_scalar(
            "SELECT version FROM todo_schedules WHERE id=$1 AND todo_id=$2 FOR UPDATE",
        )
        .bind(id)
        .bind(todo)
        .fetch_optional(&mut *tx)
        .await?;
        if found.is_none() || found != change.version {
            return Err(ApiError(StatusCode::CONFLICT, "todo_version_conflict"));
        }
        started = cancel_pending(&mut tx, id).await?;
        if change.status == "enabled" {
            let s = change
                .schedule
                .as_ref()
                .ok_or(ApiError(StatusCode::BAD_REQUEST, "invalid_todo_schedule"))?;
            let due = validate_schedule(s)?;
            let (conversation, owner, binding) =
                destination(&mut tx, actor, &s.channel, source).await?;
            sqlx::query("UPDATE todo_schedules SET kind=$2,next_run_at=$3,anchor_at=$3,timezone=$4,recurrence=$5,ends_at=$6,missed_policy=$7,grace_minutes=$8,instruction=$9,conversation_id=$10,delivery_owner=$11,binding_version=$12 WHERE id=$1")
                .bind(id).bind(&s.kind).bind(due).bind(&s.timezone).bind(&s.recurrence).bind(s.ends_at).bind(&s.missed_policy).bind(s.grace_minutes).bind(&s.instruction).bind(conversation).bind(owner).bind(binding).execute(&mut *tx).await?;
        }
        sqlx::query(
            "UPDATE todo_schedules SET status=$2,version=version+1,updated_at=now() WHERE id=$1",
        )
        .bind(id)
        .bind(&change.status)
        .execute(&mut *tx)
        .await?;
        event(&mut tx, todo, actor, "schedule_updated", input.clone()).await?;
        id
    } else {
        if change.status != "enabled" {
            return Err(ApiError(StatusCode::BAD_REQUEST, "invalid_arguments"));
        }
        insert_schedule(
            &mut tx,
            actor,
            todo,
            change
                .schedule
                .as_ref()
                .ok_or(ApiError(StatusCode::BAD_REQUEST, "invalid_todo_schedule"))?,
            source,
        )
        .await?
    };
    sqlx::query("UPDATE todos SET version=version+1,updated_at=now() WHERE id=$1")
        .bind(todo)
        .execute(&mut *tx)
        .await?;
    let result =
        json!({"id":id,"todo_version":change.todo_version+1,"delivery_may_have_started":started});
    record(&mut tx, &key, &hash, &result).await?;
    tx.commit().await?;
    Ok(result)
}
/// 关联只接受实际可访问的内部资源，Linear 标识通过服务端读取校验。
pub async fn link(
    state: &AppState,
    actor: &str,
    todo: Uuid,
    input: &Value,
    source: Option<&Source<'_>>,
) -> ApiResult<Value> {
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Link {
        version: i64,
        kind: String,
        resource_id: String,
        #[serde(default)]
        label: String,
    }
    let link: Link = serde_json::from_value(input.clone())
        .map_err(|_| ApiError(StatusCode::BAD_REQUEST, "invalid_arguments"))?;
    if link.label.chars().count() > 500 || link.resource_id.len() > 200 {
        return Err(ApiError(StatusCode::BAD_REQUEST, "invalid_arguments"));
    }
    identity::require_owner(state, actor).await?;
    let allowed=match link.kind.as_str() {
        "conversation"=>sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM conversations WHERE id::text=$1 AND personal_owner(owner)='admin')").bind(&link.resource_id).fetch_one(&state.pool).await?,
        "communication"=>sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM communication_documents d JOIN communication_sources s ON s.id=d.source_id WHERE d.id::text=$1 AND s.owner='admin' AND s.enabled AND NOT s.removal_pending)").bind(&link.resource_id).fetch_one(&state.pool).await?,
        "knowledge"=>sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM knowledge_entries WHERE id::text=$1)").bind(&link.resource_id).fetch_one(&state.pool).await?,
        "linear"=>crate::linear::read_for_todo(state,"linear_issue_get",json!({"issue":link.resource_id})).await.is_ok(),
        _=>false,
    };
    if !allowed {
        return Err(ApiError(StatusCode::NOT_FOUND, "todo_resource_unavailable"));
    }
    let mut tx = begin(state, actor, source).await?;
    let key = source.map(|s| format!("{}:{}", s.job.id, s.operation_key));
    let hash = crate::auth::hash(&format!("{todo}:{input}"));
    if let Some(key) = &key
        && let Some(result) = previous(&mut tx, key, &hash).await?
    {
        return Ok(result);
    }
    let changed=sqlx::query("UPDATE todos SET version=version+1,updated_at=now() WHERE id=$1 AND owner='admin' AND version=$2").bind(todo).bind(link.version).execute(&mut *tx).await?;
    if changed.rows_affected() == 0 {
        return Err(ApiError(StatusCode::CONFLICT, "todo_version_conflict"));
    }
    sqlx::query("INSERT INTO todo_links(id,todo_id,kind,resource_id,label) VALUES($1,$2,$3,$4,$5) ON CONFLICT(todo_id,kind,resource_id) DO UPDATE SET label=excluded.label")
        .bind(Uuid::new_v4()).bind(todo).bind(&link.kind).bind(&link.resource_id).bind(&link.label).execute(&mut *tx).await?;
    event(&mut tx, todo, actor, "linked", input.clone()).await?;
    let result = json!({"id":todo,"version":link.version+1});
    if let Some(key) = key {
        record(&mut tx, &key, &hash, &result).await?;
    }
    tx.commit().await?;
    Ok(result)
}

/// 只确认指定一期的业务结果，保留调度和投递状态，后续周期继续执行。
pub async fn complete_run(
    state: &AppState,
    actor: &str,
    todo: Uuid,
    input: &Value,
    source: Option<&Source<'_>>,
) -> ApiResult<Value> {
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Completion {
        run_id: Uuid,
        version: i64,
        note: String,
    }
    let change: Completion = serde_json::from_value(input.clone())
        .map_err(|_| ApiError(StatusCode::BAD_REQUEST, "invalid_arguments"))?;
    if change.note.trim().is_empty() || change.note.chars().count() > super::MAX_TEXT {
        return Err(ApiError(StatusCode::BAD_REQUEST, "invalid_todo"));
    }
    let mut tx = begin(state, actor, source).await?;
    let key = source.map(|s| format!("{}:{}", s.job.id, s.operation_key));
    let hash = crate::auth::hash(&format!("{todo}:{input}"));
    if let Some(key) = &key
        && let Some(result) = previous(&mut tx, key, &hash).await?
    {
        return Ok(result);
    }
    let changed=sqlx::query("UPDATE todo_runs SET completion_note=$4,completed_at=now(),version=version+1 WHERE id=$1 AND todo_id=$2 AND version=$3 AND todo_id IN(SELECT id FROM todos WHERE owner='admin')")
        .bind(change.run_id).bind(todo).bind(change.version).bind(&change.note).execute(&mut *tx).await?;
    if changed.rows_affected() == 0 {
        return Err(ApiError(StatusCode::CONFLICT, "todo_version_conflict"));
    }
    sqlx::query("UPDATE outbox SET status='cancelled',lease_token=NULL WHERE followup_id IN(SELECT followup_id FROM todo_runs WHERE id=$1) AND status IN('queued','running')")
        .bind(change.run_id).execute(&mut *tx).await?;
    sqlx::query("UPDATE followups SET status='cancelled',lease_token=NULL,version=version+1 WHERE id=(SELECT followup_id FROM todo_runs WHERE id=$1) AND status IN('scheduled','checking','queued')")
        .bind(change.run_id).execute(&mut *tx).await?;
    event(
        &mut tx,
        todo,
        actor,
        "period_completed",
        json!({"run_id":change.run_id,"note":change.note}),
    )
    .await?;
    let result = json!({"id":change.run_id,"version":change.version+1,"completed":true});
    if let Some(key) = key {
        record(&mut tx, &key, &hash, &result).await?;
    }
    tx.commit().await?;
    Ok(result)
}
