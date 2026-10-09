//! apkg 写入器：把校验过的牌组计划落成 `.apkg` 文件。
//!
//! apkg 的结构（对齐 genanki 的 `Package.write_to_file`，字段逐一对照其源码实现）：
//!
//! ```text
//! deck.apkg (zip)
//! ├── collection.anki2   ← SQLite 库（ver 11 旧版 schema），Anki 导入器按 SQLite 打开
//! ├── media              ← JSON manifest：{"0": "heap.png", ...}
//! └── 0, 1, ...          ← 媒体文件本体
//! ```
//!
//! `collection.anki2` 里的关键约定：
//! - `notes.guid` 是 Anki 的合并键：同 guid 再导入是更新，异 guid 是新建（见 [`crate::guid`]）。
//! - `notes.csum` 按 genanki 恒写 0（「can be ignored」，导入方会重建）。
//! - id 序列从「当前毫秒时间戳」起自增，避免同毫秒主键冲突（对齐 genanki 的 `id_gen`）。
//! - 模型/牌组 id 用固定常量与名称派生哈希，跨次生成稳定，这是导入合并的另一半前提。
//! - 牌组名带 `::` 时必须把**每一级父牌组**都写进 `col.decks`，缺级会导致导入层级错乱。

use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::Connection;
use serde_json::{Value, json};

use crate::error::{DeckError, ErrorCode};
use crate::guid::{guid_for, stable_deck_id};
use crate::spec::{DeckSpecArgs, extract_cloze_ords};

/// 模型（note type）id。固定值：跨次生成一致，重导入时 Anki 才会合并到既有模型
/// 而不是每次新建一份（genanki 用随机 id 导致的重复模型问题，见其 builtin_models 注释）。
const BASIC_MODEL_ID: i64 = 1_728_660_000_001;
const CLOZE_MODEL_ID: i64 = 1_728_660_000_002;

const DEFAULT_LATEX_PRE: &str = "\\documentclass[12pt]{article}\n\\special{papersize=3in,5in}\n\\usepackage[utf8]{inputenc}\n\\usepackage{amssymb,amsmath}\n\\pagestyle{empty}\n\\setlength{\\parindent}{0in}\n\\begin{document}\n";
const DEFAULT_LATEX_POST: &str = "\\end{document}";

const DEFAULT_CSS: &str = ".card {\n font-family: arial;\n font-size: 20px;\n text-align: center;\n color: black;\n background-color: white;\n}\n";
const CLOZE_CSS_EXTRA: &str = "\n.cloze {\n font-weight: bold;\n color: blue;\n}\n.nightMode .cloze {\n color: lightblue;\n}";

/// genanki 的 APKG_COL 里钉死的 `crt`（collection 创建时刻，秒）。它只影响 Anki 里的
/// 「今天」分界，genanki 也没用真时间，这里保持一致以便产物与 genanki 对齐验证。
const COL_CRT: i64 = 1_411_124_400;

/// 旧版 schema（`ver` 11），逐字对齐 genanki 的 `apkg_schema.py`。
const SCHEMA_SQL: &str = r#"
CREATE TABLE col (
    id              integer primary key,
    crt             integer not null,
    mod             integer not null,
    scm             integer not null,
    ver             integer not null,
    dty             integer not null,
    usn             integer not null,
    ls              integer not null,
    conf            text not null,
    models          text not null,
    decks           text not null,
    dconf           text not null,
    tags            text not null
);
CREATE TABLE notes (
    id              integer primary key,
    guid            text not null,
    mid             integer not null,
    mod             integer not null,
    usn             integer not null,
    tags            text not null,
    flds            text not null,
    sfld            integer not null,
    csum            integer not null,
    flags           integer not null,
    data            text not null
);
CREATE TABLE cards (
    id              integer primary key,
    nid             integer not null,
    did             integer not null,
    ord             integer not null,
    mod             integer not null,
    usn             integer not null,
    type            integer not null,
    queue           integer not null,
    due             integer not null,
    ivl             integer not null,
    factor          integer not null,
    reps            integer not null,
    lapses          integer not null,
    left            integer not null,
    odue            integer not null,
    odid            integer not null,
    flags           integer not null,
    data            text not null
);
CREATE TABLE revlog (
    id              integer primary key,
    cid             integer not null,
    usn             integer not null,
    ease            integer not null,
    ivl             integer not null,
    lastIvl         integer not null,
    factor          integer not null,
    time            integer not null,
    type            integer not null
);
CREATE TABLE graves (
    usn             integer not null,
    oid             integer not null,
    type            integer not null
);
CREATE INDEX ix_notes_usn on notes (usn);
CREATE INDEX ix_cards_usn on cards (usn);
CREATE INDEX ix_revlog_usn on revlog (usn);
CREATE INDEX ix_cards_nid on cards (nid);
CREATE INDEX ix_cards_sched on cards (did, queue, due);
CREATE INDEX ix_revlog_cid on revlog (cid);
CREATE INDEX ix_notes_csum on notes (csum);
"#;

