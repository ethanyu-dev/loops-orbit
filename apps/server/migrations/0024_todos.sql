-- 身份绑定与资料采集凭据分离；旧 owner 保留为来源和投递标识。
CREATE TABLE personal_identities (
    owner TEXT PRIMARY KEY,
    principal TEXT NOT NULL DEFAULT 'admin' CHECK(principal='admin'),
    enabled BOOLEAN NOT NULL DEFAULT true,
    version BIGINT NOT NULL DEFAULT 1,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
INSERT INTO personal_identities(owner)
SELECT 'feishu:'||open_id FROM communication_connections WHERE status='active';

-- 仅经过服务端验证的绑定参与共享；访客和其他飞书身份原样隔离。
CREATE FUNCTION personal_owner(source TEXT) RETURNS TEXT LANGUAGE sql STABLE AS $$
    SELECT COALESCE((SELECT principal FROM personal_identities WHERE owner=source AND enabled),source)
$$;
CREATE FUNCTION bind_personal_identity() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    INSERT INTO personal_identities(owner) VALUES('feishu:'||NEW.open_id)
    ON CONFLICT(owner) DO UPDATE SET enabled=true,version=personal_identities.version+1,updated_at=now();
    -- 首次绑定将已验证账号的旧待办并入本人空间，保留聊天和投递来源。
    UPDATE todos SET owner='admin' WHERE owner='feishu:'||NEW.open_id;
    UPDATE todo_schedules SET binding_version=(SELECT version FROM personal_identities WHERE owner='feishu:'||NEW.open_id)
    WHERE delivery_owner='feishu:'||NEW.open_id;
    RETURN NEW;
END $$;
CREATE TRIGGER communication_identity_binding AFTER INSERT OR UPDATE OF open_id
ON communication_connections FOR EACH ROW EXECUTE FUNCTION bind_personal_identity();

-- 事项只记录业务状态；投递、每期执行与业务完成互不混用。
CREATE TABLE todos (
    id UUID PRIMARY KEY,
    owner TEXT NOT NULL,
    title TEXT NOT NULL CHECK(length(title) BETWEEN 1 AND 500),
    objective TEXT NOT NULL DEFAULT '',
    completion_criteria TEXT NOT NULL DEFAULT '',
    next_action TEXT NOT NULL DEFAULT '',
    waiting_on TEXT NOT NULL DEFAULT '',
    status TEXT NOT NULL DEFAULT 'active' CHECK(status IN('active','waiting_external','needs_user','completed','cancelled')),
    due_at TIMESTAMPTZ,
    version BIGINT NOT NULL DEFAULT 1,
    origin_key TEXT NOT NULL,
    request_hash TEXT NOT NULL DEFAULT '',
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    closed_at TIMESTAMPTZ,
    UNIQUE(owner,origin_key)
);
CREATE INDEX todos_owner ON todos(owner,updated_at DESC,id);
CREATE TABLE todo_schedules (
    id UUID PRIMARY KEY,
    todo_id UUID NOT NULL REFERENCES todos(id),
    kind TEXT NOT NULL CHECK(kind IN('reminder','checkin','execute')),
    status TEXT NOT NULL DEFAULT 'enabled' CHECK(status IN('enabled','paused','ended')),
    next_run_at TIMESTAMPTZ NOT NULL,
    timezone TEXT NOT NULL,
    recurrence TEXT NOT NULL DEFAULT 'once' CHECK(recurrence IN('once','daily','weekdays','weekly','monthly')),
    anchor_at TIMESTAMPTZ NOT NULL,
    ends_at TIMESTAMPTZ,
    missed_policy TEXT NOT NULL DEFAULT 'latest' CHECK(missed_policy IN('skip','latest')),
    grace_minutes INT NOT NULL DEFAULT 1440 CHECK(grace_minutes BETWEEN 1 AND 10080),
    instruction TEXT NOT NULL DEFAULT '',
    conversation_id UUID NOT NULL REFERENCES conversations(id),
    delivery_owner TEXT NOT NULL,
    binding_version BIGINT,
    version BIGINT NOT NULL DEFAULT 1,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX todo_schedules_due ON todo_schedules(next_run_at) WHERE status='enabled';
CREATE TABLE todo_runs (
    id UUID PRIMARY KEY,
    todo_id UUID NOT NULL REFERENCES todos(id),
    schedule_id UUID NOT NULL REFERENCES todo_schedules(id),
    schedule_version BIGINT NOT NULL,
    scheduled_at TIMESTAMPTZ NOT NULL,
    followup_id UUID UNIQUE REFERENCES followups(id),
    status TEXT NOT NULL DEFAULT 'queued',
    result TEXT,
    completion_note TEXT NOT NULL DEFAULT '',
    completed_at TIMESTAMPTZ,
    version BIGINT NOT NULL DEFAULT 1,
    error TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    finished_at TIMESTAMPTZ,
    UNIQUE(schedule_id,schedule_version,scheduled_at)
);
CREATE TABLE todo_events (
    seq BIGSERIAL PRIMARY KEY,
    todo_id UUID NOT NULL REFERENCES todos(id),
    actor TEXT NOT NULL,
    kind TEXT NOT NULL,
    detail JSONB NOT NULL DEFAULT '{}',
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX todo_events_item ON todo_events(todo_id,seq DESC);
CREATE TABLE todo_links (
    id UUID PRIMARY KEY,
    todo_id UUID NOT NULL REFERENCES todos(id),
    kind TEXT NOT NULL CHECK(kind IN('conversation','communication','knowledge','linear')),
    resource_id TEXT NOT NULL,
    label TEXT NOT NULL DEFAULT '',
    UNIQUE(todo_id,kind,resource_id)
);
CREATE TABLE todo_operations (
    owner TEXT NOT NULL,
    operation_key TEXT NOT NULL,
    request_hash TEXT NOT NULL,
    result JSONB NOT NULL,
    PRIMARY KEY(owner,operation_key)
);
ALTER TABLE followups ADD COLUMN todo_schedule_id UUID REFERENCES todo_schedules(id);

-- 兼容旧工具与资料页的提醒创建入口，旧记录使用自身 UUID，迁移可追溯。
CREATE FUNCTION attach_followup_todo() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.todo_schedule_id IS NOT NULL THEN RETURN NEW; END IF;
    INSERT INTO todos(id,owner,title,status,origin_key,created_at,updated_at,closed_at)
    VALUES(NEW.id,personal_owner(NEW.owner),NEW.topic,
        CASE WHEN NEW.status IN('completed','cancelled') THEN NEW.status ELSE 'active' END,
        'followup:'||NEW.id,NEW.updated_at,NEW.updated_at,
        CASE WHEN NEW.status IN('completed','cancelled') THEN NEW.updated_at END);
    INSERT INTO todo_schedules(id,todo_id,kind,status,next_run_at,anchor_at,timezone,conversation_id,delivery_owner,binding_version)
    VALUES(NEW.id,NEW.id,NEW.kind,'ended',NEW.due_at,NEW.due_at,NEW.timezone,NEW.conversation_id,NEW.owner,
        (SELECT version FROM personal_identities WHERE owner=NEW.owner AND enabled));
    NEW.todo_schedule_id:=NEW.id;
    RETURN NEW;
END $$;
CREATE TRIGGER followup_attach BEFORE INSERT ON followups FOR EACH ROW EXECUTE FUNCTION attach_followup_todo();

INSERT INTO todos(id,owner,title,status,origin_key,created_at,updated_at,closed_at)
SELECT id,personal_owner(owner),topic,CASE WHEN status IN('completed','cancelled') THEN status ELSE 'active' END,
    'followup:'||id,created_at,updated_at,CASE WHEN status IN('completed','cancelled') THEN updated_at END FROM followups;
INSERT INTO todo_schedules(id,todo_id,kind,status,next_run_at,anchor_at,timezone,conversation_id,delivery_owner,binding_version)
SELECT id,id,kind,'ended',due_at,due_at,timezone,conversation_id,owner,
    (SELECT version FROM personal_identities WHERE owner=followups.owner AND enabled) FROM followups;
UPDATE followups SET todo_schedule_id=id;
INSERT INTO todo_links(id,todo_id,kind,resource_id)
SELECT id,id,'conversation',conversation_id::text FROM followups;
INSERT INTO todo_runs(id,todo_id,schedule_id,schedule_version,scheduled_at,followup_id,status,error,result,finished_at)
SELECT id,id,id,1,due_at,id,status,error,(SELECT content FROM messages WHERE followup_id=followups.id ORDER BY seq DESC LIMIT 1),CASE WHEN status NOT IN('scheduled','checking','queued') THEN updated_at END FROM followups;
INSERT INTO todo_events(todo_id,actor,kind,detail)
SELECT id,owner,'migrated',jsonb_build_object('followup_id',id,'delivery_status',status) FROM followups;

-- 每次处理保留真实投递结果；旧接口明确完成/取消时才结束对应单次待办。
CREATE FUNCTION record_followup_todo() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE item UUID; schedule_version BIGINT;
BEGIN
    SELECT todo_id,version INTO item,schedule_version FROM todo_schedules WHERE id=NEW.todo_schedule_id;
    IF item IS NULL THEN RETURN NEW; END IF;
    INSERT INTO todo_runs(id,todo_id,schedule_id,schedule_version,scheduled_at,followup_id,status,error,finished_at)
    VALUES(NEW.id,item,NEW.todo_schedule_id,schedule_version,NEW.due_at,NEW.id,NEW.status,NEW.error,
        CASE WHEN NEW.status NOT IN('scheduled','checking','queued') THEN now() END)
    ON CONFLICT(followup_id) DO UPDATE SET status=excluded.status,error=excluded.error,
        finished_at=excluded.finished_at,
        result=(SELECT content FROM messages WHERE followup_id=NEW.id ORDER BY seq DESC LIMIT 1);
    IF TG_OP='INSERT' THEN
        INSERT INTO todo_links(id,todo_id,kind,resource_id) VALUES(NEW.id,item,'conversation',NEW.conversation_id::text) ON CONFLICT DO NOTHING;
        INSERT INTO todo_events(todo_id,actor,kind,detail) VALUES(item,NEW.owner,'scheduled',jsonb_build_object('followup_id',NEW.id));
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER followup_record AFTER INSERT OR UPDATE OF status,error ON followups
FOR EACH ROW EXECUTE FUNCTION record_followup_todo();
