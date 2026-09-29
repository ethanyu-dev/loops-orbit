use super::Entry;
use jieba_rs::Jieba;
use tantivy::{
    Index, TantivyDocument, Term,
    collector::TopDocs,
    query::{BooleanQuery, Occur, TermQuery},
    schema::{IndexRecordOption, STORED, STRING, Schema, TextFieldIndexing, TextOptions, Value},
    tokenizer::{SimpleTokenizer, TextAnalyzer},
};
use uuid::Uuid;

// 每条检索路径独立召回，再用名次融合；小规模个人语料无须常驻搜索服务。
const CANDIDATES: usize = 20;
// 词典加载一次，避免每篇文档重新分配完整中文词库。
static SEGMENTER: std::sync::LazyLock<Jieba> = std::sync::LazyLock::new(Jieba::new);

/// 中文先分词，英文统一小写；文档和查询使用同一分析流程。
fn tokens(text: &str) -> Vec<String> {
    SEGMENTER
        .cut_for_search(text, true)
        .into_iter()
        .flat_map(|part| {
            part.word
                .split(|c: char| !c.is_alphanumeric())
                .filter(|word| !word.is_empty())
                .map(str::to_lowercase)
        })
        .collect()
}

/// 缓存一个身份的 BM25 索引，内容指纹变化时整体重建，避免旧版本残留。
pub struct Lexical {
    /// 来源内容哈希，不包含其他身份文件。
    pub fingerprint: String,
    /// 内存索引可完全从文件恢复。
    index: Index,
    /// 存储 UUID 与分词正文的 schema 字段。
    id: tantivy::schema::Field,
    /// 仅用于匹配和打分，不保存原文副本。
    body: tantivy::schema::Field,
}
impl Lexical {
    /// 个人语料使用内存索引，提交后才能创建读取快照。
    pub fn build(entries: &[Entry], fingerprint: String) -> anyhow::Result<Self> {
        let mut schema = Schema::builder();
        let id = schema.add_text_field("id", STRING | STORED);
        let body = schema.add_text_field(
            "body",
            TextOptions::default().set_indexing_options(
                TextFieldIndexing::default()
                    .set_tokenizer("words")
                    .set_index_option(IndexRecordOption::WithFreqs),
            ),
        );
        let index = Index::create_in_ram(schema.build());
        index
            .tokenizers()
            .register("words", TextAnalyzer::from(SimpleTokenizer::default()));
        let mut writer = index.writer_with_num_threads(1, 20_000_000)?;
        for entry in entries {
            let mut doc = TantivyDocument::default();
            doc.add_text(id, entry.id.to_string());
            doc.add_text(
                body,
                tokens(&format!("{} {}", entry.key, entry.content)).join(" "),
            );
            writer.add_document(doc)?;
        }
        writer.commit()?;
        Ok(Self {
            fingerprint,
            index,
            id,
            body,
        })
    }
    /// 用 term 查询避免用户输入被当作查询语法，不匹配时返回空列表。
    pub fn search(&self, query: &str) -> anyhow::Result<Vec<Uuid>> {
        let terms: std::collections::BTreeSet<_> = tokens(query).into_iter().take(128).collect();
        if terms.is_empty() {
            return Ok(vec![]);
        }
        let query = BooleanQuery::new(
            terms
                .into_iter()
                .map(|word| {
                    (
                        Occur::Should,
                        Box::new(TermQuery::new(
                            Term::from_field_text(self.body, &word),
                            IndexRecordOption::WithFreqs,
                        )) as Box<dyn tantivy::query::Query>,
                    )
                })
                .collect(),
        );
        let reader = self.index.reader()?;
        let searcher = reader.searcher();
        searcher
            .search(&query, &TopDocs::with_limit(CANDIDATES).order_by_score())?
            .into_iter()
            .map(|(_, address)| {
                let doc: TantivyDocument = searcher.doc(address)?;
                Ok(Uuid::parse_str(
                    doc.get_first(self.id)
                        .and_then(|v| v.as_str())
                        .ok_or_else(|| anyhow::anyhow!("缺少索引 ID"))?,
                )?)
            })
            .collect()
    }
}