/// 打包完的统计，进工具结果文本给模型看。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WriteStats {
    pub notes: usize,
    /// 其中挖空卡数。
    pub cloze_notes: usize,
    /// 实际写入 `cards` 表的行数（挖空卡一张笔记可能有多张卡）。
    pub cards: usize,
    pub media: usize,
}

/// 相对 working_dir 解析路径；绝对路径原样保留。
pub fn resolve_path(working_dir: &Path, path: &str) -> PathBuf {
    let candidate = Path::new(path);
    if candidate.is_absolute() {
        candidate.to_path_buf()
    } else {
        working_dir.join(candidate)
    }
}

/// 把 spec 解析成可打包的计划：解析媒体文件、派生 GUID。
///
/// 纯校验（[`crate::spec::validate`]）已在工具入口跑过；这里做需要文件系统的那半。
pub fn build_plan(
    spec: &DeckSpecArgs,
    working_dir: &Path,
) -> Result<DeckPlan, DeckError> {
    let mut media = Vec::with_capacity(spec.deck.media.len());
    let mut seen_basenames = BTreeMap::new();
    for (index, entry) in spec.deck.media.iter().enumerate() {
        let source = resolve_path(working_dir, entry);
        let metadata = std::fs::metadata(&source).map_err(|error| {
            DeckError::new(
                ErrorCode::MediaMissing,
                format!("media[{index}] {:?} is missing: {error}", entry),
            )
        })?;
        if !metadata.is_file() {
            return Err(DeckError::new(
                ErrorCode::MediaMissing,
                format!("media[{index}] {:?} is not a regular file", entry),
            ));
        }
        let basename = source
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| {
                DeckError::new(
                    ErrorCode::MediaMissing,
                    format!("media[{index}] {:?} has no usable file name", entry),
                )
            })?
            .to_owned();
        if let Some(previous) = seen_basenames.get(basename.as_str()) {
            return Err(DeckError::new(
                ErrorCode::MediaDuplicate,
                format!(
                    "media[{index}] {:?} and media[{previous}] share the file name {:?}; \
                     Anki media are stored flat by file name",
                    entry, basename
                ),
            ));
        }
        seen_basenames.insert(basename.clone(), index);
        media.push(MediaEntry { source, basename });
    }

    let cards = spec
        .deck
        .cards
        .iter()
        .map(|card| {
            let identity = match &card.id {
                Some(id) => vec![spec.deck.name.as_str(), "id", id.as_str()],
                None => vec![spec.deck.name.as_str(), "fields", card.front.as_str(), card.back.as_str()],
            };
            PlanCard {
                front: card.front.clone(),
                back: card.back.clone(),
                tags: card.tags.clone(),
                cloze: card.cloze,
                guid: guid_for(&identity),
            }
        })
        .collect();

    Ok(DeckPlan {
        name: spec.deck.name.clone(),
        css: spec.deck.css.clone(),
        cards,
        media,
    })
}

