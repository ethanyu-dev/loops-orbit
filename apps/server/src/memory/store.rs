use super::Entry;
use anyhow::{Context, ensure};
use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};
use uuid::Uuid;

// 单条记忆文件限制，防止手动编辑引入无界文件；索引不会读取符号链接。
const MAX_FILE_BYTES: u64 = 32_768;

/// owner 使用哈希目录名，用户和模型不能提供文件路径。
pub fn owner_dir(root: &Path, owner: &str) -> PathBuf {
    root.join(crate::auth::hash(owner))
}

/// 同目录临时文件、同步落盘再重命名，进程中断不会留下半份原文。
pub fn atomic_write(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    let parent = path.parent().context("缺少目录")?;
    fs::create_dir_all(parent)?;
    let temporary = parent.join(format!(".{}.tmp", Uuid::new_v4()));
    let result = (|| {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::rename(&temporary, path)?;
        fs::File::open(parent)?.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result
}

/// 正文保留 Markdown，首段 JSON front matter 保存可验证的元数据。
pub fn write(root: &Path, owner: &str, entry: &Entry) -> anyhow::Result<()> {
    let mut metadata = entry.clone();
    metadata.content.clear();
    let text = format!(
        "---\n{}\n---\n{}\n",
        serde_json::to_string_pretty(&metadata)?,
        entry.content
    );
    atomic_write(
        &owner_dir(root, owner).join(format!("{}.md", entry.id)),
        text.as_bytes(),
    )
}

/// 每次读取文件原文，哈希变化立即使旧向量失效；损坏文件报错而非当作删除。
pub fn read(root: &Path, owner: &str) -> anyhow::Result<Vec<Entry>> {
    let directory = owner_dir(root, owner);
    if !directory.exists() {
        return Ok(Vec::new());
    }
    let mut entries = Vec::new();
    for file in fs::read_dir(directory)? {
        let file = file?;
        if file.path().extension().and_then(|s| s.to_str()) != Some("md") {
            continue;
        }
        ensure!(file.file_type()?.is_file(), "记忆原文必须是普通文件");
        ensure!(file.metadata()?.len() <= MAX_FILE_BYTES, "记忆文件过大");
        let text = fs::read_to_string(file.path())?;
        let (metadata, content) = text
            .strip_prefix("---\n")
            .context("记忆元数据缺失")?
            .split_once("\n---\n")
            .context("记忆元数据边界无效")?;
        let mut entry: Entry = serde_json::from_str(metadata)?;
        ensure!(
            file.file_name().to_str() == Some(&format!("{}.md", entry.id)),
            "记忆 ID 与路径不一致"
        );
        entry.content = content.trim().to_owned();
        if !entry.deleted {
            entry.validate()?;
        }
        entries.push(entry);
    }
    entries.sort_by(|a, b| b.updated_at.cmp(&a.updated_at).then(a.id.cmp(&b.id)));
    Ok(entries)
}

/// 遗忘边界也存为文件，数据库丢失后重建不会重新消费旧聊天。
pub fn boundary(root: &Path, owner: &str) -> anyhow::Result<i64> {
    let path = owner_dir(root, owner).join("_boundary.json");
    if !path.exists() {
        return Ok(0);
    }
    Ok(serde_json::from_slice(&fs::read(path)?)?)
}

/// 先持久化边界，再改正文；重启时可修复尚未完成的数据库失效操作。
pub fn write_boundary(root: &Path, owner: &str, through: i64) -> anyhow::Result<()> {
    atomic_write(
        &owner_dir(root, owner).join("_boundary.json"),
        &serde_json::to_vec(&through)?,
    )
}
