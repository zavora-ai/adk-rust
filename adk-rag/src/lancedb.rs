//! LanceDB vector store backend.
//!
//! Provides [`LanceDBVectorStore`] which implements [`VectorStore`] using
//! the [lancedb](https://docs.rs/lancedb) crate in embedded mode.
//!
//! # Example
//!
//! ```rust,ignore
//! use adk_rag::lancedb::LanceDBVectorStore;
//! use adk_rag::{CollectionSpec, Scope, VectorQuery, VectorStore};
//!
//! let store = LanceDBVectorStore::new("data/my-vectors").await?;
//! let scope = Scope::single_tenant();
//! store.create_collection("docs", &CollectionSpec::new(384)).await?;
//! store.upsert("docs", &scope, &chunks).await?;
//! let results = store.search("docs", &scope, &VectorQuery::new(&query_embedding, 5)).await?;
//! ```

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use futures::TryStreamExt;
use lancedb::Connection;
// All arrow types come from lancedb's own re-exports, so their arrow version is stated
// exactly once (by lancedb) and cannot drift out from under us. See the comment on the
// `lancedb` dependency in Cargo.toml.
use lancedb::arrow::arrow_array::cast::AsArray;
use lancedb::arrow::arrow_array::types::Float32Type;
use lancedb::arrow::arrow_array::{
    Array, FixedSizeListArray, Float32Array, RecordBatch, RecordBatchIterator, RecordBatchReader,
    StringArray,
};
use lancedb::arrow::arrow_schema::{DataType, Field, Schema};
use lancedb::expr::{DfExpr, col, is_in, lit};
use lancedb::query::{ExecutableQuery, QueryBase, Select};
use tracing::debug;

use crate::document::{Chunk, SearchResult};
use crate::error::{RagError, Result};
use crate::filter::{self, Filter};
use crate::scope::Scope;
use crate::vectorstore::{CollectionSpec, VectorQuery, VectorStore};

/// Prefix of the column that holds a filterable metadata key.
const METADATA_COLUMN_PREFIX: &str = "m_";

/// A [`VectorStore`] backed by [LanceDB](https://lancedb.github.io/lancedb/).
///
/// Wraps a [`lancedb::Connection`] and maps collections to LanceDB tables.
/// Runs in embedded mode by default using a local directory for storage.
///
/// `upsert` replaces rows by chunk id through a merge-insert, so ingesting the
/// same chunks again updates them instead of adding duplicates. Every embedding
/// in an `upsert` must match the collection's dimensions.
///
/// Rows are keyed by `scope` and `id`: `upsert` merges on both, and search, delete,
/// count, and purge select the scope with a typed predicate that LanceDB applies before
/// the nearest rows are chosen. A chunk's `source_uri` and `title` are columns of their
/// own. A table created by an earlier release, without these columns, fails `upsert`
/// with guidance to re-create it and re-ingest its documents.
///
/// Each key in [`CollectionSpec::filterable`] is a nullable column `m_<key>` that `upsert`
/// fills from the chunk's metadata, and a [`Filter`] compiles to a typed DataFusion
/// expression over these columns that LanceDB applies before the nearest rows are chosen.
/// A filter on a key the table has no column for fails with
/// [`RagError::UnsupportedFilter`].
pub struct LanceDBVectorStore {
    connection: Connection,
}

impl LanceDBVectorStore {
    /// Create a new LanceDB vector store at the given storage path.
    pub async fn new(path: &str) -> std::result::Result<Self, lancedb::Error> {
        let connection = lancedb::connect(path).execute().await?;
        Ok(Self { connection })
    }

    /// Create a new LanceDB vector store from an existing connection.
    pub fn from_connection(connection: Connection) -> Self {
        Self { connection }
    }

    fn map_err(e: lancedb::Error) -> RagError {
        RagError::VectorStoreError { backend: "lancedb".to_string(), message: e.to_string() }
    }

    fn store_error(message: String) -> RagError {
        RagError::VectorStoreError { backend: "lancedb".to_string(), message }
    }

    /// Checks a collection name against LanceDB's table-name rule before any call.
    ///
    /// lancedb 0.31 panics instead of erroring when it creates or opens a table whose
    /// name breaks the rule.
    fn table_name(name: &str) -> Result<&str> {
        if name.is_empty()
            || !name.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'))
        {
            return Err(Self::store_error(format!(
                "invalid collection name `{name}`: use one or more ASCII letters, digits, \
                 underscores, hyphens, or periods"
            )));
        }
        Ok(name)
    }