#[derive(Debug)]
pub struct DeckPlan {
    pub name: String,
    pub css: Option<String>,
    pub cards: Vec<PlanCard>,
    pub media: Vec<MediaEntry>,
}

#[derive(Debug)]
pub struct PlanCard {
    pub front: String,
    pub back: String,
    pub tags: Vec<String>,
    pub cloze: bool,
    pub guid: String,
}

#[derive(Debug)]
pub struct MediaEntry {
    pub source: PathBuf,
    pub basename: String,
}

/// 毫秒时间戳起自增的 id 分配器（对齐 genanki 的 `itertools.count(int(t * 1000))`）。
struct IdGen {
    next: i64,
}

impl IdGen {
    fn next_id(&mut self) -> i64 {
        let id = self.next;
        self.next += 1;
        id
    }
}

/// 把计划写成 .apkg。成功返回统计。
pub fn write_apkg(plan: &DeckPlan, output: &Path) -> Result<WriteStats, DeckError> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| DeckError::new(ErrorCode::Packaging, format!("clock went backwards: {error}")))?;
    let now_ms = now.as_millis() as i64;
    let now_s = now_ms / 1000;
    let mut id_gen = IdGen { next: now_ms };

    let db_bytes = build_collection(plan, now_ms, now_s, &mut id_gen)?;

    write_zip(output, &db_bytes, plan)
        .map(|media| WriteStats {
            notes: plan.cards.len(),
            cloze_notes: plan.cards.iter().filter(|card| card.cloze).count(),
            cards: plan
                .cards
                .iter()
                .map(card_card_count)
                .sum(),
            media,
        })
}

/// 挖空笔记的实际卡数：每个不同 cN 一张，没有标记时按 1 张兜底（genanki `_cloze_cards`）。
fn card_card_count(card: &PlanCard) -> usize {
    if card.cloze {
        let ords = extract_cloze_ords(&card.front);
        if ords.is_empty() { 1 } else { ords.len() }
    } else {
        1
    }
}

/// 构建 `collection.anki2` 的字节内容。
fn build_collection(
    plan: &DeckPlan,
    now_ms: i64,
    now_s: i64,
    id_gen: &mut IdGen,
) -> Result<Vec<u8>, DeckError> {
    let temp_dir = std::env::temp_dir();
    let db_path = temp_dir.join(format!(
        "astrcode-anki-deck-{}-{}.sqlite",
        std::process::id(),
        now_ms
    ));
    let result = build_collection_at(&db_path, plan, now_ms, now_s, id_gen);
    // 临时库用完即删；删除失败不影响产物（留在系统临时目录里会被清理）。
    let _ = std::fs::remove_file(&db_path);
    result
}

