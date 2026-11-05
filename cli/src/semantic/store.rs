//! SQLite-хранилище semantic-индекса (пер-машинный файл).
//!
//! WAL + busy_timeout=5s: параллельные сессии фабрики — читатели свободны,
//! писатели сериализуются; upsert по block_id идемпотентен, гонка сходится
//! к тому же состоянию (задача 20261104072044, риск «конкуренция сессий»).

use anyhow::{Context, Result};
use rusqlite::Connection;
use std::path::Path;

use super::sync::sql_literal;

const SCHEMA: &str = "
create table if not exists meta(
    key   text primary key,
    value text not null
);
create table if not exists chunks(
    block_id text primary key,
    root_id  text not null,
    box_id   text not null,
    hpath    text not null,
    updated  text not null,
    content  text not null,
    vec      blob not null
);
";

pub struct Store {
    conn: Connection,
}

/// Строка индекса без вектора — reconcile-diff и метаданные.
#[derive(Clone)]
pub struct ChunkMeta {
    pub block_id: String,
    pub root_id: String,
    pub box_id: String,
    pub hpath: String,
    pub updated: String,
}

/// Строка индекса целиком: upsert дельты/backfill и KNN-выдача.
#[derive(Clone)]
pub struct ChunkRecord {
    pub meta: ChunkMeta,
    pub content: String,
    /// Нормализуется при записи.
    pub vec: Vec<f32>,
}

pub struct Hit {
    pub block_id: String,
    pub hpath: String,
    pub snippet: String,
    pub score: f32,
}

impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)
                .with_context(|| format!("не создать каталог индекса {}", dir.display()))?;
        }
        let conn = Connection::open(path)
            .with_context(|| format!("не открыть индекс {}", path.display()))?;
        conn.pragma_update(None, "journal_mode", "WAL")
            .context("PRAGMA journal_mode=WAL")?;
        conn.busy_timeout(std::time::Duration::from_millis(5000))
            .context("PRAGMA busy_timeout")?;
        conn.pragma_update(None, "synchronous", "NORMAL")
            .context("PRAGMA synchronous=NORMAL")?;
        conn.execute_batch(SCHEMA).context("схема semantic.db")?;
        Ok(Self { conn })
    }

    /// Текущий режим журнала (тест-проверка WAL-прагмы при открытии).
    #[cfg(test)]
    pub(crate) fn journal_mode(&self) -> Result<String> {
        Ok(self
            .conn
            .query_row("pragma journal_mode", [], |r| r.get(0))?)
    }

    pub fn meta(&self, key: &str) -> Result<Option<String>> {
        let mut stmt = self
            .conn
            .prepare("select value from meta where key = ?1 limit 1")?;
        let mut rows = stmt.query([key])?;
        Ok(rows.next()?.map(|r| r.get(0)).transpose()?)
    }

    pub fn set_meta(&self, key: &str, value: &str) -> Result<()> {
        self.conn.execute(
            "insert into meta(key, value) values (?1, ?2)
             on conflict(key) do update set value = excluded.value",
            [key, value],
        )?;
        Ok(())
    }

    pub fn chunk_count(&self) -> Result<u64> {
        Ok(self
            .conn
            .query_row("select count(*) from chunks limit 1", [], |r| {
                r.get::<_, i64>(0)
            })? as u64)
    }

    /// Сброс к пустому индексу перед полным пересбором (sem/rebuild):
    /// чанки и мета уходят, модель/dim зададутся первым батчем нового прогона.
    pub fn clear_for_rebuild(&self) -> Result<()> {
        self.conn.execute("delete from chunks", [])?;
        self.conn.execute("delete from meta", [])?;
        Ok(())
    }

    /// write-транзакция на батч: короткие IMMEDIATE-серии не держат
    /// блокировку дольше самого батча.
    pub fn begin(&mut self) -> Result<rusqlite::Transaction<'_>> {
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        Ok(tx)
    }

    /// Upsert батча + watermark одной транзакцией: индекс и водяной знак
    /// не расходятся (watermark двигается только по записанным строкам).
    pub fn upsert_batch(
        tx: &rusqlite::Transaction<'_>,
        records: &[ChunkRecord],
        watermark: Option<&str>,
    ) -> Result<()> {
        let mut stmt = tx.prepare(
            "insert into chunks(block_id, root_id, box_id, hpath, updated, content, vec)
             values (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             on conflict(block_id) do update set
                root_id = excluded.root_id,
                box_id  = excluded.box_id,
                hpath   = excluded.hpath,
                updated = excluded.updated,
                content = excluded.content,
                vec     = excluded.vec",
        )?;
        for record in records {
            let blob: Vec<u8> = record.vec.iter().flat_map(|c| c.to_le_bytes()).collect();
            stmt.execute(rusqlite::params![
                record.meta.block_id,
                record.meta.root_id,
                record.meta.box_id,
                record.meta.hpath,
                record.meta.updated,
                record.content,
                blob,
            ])?;
        }
        drop(stmt);
        if let Some(mark) = watermark {
            tx.execute(
                "insert into meta(key, value) values ('watermark', ?1)
                 on conflict(key) do update set value = excluded.value",
                [mark],
            )?;
        }
        Ok(())
    }

    /// Метаданные всех строк индекса (reconcile-diff; ~74k лёгких строк).
    pub fn chunk_metas(&self) -> Result<Vec<ChunkMeta>> {
        let mut stmt = self.conn.prepare(
            "select block_id, root_id, box_id, hpath, updated from chunks order by block_id asc",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(ChunkMeta {
                block_id: r.get(0)?,
                root_id: r.get(1)?,
                box_id: r.get(2)?,
                hpath: r.get(3)?,
                updated: r.get(4)?,
            })
        })?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// Prune строк, исчезнувших из ядра (трэш/удаление доков ловится тут же).
    pub fn prune(&mut self, block_ids: &[String]) -> Result<usize> {
        let mut removed = 0;
        for batch in block_ids.chunks(500) {
            let tx = self.begin()?;
            let placeholders = batch
                .iter()
                .map(|id| sql_literal(id))
                .collect::<Vec<_>>()
                .join(",");
            removed += tx.execute(
                &format!("delete from chunks where block_id in ({placeholders})"),
                [],
            )?;
            tx.commit()?;
        }
        Ok(removed)
    }

    /// Дрейф путей: обновить hpath/box/root без переэмбеддинга.
    pub fn refresh_paths(&mut self, metas: &[ChunkMeta]) -> Result<usize> {
        let mut updated = 0;
        for batch in metas.chunks(500) {
            let tx = self.begin()?;
            {
                let mut stmt = tx.prepare(
                    "update chunks set root_id = ?2, box_id = ?3, hpath = ?4
                     where block_id = ?1",
                )?;
                for meta in batch {
                    updated += stmt.execute(rusqlite::params![
                        meta.block_id,
                        meta.root_id,
                        meta.box_id,
                        meta.hpath,
                    ])?;
                }
            }
            tx.commit()?;
        }
        Ok(updated)
    }

    /// Brute-force top-K по скалярному произведению нормированных векторов
    /// (косинус): 54k × 1024 ≈ десятки мс; mmap — запасной план по замеру.
    pub fn knn(&self, query: &[f32], limit: usize, threshold: f32) -> Result<Vec<Hit>> {
        let mut stmt = self
            .conn
            .prepare("select block_id, hpath, content, vec from chunks")?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, Vec<u8>>(3)?,
            ))
        })?;
        let mut hits: Vec<Hit> = Vec::new();
        for row in rows {
            let (block_id, hpath, content, blob) = row?;
            let mut score = 0.0f32;
            let (components, _) = blob.as_chunks::<4>();
            for (i, component) in components.iter().enumerate() {
                if i >= query.len() {
                    break;
                }
                score += f32::from_le_bytes(*component) * query[i];
            }
            if score >= threshold {
                hits.push(Hit {
                    block_id,
                    hpath,
                    snippet: content,
                    score,
                });
            }
        }
        hits.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        hits.truncate(limit);
        Ok(hits)
    }
}