    /// Builds the predicate that selects a scope's rows.
    ///
    /// The scope becomes a typed string literal in a DataFusion expression rather than text
    /// spliced into SQL, so a quote in a scope cannot alter the predicate.
    fn scope_predicate(scope: &Scope) -> DfExpr {
        col("scope").eq(lit(scope.as_str()))
    }

    /// Builds the predicate that selects a scope's rows by chunk id, with every id a typed
    /// string literal.
    fn id_predicate(scope: &Scope, ids: &[&str]) -> DfExpr {
        Self::scope_predicate(scope).and(is_in(col("id"), ids.iter().map(|id| lit(*id)).collect()))
    }

    async fn open(&self, collection: &str) -> Result<lancedb::Table> {
        self.connection
            .open_table(Self::table_name(collection)?)
            .execute()
            .await
            .map_err(Self::map_err)
    }

    /// Physically removes the rows earlier deletes in `collection` marked as deleted, and
    /// every older version of the table, returning the bytes freed.
    ///
    /// LanceDB's `delete` is logical: it writes a new table version whose fragments mark
    /// the rows deleted, while the data files and older versions still hold them.
    /// [`VectorStore::delete_where`], `delete`, and `purge_scope` stop at that; erasure that
    /// must remove the bytes calls this afterwards. It rewrites every fragment that has a
    /// deleted row, then prunes all versions but the current one, so the table can no longer
    /// be checked out at an earlier version.
    ///
    /// **Important:** run it only while no other process writes to the collection: pruning
    /// removes files of every older version, including one a concurrent writer still uses.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use adk_rag::{Field, Filter, Scope, VectorStore};
    ///
    /// let subject = Filter::eq(Field::metadata("subject"), "visitor-42");
    /// store.delete_where("docs", &Scope::single_tenant(), &subject).await?;
    /// let freed = store.purge_deleted("docs").await?;
    /// ```
    ///
    /// # Errors
    ///
    /// Returns [`RagError::VectorStoreError`] when the table cannot be opened, compacted, or
    /// pruned.
    pub async fn purge_deleted(&self, collection: &str) -> Result<u64> {
        let table = self.open(collection).await?;
        let compaction = lancedb::table::CompactionOptions {
            materialize_deletions: true,
            materialize_deletions_threshold: 0.0,
            ..Default::default()
        };
        table
            .optimize(lancedb::table::OptimizeAction::Compact {
                options: compaction,
                remap_options: None,
            })
            .await
            .map_err(Self::map_err)?;
        let pruned = table
            .optimize(lancedb::table::OptimizeAction::Prune {
                older_than: Some(lancedb::table::optimize::Duration::zero()),
                delete_unverified: Some(true),
                error_if_tagged_old_versions: Some(true),
            })
            .await
            .map_err(Self::map_err)?;
        let freed = pruned.prune.map_or(0, |stats| stats.bytes_removed);
        debug!(collection, bytes.freed = freed, "purged deleted lancedb rows");
        Ok(freed)
    }

    /// The column holding a filterable metadata key.
    fn metadata_column(key: &str) -> String {
        format!("{METADATA_COLUMN_PREFIX}{key}")
    }

    /// Compiles `filter` to a typed expression over `schema`'s columns.
    ///
    /// Every leaf is two-valued (`IS NOT NULL AND ..`), so `NOT` matches a row that lacks
    /// the field, as [`Filter::matches`] does.
    fn filter_expr(collection: &str, schema: &Schema, filter: &Filter) -> Result<DfExpr> {
        let column = |field: &filter::Field| -> Result<DfExpr> {
            Ok(match field {
                filter::Field::Metadata(key) => {
                    let name = Self::metadata_column(key);
                    if schema.field_with_name(&name).is_err() {
                        return Err(RagError::UnsupportedFilter(format!(
                            "metadata key `{key}` is not declared filterable on collection \
                             '{collection}'; create the collection with it in \
                             `CollectionSpec::with_filterable`"
                        )));
                    }
                    col(name)
                }
                filter::Field::DocumentId => col("document_id"),
                filter::Field::SourceUri => col("source_uri"),
                filter::Field::ChunkId => col("id"),
            })
        };
        Ok(match filter {
            Filter::Eq(field, value) => {
                let column = column(field)?;
                column.clone().is_not_null().and(column.eq(lit(value.as_str())))
            }
            Filter::In(field, values) => {
                let column = column(field)?;
                let values = values.iter().map(|value| lit(value.as_str())).collect();
                column.clone().is_not_null().and(column.in_list(values, false))
            }
            Filter::Exists(field) => column(field)?.is_not_null(),
            Filter::And(filters) | Filter::Or(filters) => {
                let mut exprs = filters
                    .iter()
                    .map(|filter| Self::filter_expr(collection, schema, filter))
                    .collect::<Result<Vec<_>>>()?
                    .into_iter();
                let first = exprs.next().ok_or_else(|| {
                    RagError::UnsupportedFilter("an `And` or `Or` filter needs a filter".into())
                })?;
                if matches!(filter, Filter::And(_)) {
                    exprs.fold(first, DfExpr::and)
                } else {
                    exprs.fold(first, DfExpr::or)
                }
            }
            Filter::Not(filter) => !Self::filter_expr(collection, schema, filter)?,
        })
    }