fn build_collection_at(
    db_path: &Path,
    plan: &DeckPlan,
    now_ms: i64,
    now_s: i64,
    id_gen: &mut IdGen,
) -> Result<Vec<u8>, DeckError> {
    let io_error = |stage: &'static str| move |error: std::io::Error| {
        DeckError::new(ErrorCode::Io, format!("{stage}: {error}"))
    };

    std::fs::create_dir_all(db_path.parent().expect("temp dir has a parent"))
        .map_err(io_error("create temp dir"))?;

    let connection = Connection::open(db_path)
        .map_err(|error| DeckError::new(ErrorCode::Packaging, format!("open sqlite: {error}")))?;
    connection
        .execute_batch(SCHEMA_SQL)
        .map_err(|error| DeckError::new(ErrorCode::Packaging, format!("apply schema: {error}")))?;

    let leaf_id = stable_deck_id(&plan.name);
    let uses_cloze = plan.cards.iter().any(|card| card.cloze);
    let uses_basic = plan.cards.iter().any(|card| !card.cloze);

    let decks = decks_json(&plan.name, now_s);
    let models = models_json(plan, leaf_id, now_s, uses_basic, uses_cloze);
    let conf = conf_json(if uses_basic { BASIC_MODEL_ID } else { CLOZE_MODEL_ID });
    let dconf = dconf_json();

    connection
        .execute(
            "INSERT INTO col VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?)",
            rusqlite::params![
                1,
                COL_CRT,
                now_ms,
                now_ms,
                11,
                0,
                0,
                0,
                conf.to_string(),
                models.to_string(),
                decks.to_string(),
                dconf.to_string(),
                "{}",
            ],
        )
        .map_err(|error| DeckError::new(ErrorCode::Packaging, format!("insert col: {error}")))?;

    for card in &plan.cards {
        let (mid, flds) = if card.cloze {
            (CLOZE_MODEL_ID, vec![card.front.as_str(), card.back.as_str()])
        } else {
            (BASIC_MODEL_ID, vec![card.front.as_str(), card.back.as_str()])
        };

        let note_id = id_gen.next_id();
        let tags = format!(" {} ", card.tags.join(" "));
        connection
            .execute(
                "INSERT INTO notes VALUES(?,?,?,?,?,?,?,?,?,?,?)",
                rusqlite::params![
                    note_id,
                    card.guid,
                    mid,
                    now_s,
                    -1,
                    tags,
                    flds.join("\u{1f}"),
                    card.front,
                    0, // csum：genanki 恒写 0，导入方重建
                    0,
                    "",
                ],
            )
            .map_err(|error| DeckError::new(ErrorCode::Packaging, format!("insert note: {error}")))?;

        let ords: Vec<i64> = if card.cloze {
            let ords = extract_cloze_ords(&card.front);
            if ords.is_empty() {
                vec![0]
            } else {
                ords.into_iter().map(i64::from).collect()
            }
        } else {
            vec![0]
        };
        for ord in ords {
            connection
                .execute(
                    "INSERT INTO cards VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
                    rusqlite::params![
                        id_gen.next_id(),
                        note_id,
                        leaf_id,
                        ord,
                        now_s,
                        -1,
                        0, // type：非学习状态（新卡）
                        0, // queue
                        0, // due
                        0,
                        0,
                        0,
                        0,
                        0,
                        0,
                        0,
                        0,
                        "",
                    ],
                )
                .map_err(|error| {
                    DeckError::new(ErrorCode::Packaging, format!("insert card: {error}"))
                })?;
        }
    }

    connection
        .close()
        .map_err(|(_, error)| DeckError::new(ErrorCode::Packaging, format!("close sqlite: {error}")))?;

    std::fs::read(db_path).map_err(io_error("read built collection"))
}

/// `col.decks`：Default 牌组 + 目标牌组的每一级父链。
fn decks_json(name: &str, now_s: i64) -> Value {
    let mut decks = serde_json::Map::new();
    decks.insert(
        String::from("1"),
        json!({
            "collapsed": false,
            "conf": 1,
            "desc": "",
            "dyn": 0,
            "extendNew": 10,
            "extendRev": 50,
            "id": 1,
            "lrnToday": [0, 0],
            "mod": now_s,
            "name": "Default",
            "newToday": [0, 0],
            "revToday": [0, 0],
            "timeToday": [0, 0],
            "usn": 0,
        }),
    );

    // 父链逐级注册：`A::B::C` 需要 `A`、`A::B`、`A::B::C` 三个条目，id 由整条链名派生。
    let mut chain = String::new();
    for component in name.split("::") {
        if !chain.is_empty() {
            chain.push_str("::");
        }
        chain.push_str(component);
        let id = stable_deck_id(&chain);
        decks.insert(
            id.to_string(),
            json!({
                "collapsed": false,
                "conf": 1,
                "desc": "",
                "dyn": 0,
                "extendNew": 0,
                "extendRev": 50,
                "id": id,
                "lrnToday": [0, 0],
                "mod": now_s,
                "name": chain,
                "newToday": [0, 0],
                "revToday": [0, 0],
                "timeToday": [0, 0],
                "usn": -1,
            }),
        );
    }

    Value::Object(decks)
}

