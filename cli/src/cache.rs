use std::collections::HashMap;
use std::path::Path;

use anyhow::Result;
use surrealdb::engine::local::SurrealKv;
use surrealdb::types::SurrealValue;
use surrealdb::Surreal;

// surrealdb 3.x requires `SurrealValue` for types passed to `.content()` / `.take()`;
// it provides (de)serialization to/from SurrealDB `Value`, so serde is not needed.
#[derive(Debug, Clone, SurrealValue)]
pub struct ChunkRecord {
    pub filepath: String,
    pub chunk_index: i64,
    pub mtime: i64,
    pub embedding: Vec<f32>,
    pub preview: String,
}

pub struct Cache {
    db: Surreal<surrealdb::engine::local::Db>,
}

impl Cache {
    pub async fn open(path: &Path) -> Result<Self> {
        let db = Surreal::new::<SurrealKv>(path.to_string_lossy().to_string())
            .await
            .map_err(|e| anyhow::anyhow!("surrealdb connect: {e}"))?;

        db.use_ns("wiki")
            .use_db("embeddings")
            .await
            .map_err(|e| anyhow::anyhow!("surrealdb use_ns/use_db: {e}"))?;

        db.query("DEFINE TABLE IF NOT EXISTS chunks SCHEMALESS")
            .await
            .map_err(|e| anyhow::anyhow!("surrealdb define table: {e}"))?;

        Ok(Self { db })
    }

    pub async fn load_all(&self) -> Result<HashMap<String, (i64, Vec<ChunkRecord>)>> {
        let mut res = self
            .db
            .query("SELECT filepath, chunk_index, mtime, embedding, preview FROM chunks")
            .await
            .map_err(|e| anyhow::anyhow!("surrealdb load_all: {e}"))?;

        let rows: Vec<ChunkRecord> = res
            .take(0)
            .map_err(|e| anyhow::anyhow!("surrealdb take: {e}"))?;

        let mut grouped: HashMap<String, (i64, Vec<ChunkRecord>)> = HashMap::new();
        for rec in rows {
            let entry = grouped
                .entry(rec.filepath.clone())
                .or_insert_with(|| (rec.mtime, Vec::new()));
            entry.1.push(rec);
        }

        for (_, chunks) in grouped.values_mut() {
            chunks.sort_by_key(|c| c.chunk_index);
        }

        Ok(grouped)
    }

    pub async fn put_chunk(&self, rec: &ChunkRecord) -> Result<()> {
        let _: Option<ChunkRecord> = self
            .db
            .create("chunks")
            .content(rec.clone())
            .await
            .map_err(|e| anyhow::anyhow!("surrealdb create: {e}"))?;

        Ok(())
    }

    pub async fn delete_file(&self, filepath: &str) -> Result<()> {
        self.db
            .query("DELETE FROM chunks WHERE filepath = $fp")
            .bind(("fp", filepath.to_string()))
            .await
            .map_err(|e| anyhow::anyhow!("surrealdb delete: {e}"))?;

        Ok(())
    }
}