    /// The scope predicate, joined with `filter` compiled over the table's columns.
    async fn predicate(
        collection: &str,
        table: &lancedb::Table,
        scope: &Scope,
        filter: Option<&Filter>,
    ) -> Result<DfExpr> {
        let Some(filter) = filter else {
            return Ok(Self::scope_predicate(scope));
        };
        filter.validate()?;
        let schema = table.schema().await.map_err(Self::map_err)?;
        Ok(Self::scope_predicate(scope).and(Self::filter_expr(collection, &schema, filter)?))
    }

    fn build_schema<'a>(
        dimensions: usize,
        filterable: impl IntoIterator<Item = &'a str>,
    ) -> Arc<Schema> {
        let declared = filterable
            .into_iter()
            .map(|key| Field::new(Self::metadata_column(key), DataType::Utf8, true));
        Arc::new(Schema::new(
            [
                Field::new("scope", DataType::Utf8, false),
                Field::new("id", DataType::Utf8, false),
                Field::new("text", DataType::Utf8, false),
                Field::new(
                    "vector",
                    DataType::FixedSizeList(
                        Arc::new(Field::new("item", DataType::Float32, true)),
                        dimensions as i32,
                    ),
                    true,
                ),
                Field::new("document_id", DataType::Utf8, false),
                Field::new("metadata_json", DataType::Utf8, false),
                Field::new("source_uri", DataType::Utf8, true),
                Field::new("title", DataType::Utf8, true),
            ]
            .into_iter()
            .chain(declared)
            .collect::<Vec<_>>(),
        ))
    }
}

#[async_trait]
impl VectorStore for LanceDBVectorStore {
    async fn create_collection(&self, name: &str, spec: &CollectionSpec) -> Result<()> {
        spec.validate()?;
        let name = Self::table_name(name)?;
        let dimensions = spec.dimensions;
        let tables = self.connection.table_names().execute().await.map_err(Self::map_err)?;
        if tables.iter().any(|t| t == name) {
            debug!(collection = name, "lancedb table already exists, skipping creation");
            return Ok(());
        }

        let schema = Self::build_schema(dimensions, spec.filterable.iter().map(String::as_str));
        self.connection.create_empty_table(name, schema).execute().await.map_err(Self::map_err)?;

        debug!(collection = name, dimensions, "created lancedb table");
        Ok(())
    }

    async fn delete_collection(&self, name: &str) -> Result<()> {
        self.connection.drop_table(Self::table_name(name)?, &[]).await.map_err(Self::map_err)?;
        debug!(collection = name, "deleted lancedb table");
        Ok(())
    }