/// `col.models`：只登记实际用到的模型，避免给用户的 collection 留下无主 note type。
fn models_json(
    plan: &DeckPlan,
    leaf_id: i64,
    now_s: i64,
    uses_basic: bool,
    uses_cloze: bool,
) -> Value {
    let mut models = serde_json::Map::new();
    if uses_basic {
        models.insert(BASIC_MODEL_ID.to_string(), model_json(
            BASIC_MODEL_ID,
            "AstrCode Basic",
            false,
            plan.css.as_deref().unwrap_or(DEFAULT_CSS),
            leaf_id,
            now_s,
        ));
    }
    if uses_cloze {
        let css = match &plan.css {
            Some(css) => css.clone(),
            None => format!("{DEFAULT_CSS}{CLOZE_CSS_EXTRA}"),
        };
        models.insert(CLOZE_MODEL_ID.to_string(), model_json(
            CLOZE_MODEL_ID,
            "AstrCode Cloze",
            true,
            &css,
            leaf_id,
            now_s,
        ));
    }
    Value::Object(models)
}

/// 单个模型的 JSON（字段对齐 genanki `Model.to_json`）。
fn model_json(
    model_id: i64,
    name: &str,
    cloze: bool,
    css: &str,
    deck_id: i64,
    now_s: i64,
) -> Value {
    let (model_type, field_names, template) = if cloze {
        (
            1,
            vec!["Text", "Back Extra"],
            json!({
                "name": "Cloze",
                "ord": 0,
                "qfmt": "{{cloze:Text}}",
                "afmt": "{{cloze:Text}}<br>\n{{Back Extra}}",
                "bqfmt": "",
                "bafmt": "",
                "bfont": "",
                "bsize": 0,
                "did": Value::Null,
            }),
        )
    } else {
        (
            0,
            vec!["Front", "Back"],
            json!({
                "name": "Card 1",
                "ord": 0,
                "qfmt": "{{Front}}",
                "afmt": "{{FrontSide}}\n\n<hr id=answer>\n\n{{Back}}",
                "bqfmt": "",
                "bafmt": "",
                "bfont": "",
                "bsize": 0,
                "did": Value::Null,
            }),
        )
    };

    let flds: Vec<Value> = field_names
        .iter()
        .enumerate()
        .map(|(ord, field_name)| {
            json!({
                "name": field_name,
                "ord": ord,
                "font": "Arial",
                "media": [],
                "rtl": false,
                "size": 20,
                "sticky": false,
            })
        })
        .collect();

    json!({
        "css": css,
        "did": deck_id,
        "flds": flds,
        "id": model_id.to_string(),
        "latexPost": DEFAULT_LATEX_POST,
        "latexPre": DEFAULT_LATEX_PRE,
        "latexsvg": false,
        "mod": now_s,
        "name": name,
        // req 对 cloze 模型会被 Anki 忽略（type 1），这里与 genanki 对 Front 单字段的
        // 计算结果保持一致：front 模板只引用第一个字段。
        "req": [[0, "all", [0]]],
        "sortf": 0,
        "tags": [],
        "tmpls": [template],
        "type": model_type,
        "usn": -1,
        "vers": [],
    })
}

/// `col.conf`（对齐 genanki APKG_COL，仅 curModel 换成实际用到的模型）。
fn conf_json(cur_model_id: i64) -> Value {
    json!({
        "activeDecks": [1],
        "addToCur": true,
        "collapseTime": 1200,
        "curDeck": 1,
        "curModel": cur_model_id.to_string(),
        "dueCounts": true,
        "estTimes": true,
        "newBury": true,
        "newSpread": 0,
        "nextPos": 1,
        "sortBackwards": false,
        "sortType": "noteFld",
        "timeLim": 0,
    })
}

/// `col.dconf`（对齐 genanki APKG_COL 的默认牌组配置）。
fn dconf_json() -> Value {
    json!({
        "1": {
            "autoplay": true,
            "id": 1,
            "lapse": {
                "delays": [10],
                "leechAction": 0,
                "leechFails": 8,
                "minInt": 1,
                "mult": 0,
            },
            "maxTaken": 60,
            "mod": 0,
            "name": "Default",
            "new": {
                "bury": true,
                "delays": [1, 10],
                "initialFactor": 2500,
                "ints": [1, 4, 7],
                "order": 1,
                "perDay": 20,
                "separate": true,
            },
            "replayq": true,
            "rev": {
                "bury": true,
                "ease4": 1.3,
                "fuzz": 0.05,
                "ivlFct": 1,
                "maxIvl": 36500,
                "minSpace": 1,
                "perDay": 100,
            },
            "timer": 0,
            "usn": 0,
        }
    })
}

/// 打 zip 外壳：collection + media manifest + 媒体文件。返回打包的媒体数。
fn write_zip(output: &Path, db_bytes: &[u8], plan: &DeckPlan) -> Result<usize, DeckError> {
    if let Some(parent) = output.parent() {
        std::fs::create_dir_all(parent).map_err(|error| {
            DeckError::new(ErrorCode::Io, format!("create output dir: {error}"))
        })?;
    }
    let file = std::fs::File::create(output).map_err(|error| {
        DeckError::new(ErrorCode::Io, format!("create output file: {error}"))
    })?;
    let mut zip = zip::ZipWriter::new(file);
    let options = zip::write::SimpleFileOptions::default();

    zip.start_file("collection.anki2", options)
        .map_err(|error| DeckError::new(ErrorCode::Packaging, format!("zip collection: {error}")))?;
    zip.write_all(db_bytes)
        .map_err(|error| DeckError::new(ErrorCode::Packaging, format!("write collection: {error}")))?;

    let manifest: serde_json::Map<String, Value> = plan
        .media
        .iter()
        .enumerate()
        .map(|(index, entry)| (index.to_string(), Value::String(entry.basename.clone())))
        .collect();
    zip.start_file("media", options)
        .map_err(|error| DeckError::new(ErrorCode::Packaging, format!("zip media manifest: {error}")))?;
    zip.write_all(Value::Object(manifest).to_string().as_bytes())
        .map_err(|error| DeckError::new(ErrorCode::Packaging, format!("write media manifest: {error}")))?;

    for (index, entry) in plan.media.iter().enumerate() {
        let bytes = std::fs::read(&entry.source).map_err(|error| {
            DeckError::new(
                ErrorCode::MediaMissing,
                format!("read media {:?}: {error}", entry.source),
            )
        })?;
        zip.start_file(index.to_string(), options)
            .map_err(|error| {
                DeckError::new(
                    ErrorCode::Packaging,
                    format!("zip media {} ({}): {error}", index, entry.basename),
                )
            })?;
        zip.write_all(&bytes)
            .map_err(|error| {
                DeckError::new(
                    ErrorCode::Packaging,
                    format!("write media {} ({}): {error}", index, entry.basename),
                )
            })?;
    }

    zip.finish()
        .map_err(|error| DeckError::new(ErrorCode::Packaging, format!("finish zip: {error}")))?;
    Ok(plan.media.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spec::{CardSpec, DeckSpec, DeckSpecArgs};
    use std::io::Read;

    fn temp_dir(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "anki-deck-test-{}-{}-{label}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).expect("建临时目录");
        dir
    }

    fn card(front: &str, back: &str) -> CardSpec {
        CardSpec {
            front: front.to_owned(),
            back: back.to_owned(),
            cloze: false,
            tags: Vec::new(),
            id: None,
        }
    }

    fn spec(name: &str, cards: Vec<CardSpec>, media: Vec<String>) -> DeckSpecArgs {
        DeckSpecArgs {
            output: String::from("out.apkg"),
            deck: DeckSpec {
                name: name.to_owned(),
                cards,
                media,
                css: None,
            },
        }
    }

    /// 打开产出的 zip 并返回 (collection 字节, media manifest)。
    fn unzip_collection(output: &Path) -> (Vec<u8>, serde_json::Map<String, Value>) {
        let file = std::fs::File::open(output).expect("打开 apkg");
        let mut archive = zip::ZipArchive::new(file).expect("解包 apkg");

        let mut collection = Vec::new();
        archive
            .by_name("collection.anki2")
            .expect("zip 里有 collection.anki2")
            .read_to_end(&mut collection)
            .expect("读 collection");

        let manifest: serde_json::Map<String, Value> =
            serde_json::from_reader(archive.by_name("media").expect("zip 里有 media"))
                .expect("media 是 JSON");
        (collection, manifest)
    }

    /// 把 collection 字节当 SQLite 打开，执行查询。
    fn query_collection(collection: &[u8], sql: &str) -> Vec<Vec<Value>> {
        // rusqlite 没有 deserialize API：先把字节落盘再读。测试沙盒内可接受。
        let temp = std::env::temp_dir().join(format!(
            "anki-deck-test-read-{}-{}",
            std::process::id(),
            SystemTime::now().duration_since(UNIX_EPOCH).expect("clock").as_nanos()
        ));
        std::fs::write(&temp, collection).expect("落盘 collection");
        let connection = Connection::open(&temp).expect("打开 collection");
        let mut rows = Vec::new();
        let mut statement = connection.prepare(sql).expect("准备查询");
        let column_count = statement.column_count();
        let mut results = statement
            .query([])
            .expect("执行查询");
        while let Ok(Some(row)) = results.next() {
            let mut values = Vec::new();
            for column in 0..column_count {
                let value: Value = match row.get_ref(column).expect("取列") {
                    rusqlite::types::ValueRef::Null => Value::Null,
                    rusqlite::types::ValueRef::Integer(n) => json!(n),
                    rusqlite::types::ValueRef::Real(f) => json!(f),
                    rusqlite::types::ValueRef::Text(text) => {
                        json!(String::from_utf8_lossy(text).to_string())
                    }
                    rusqlite::types::ValueRef::Blob(_) => json!("<blob>"),
                };
                values.push(value);
            }
            rows.push(values);
        }
        let _ = std::fs::remove_file(&temp);
        rows
    }

    #[test]
    fn a_full_build_produces_an_importable_looking_apkg() {
        let dir = temp_dir("full");
        let media_path = dir.join("heap.png");
        std::fs::write(&media_path, b"png-bytes").expect("写媒体");

        let mut cloze_card = card("A lifetime is {{c1::how long}} and {{c2::why}}", "extra");
        cloze_card.cloze = true;
        cloze_card.tags = vec![String::from("rust")];
        cloze_card.id = Some(String::from("vault:notes/rust.md#lifetime"));

        let args = spec(
            "Rust::Ownership",
            vec![card("What is a lifetime?", "How long a value lives"), cloze_card],
            vec![media_path.display().to_string()],
        );

        let plan = build_plan(&args, &dir).expect("build plan");
        let output = dir.join("out.apkg");
        let stats = write_apkg(&plan, &output).expect("写 apkg");
        assert_eq!(stats.notes, 2);
        assert_eq!(stats.cloze_notes, 1);
        // basic 1 张卡 + cloze 2 个序号 = 3 张卡。
        assert_eq!(stats.cards, 3);
        assert_eq!(stats.media, 1);

        let (collection, manifest) = unzip_collection(&output);

        let col = query_collection(&collection, "SELECT ver, crt FROM col");
        assert_eq!(col.len(), 1);
        assert_eq!(col[0][0], json!(11));

        let notes =
            query_collection(&collection, "SELECT guid, mid, flds, tags FROM notes ORDER BY id");
        assert_eq!(notes.len(), 2);

        let cards = query_collection(&collection, "SELECT did, ord FROM cards ORDER BY id");
        assert_eq!(cards.len(), 3);

        let decks_text = query_collection(&collection, "SELECT decks FROM col")[0][0]
            .as_str()
            .expect("decks 是 JSON 文本")
            .to_owned();
        let decks: serde_json::Map<String, Value> =
            serde_json::from_str(&decks_text).expect("解析 decks");
        // 父链逐级注册 + Default。
        assert!(decks.contains_key("1"));
        assert!(decks.values().any(|d| d["name"] == json!("Rust")));
        assert!(decks.values().any(|d| d["name"] == json!("Rust::Ownership")));

        let models_text = query_collection(&collection, "SELECT models FROM col")[0][0]
            .as_str()
            .expect("models 是 JSON 文本")
            .to_owned();
        let models: serde_json::Map<String, Value> =
            serde_json::from_str(&models_text).expect("解析 models");
        assert!(models.contains_key(BASIC_MODEL_ID.to_string().as_str()));
        assert!(models.contains_key(CLOZE_MODEL_ID.to_string().as_str()));

        // 挖空模型的模板必须引用 {{cloze:Text}}。
        let cloze_model = &models[CLOZE_MODEL_ID.to_string().as_str()];
        assert_eq!(cloze_model["type"], json!(1));
        assert_eq!(cloze_model["tmpls"][0]["qfmt"], json!("{{cloze:Text}}"));

        // 媒体：manifest 把索引映射到 basename，正文条目里是文件本体。
        assert_eq!(manifest.get("0"), Some(&json!("heap.png")));
        let mut media_bytes = Vec::new();
        {
            let file = std::fs::File::open(&output).expect("打开 apkg");
            let mut archive = zip::ZipArchive::new(file).expect("解包 apkg");
            archive
                .by_name("0")
                .expect("zip 里有媒体条目 0")
                .read_to_end(&mut media_bytes)
                .expect("读媒体");
        }
        assert_eq!(media_bytes, b"png-bytes");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn guid_is_stable_across_builds_with_the_same_identity() {
        let mut card_spec = card("q", "a");
        card_spec.id = Some(String::from("vault:notes/a.md#q"));
        let args = spec("Rust", vec![clone_card(&card_spec)], Vec::new());
        let guid_one = build_plan(&args, Path::new("/nonexistent"))
            .expect("build plan")
            .cards[0]
            .guid
            .clone();
        let plan = build_plan(&args, Path::new("/nonexistent")).expect("build plan");
        assert_eq!(plan.cards[0].guid, guid_one);
    }

    #[test]
    fn same_id_in_different_decks_yields_different_guids() {
        let mut card_spec = card("q", "a");
        card_spec.id = Some(String::from("vault:notes/a.md#q"));
        let plan_a = build_plan(&spec("Rust", vec![clone_card(&card_spec)], Vec::new()), Path::new("/"))
            .expect("build plan");
        let plan_b = build_plan(
            &spec("Rust::Lifetimes", vec![clone_card(&card_spec)], Vec::new()),
            Path::new("/"),
        )
        .expect("build plan");
        assert_ne!(plan_a.cards[0].guid, plan_b.cards[0].guid);
    }

    #[test]
    fn missing_media_is_a_domain_error() {
        let args = spec("Rust", vec![card("q", "a")], vec![String::from("nope.png")]);
        let error = build_plan(&args, Path::new("/nonexistent")).expect_err("应报媒体缺失");
        assert_eq!(error.code, ErrorCode::MediaMissing);
    }

    #[test]
    fn duplicate_media_basenames_are_rejected() {
        let dir = temp_dir("dup");
        std::fs::create_dir_all(dir.join("a")).expect("建目录 a");
        std::fs::create_dir_all(dir.join("b")).expect("建目录 b");
        std::fs::write(dir.join("a/img.png"), b"one").expect("写媒体一");
        std::fs::write(dir.join("b/img.png"), b"two").expect("写媒体二");
        let args = spec(
            "Rust",
            vec![card("q", "a")],
            vec![
                dir.join("a/img.png").display().to_string(),
                dir.join("b/img.png").display().to_string(),
            ],
        );
        let error = build_plan(&args, &dir).expect_err("应报媒体重名");
        assert_eq!(error.code, ErrorCode::MediaDuplicate);
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn clone_card(card: &CardSpec) -> CardSpec {
        CardSpec {
            front: card.front.clone(),
            back: card.back.clone(),
            cloze: card.cloze,
            tags: card.tags.clone(),
            id: card.id.clone(),
        }
    }
}