    async fn upsert(&self, collection: &str, scope: &Scope, chunks: &[Chunk]) -> Result<()> {
        if chunks.is_empty() {
            return Ok(());
        }

        // Merge-insert rejects a source batch with two rows for one id; the last
        // occurrence wins, matching the other backends.
        let mut last_index: HashMap<&str, usize> = HashMap::with_capacity(chunks.len());
        for (i, chunk) in chunks.iter().enumerate() {
            last_index.insert(chunk.id.as_str(), i);
        }
        let chunks: Vec<&Chunk> = chunks
            .iter()
            .enumerate()
            .filter(|(i, chunk)| last_index[chunk.id.as_str()] == *i)
            .map(|(_, chunk)| chunk)
            .collect();

        let table = self.open(collection).await?;
        let table_schema = table.schema().await.map_err(Self::map_err)?;
        let dimensions = match table_schema.field_with_name("vector").map(Field::data_type) {
            Ok(DataType::FixedSizeList(_, size)) => *size as usize,
            _ => {
                return Err(Self::store_error(format!(
                    "collection '{collection}' has no fixed-size `vector` column; \
                     create it with `create_collection`"
                )));
            }
        };
        let declared: Vec<&str> = table_schema
            .fields()
            .iter()
            .filter_map(|field| field.name().strip_prefix(METADATA_COLUMN_PREFIX))
            .collect();
        let schema = Self::build_schema(dimensions, declared.iter().copied());
        if let Some(missing) =
            schema.fields().iter().find(|field| table_schema.field_with_name(field.name()).is_err())
        {
            return Err(Self::store_error(format!(
                "collection '{collection}' was created by an earlier release and has no `{}` \
                 column; delete it, create it again, and re-ingest its documents",
                missing.name()
            )));
        }
        if let Some(chunk) = chunks.iter().find(|c| c.embedding.len() != dimensions) {
            return Err(Self::store_error(format!(
                "chunk '{}' has a {}-dimensional embedding but collection '{collection}' \
                 stores {dimensions} dimensions",
                chunk.id,
                chunk.embedding.len()
            )));
        }
        let scopes = vec![scope.as_str(); chunks.len()];
        let ids: Vec<&str> = chunks.iter().map(|c| c.id.as_str()).collect();
        let texts: Vec<&str> = chunks.iter().map(|c| c.text.as_str()).collect();
        let doc_ids: Vec<&str> = chunks.iter().map(|c| c.document_id.as_str()).collect();
        let metadata_jsons: Vec<String> = chunks
            .iter()
            .map(|c| serde_json::to_string(&c.metadata).unwrap_or_else(|_| "{}".to_string()))
            .collect();
        let metadata_refs: Vec<&str> = metadata_jsons.iter().map(|s| s.as_str()).collect();
        let source_uris: Vec<Option<&str>> =
            chunks.iter().map(|c| c.source_uri.as_deref()).collect();
        let titles: Vec<Option<&str>> = chunks.iter().map(|c| c.title.as_deref()).collect();

        let all_values: Vec<f32> =
            chunks.iter().flat_map(|c| c.embedding.iter().copied()).collect();
        let values_array = Arc::new(Float32Array::from(all_values));
        let list_field = Arc::new(Field::new("item", DataType::Float32, true));
        let vector_array =
            FixedSizeListArray::try_new(list_field, dimensions as i32, values_array, None)
                .map_err(|e| Self::store_error(format!("failed to build vector column: {e}")))?;

        let batch = RecordBatch::try_new(
            schema.clone(),
            vec![
                Arc::new(StringArray::from(scopes)),
                Arc::new(StringArray::from(ids)),
                Arc::new(StringArray::from(texts)),
                Arc::new(vector_array) as Arc<dyn Array>,
                Arc::new(StringArray::from(doc_ids)),
                Arc::new(StringArray::from(metadata_refs)),
                Arc::new(StringArray::from(source_uris)),
                Arc::new(StringArray::from(titles)),
            ]
            .into_iter()
            .chain(declared.iter().map(|key| {
                let values: Vec<Option<&str>> =
                    chunks.iter().map(|c| c.metadata.get(*key).map(String::as_str)).collect();
                Arc::new(StringArray::from(values)) as Arc<dyn Array>
            }))
            .collect(),
        )
        .map_err(|e| Self::store_error(format!("failed to build record batch: {e}")))?;

        let batches: Box<dyn RecordBatchReader + Send> =
            Box::new(RecordBatchIterator::new(vec![Ok(batch)], schema));
        let mut merge = table.merge_insert(&["scope", "id"]);
        merge.when_matched_update_all(None).when_not_matched_insert_all();
        merge.execute(batches).await.map_err(Self::map_err)?;

        debug!(collection, count = chunks.len(), "upserted chunks to lancedb");
        Ok(())
    }

    async fn delete(&self, collection: &str, scope: &Scope, ids: &[&str]) -> Result<()> {
        if ids.is_empty() {
            return Ok(());
        }

        let table = self.open(collection).await?;
        table.delete(&Self::id_predicate(scope, ids)).await.map_err(Self::map_err)?;

        debug!(collection, count = ids.len(), "deleted chunks from lancedb");
        Ok(())
    }

    async fn search(
        &self,
        collection: &str,
        scope: &Scope,
        query: &VectorQuery<'_>,
    ) -> Result<Vec<SearchResult>> {
        let table = self.open(collection).await?;

        // A vector query prefilters by default: the scope applies before the nearest rows
        // are chosen.
        let predicate = Self::predicate(collection, &table, scope, query.filter).await?;
        let stream = table
            .query()
            .only_if_expr(predicate)
            .nearest_to(query.embedding)
            .map_err(|e| RagError::VectorStoreError {
                backend: "lancedb".to_string(),
                message: format!("failed to build vector query: {e}"),
            })?
            .limit(query.top_k)
            .execute()
            .await
            .map_err(Self::map_err)?;

        let batches: Vec<RecordBatch> =
            stream.try_collect().await.map_err(|e| RagError::VectorStoreError {
                backend: "lancedb".to_string(),
                message: format!("failed to collect search results: {e}"),
            })?;

        let mut results = Vec::new();
        for batch in &batches {
            let id_col = batch.column_by_name("id");
            let text_col = batch.column_by_name("text");
            let doc_id_col = batch.column_by_name("document_id");
            let metadata_col = batch.column_by_name("metadata_json");
            let source_uri_col = batch.column_by_name("source_uri");
            let title_col = batch.column_by_name("title");
            let distance_col = batch.column_by_name("_distance");
            let optional = |column: Option<&Arc<dyn Array>>, i: usize| {
                column
                    .and_then(|c| c.as_string_opt::<i32>())
                    .filter(|a: &&StringArray| a.is_valid(i))
                    .map(|a| a.value(i).to_string())
            };

            for i in 0..batch.num_rows() {
                let id = id_col
                    .and_then(|c| c.as_string_opt::<i32>())
                    .map(|a: &StringArray| a.value(i).to_string())
                    .unwrap_or_default();

                let text = text_col
                    .and_then(|c| c.as_string_opt::<i32>())
                    .map(|a: &StringArray| a.value(i).to_string())
                    .unwrap_or_default();

                let document_id = doc_id_col
                    .and_then(|c| c.as_string_opt::<i32>())
                    .map(|a: &StringArray| a.value(i).to_string())
                    .unwrap_or_default();

                let metadata: HashMap<String, String> = metadata_col
                    .and_then(|c| c.as_string_opt::<i32>())
                    .and_then(|a: &StringArray| serde_json::from_str(a.value(i)).ok())
                    .unwrap_or_default();

                let distance = distance_col
                    .and_then(|c| c.as_primitive_opt::<Float32Type>())
                    .map(|a| a.value(i))
                    .unwrap_or(0.0);
                let score = 1.0 - distance;

                let mut chunk = Chunk::new(id, document_id, text);
                chunk.metadata = metadata;
                chunk.source_uri = optional(source_uri_col, i);
                chunk.title = optional(title_col, i);
                results.push(SearchResult { chunk, score });
            }
        }

        Ok(results)
    }

    async fn count(&self, collection: &str, scope: &Scope, filter: Option<&Filter>) -> Result<u64> {
        let table = self.open(collection).await?;
        let predicate = Self::predicate(collection, &table, scope, filter).await?;
        let batches: Vec<RecordBatch> = table
            .query()
            .only_if_expr(predicate)
            .select(Select::columns(&["id"]))
            .execute()
            .await
            .map_err(Self::map_err)?
            .try_collect()
            .await
            .map_err(|e| Self::store_error(format!("failed to count rows: {e}")))?;
        Ok(batches.iter().map(|batch| batch.num_rows() as u64).sum())
    }

    async fn delete_where(&self, collection: &str, scope: &Scope, filter: &Filter) -> Result<u64> {
        let table = self.open(collection).await?;
        let predicate = Self::predicate(collection, &table, scope, Some(filter)).await?;
        let deleted = table.delete(&predicate).await.map_err(Self::map_err)?;
        debug!(
            collection,
            deleted = deleted.num_deleted_rows,
            "deleted matching chunks from lancedb"
        );
        Ok(deleted.num_deleted_rows)
    }

    async fn purge_scope(&self, collection: &str, scope: &Scope) -> Result<u64> {
        let table = self.open(collection).await?;
        let deleted = table.delete(&Self::scope_predicate(scope)).await.map_err(Self::map_err)?;
        debug!(collection, purged = deleted.num_deleted_rows, "purged scope from lancedb");
        Ok(deleted.num_deleted_rows)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lancedb::expr::expr_to_sql_string;

    #[test]
    fn predicates_keep_quotes_inside_the_literal() {
        let injection = "x') OR ('1'='1";
        let scope = Scope::new(injection).expect("valid scope");
        assert_eq!(
            LanceDBVectorStore::id_predicate(&scope, &["a", injection]),
            col("scope")
                .eq(lit(injection))
                .and(col("id").in_list(vec![lit("a"), lit(injection)], false))
        );

        let sql = expr_to_sql_string(&LanceDBVectorStore::id_predicate(&scope, &[injection]))
            .expect("render predicate");
        assert_eq!(sql.matches("'x'') OR (''1''=''1'").count(), 2, "quote not escaped: {sql}");
    }
}
