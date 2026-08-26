//! Shared [`VectorStore`] contract suite and wire-shape assertions for the
//! Agent Retrieval backend, run against an in-process mock of the
//! `vectorsearch.googleapis.com` v1 surface (golden shapes per the REST
//! reference): collection create/delete LROs, data-object CRUD and atomic
//! batches, and `dataObjects:search`/`:batchSearch` with real dot-product
//! scoring.

#![cfg(feature = "agent-retrieval")]

mod common;

use adk_rag::VectorStore;
use adk_rag::agent_retrieval::{AgentRetrievalConfig, AgentRetrievalStore};
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use common::vector_store_contract::{
    ContractOptions, arb_normalized_embedding, arb_unique_chunks, assert_vector_store_contract,
    check_search_invariants,
};
use google_cloud_auth::credentials::api_key_credentials;
use proptest::prelude::*;
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use tokio::sync::Mutex;

const PROJECT: &str = "test-project";
const LOCATION: &str = "us-central1";

#[derive(Default)]
struct MockState {
    /// collectionId → (dataObjectId → stored object body).
    collections: BTreeMap<String, BTreeMap<String, Value>>,
    /// Captured wire bodies for golden assertions.
    create_collection_bodies: Vec<Value>,
    search_bodies: Vec<Value>,
}

type Shared = Arc<Mutex<MockState>>;

fn operation(done_response: Option<Value>) -> Value {
    let mut op = json!({
        "name": format!("projects/{PROJECT}/locations/{LOCATION}/operations/1"),
        "done": true,
    });
    if let Some(response) = done_response {
        op["response"] = response;
    }
    op
}

fn error_status(status: StatusCode, message: &str) -> Response {
    (status, Json(json!({ "error": { "code": status.as_u16(), "message": message } })))
        .into_response()
}

fn dense_values(object: &Value) -> Vec<f64> {
    object["vectors"]["embedding"]["dense"]["values"]
        .as_array()
        .map(|values| values.iter().filter_map(Value::as_f64).collect())
        .unwrap_or_default()
}

fn dot(a: &[f64], b: &[f64]) -> f64 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

fn ranked_vector_results(
    objects: &BTreeMap<String, Value>,
    query: &[f64],
    top_k: usize,
) -> Vec<(String, f64)> {
    let mut scored: Vec<(String, f64)> = objects
        .iter()
        .map(|(id, object)| (id.clone(), dot(&dense_values(object), query)))
        .collect();
    scored.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    scored.truncate(top_k);
    scored
}

fn ranked_text_results(
    objects: &BTreeMap<String, Value>,
    needle: &str,
    top_k: usize,
) -> Vec<(String, f64)> {
    let mut hits: Vec<(String, f64)> = objects
        .iter()
        .filter(|(_, object)| {
            object["data"]["text"].as_str().is_some_and(|text| text.contains(needle))
        })
        .map(|(id, _)| (id.clone(), 1.0))
        .collect();
    hits.sort_by(|a, b| a.0.cmp(&b.0));
    hits.truncate(top_k);
    hits
}

fn results_json(objects: &BTreeMap<String, Value>, ranked: &[(String, f64)]) -> Value {
    let results: Vec<Value> = ranked
        .iter()
        .map(|(id, score)| {
            let mut object = objects[id].clone();
            object["dataObjectId"] = json!(id);
            object["name"] = json!(format!(
                "projects/{PROJECT}/locations/{LOCATION}/collections/c/dataObjects/{id}",
            ));
            json!({ "dataObject": object, "distance": score })
        })
        .collect();
    json!({ "results": results })
}

/// Reciprocal rank fusion over per-search ranked ID lists.
fn rrf_fuse(rankings: &[Vec<(String, f64)>], weights: &[f64], top_k: usize) -> Vec<(String, f64)> {
    const K: f64 = 60.0;
    let mut fused: HashMap<String, f64> = HashMap::new();
    for (ranking, weight) in rankings.iter().zip(weights) {
        for (rank, (id, _)) in ranking.iter().enumerate() {
            *fused.entry(id.clone()).or_default() += weight / (K + rank as f64 + 1.0);
        }
    }
    let mut fused: Vec<(String, f64)> = fused.into_iter().collect();
    fused.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    fused.truncate(top_k);
    fused
}

async fn start_mock(state: Shared) -> String {
    let base = format!("/v1/projects/{PROJECT}/locations/{LOCATION}");
    let app = Router::new()
        .route(
            &format!("{base}/collections"),
            post(
                |State(state): State<Shared>,
                 Query(params): Query<HashMap<String, String>>,
                 Json(body): Json<Value>| async move {
                    let mut state = state.lock().await;
                    let id = params.get("collectionId").cloned().unwrap_or_default();
                    if state.collections.contains_key(&id) {
                        return error_status(StatusCode::CONFLICT, "collection already exists");
                    }
                    state.create_collection_bodies.push(body);
                    state.collections.insert(id, BTreeMap::new());
                    Json(operation(Some(json!({ "@type": "t", "name": "c" })))).into_response()
                },
            ),
        )
        .route(
            &format!("{base}/collections/{{cid}}"),
            delete(|State(state): State<Shared>, Path(cid): Path<String>| async move {
                let mut state = state.lock().await;
                if state.collections.remove(&cid).is_none() {
                    return error_status(StatusCode::NOT_FOUND, "collection not found");
                }
                Json(operation(None)).into_response()
            })
            .merge(get(
                |State(state): State<Shared>, Path(cid): Path<String>| async move {
                    let state = state.lock().await;
                    if state.collections.contains_key(&cid) {
                        Json(json!({ "name": cid })).into_response()
                    } else {
                        error_status(StatusCode::NOT_FOUND, "collection not found")
                    }
                },
            )),
        )
        .route(
            &format!("{base}/collections/{{cid}}/{{action}}"),
            post(
                |State(state): State<Shared>,
                 Path((cid, action)): Path<(String, String)>,
                 Query(params): Query<HashMap<String, String>>,
                 Json(body): Json<Value>| async move {
                    let mut state = state.lock().await;
                    if !state.collections.contains_key(&cid) {
                        return error_status(StatusCode::NOT_FOUND, "collection not found");
                    }
                    match action.as_str() {
                        "dataObjects" => {
                            let id = params.get("dataObjectId").cloned().unwrap_or_default();
                            let objects = state.collections.get_mut(&cid).unwrap();
                            if objects.contains_key(&id) {
                                return error_status(
                                    StatusCode::CONFLICT,
                                    "data object already exists",
                                );
                            }
                            objects.insert(id, body);
                            Json(json!({})).into_response()
                        }
                        "dataObjects:batchCreate" => {
                            let requests = body["requests"].as_array().cloned().unwrap_or_default();
                            let objects = state.collections.get_mut(&cid).unwrap();
                            // Atomic: any existing ID fails the whole batch.
                            if requests.iter().any(|request| {
                                request["dataObjectId"]
                                    .as_str()
                                    .is_some_and(|id| objects.contains_key(id))
                            }) {
                                return error_status(
                                    StatusCode::CONFLICT,
                                    "data object already exists",
                                );
                            }
                            for request in &requests {
                                objects.insert(
                                    request["dataObjectId"]
                                        .as_str()
                                        .unwrap_or_default()
                                        .to_string(),
                                    request["dataObject"].clone(),
                                );
                            }
                            Json(json!({ "dataObjects": [] })).into_response()
                        }
                        "dataObjects:batchDelete" => {
                            let requests = body["requests"].as_array().cloned().unwrap_or_default();
                            let names: Vec<String> = requests
                                .iter()
                                .filter_map(|request| request["name"].as_str())
                                .filter_map(|name| name.rsplit('/').next().map(str::to_string))
                                .collect();
                            let objects = state.collections.get_mut(&cid).unwrap();
                            // Atomic: any missing ID fails the whole batch.
                            if names.iter().any(|id| !objects.contains_key(id)) {
                                return error_status(StatusCode::NOT_FOUND, "data object missing");
                            }
                            for id in &names {
                                objects.remove(id);
                            }
                            Json(json!({})).into_response()
                        }
                        "dataObjects:search" => {
                            state.search_bodies.push(body.clone());
                            let objects = &state.collections[&cid];
                            let search = &body["vectorSearch"];
                            let query: Vec<f64> = search["vector"]["values"]
                                .as_array()
                                .map(|values| values.iter().filter_map(Value::as_f64).collect())
                                .unwrap_or_default();
                            let top_k = search["topK"].as_u64().unwrap_or(10) as usize;
                            let ranked = ranked_vector_results(objects, &query, top_k);
                            Json(results_json(objects, &ranked)).into_response()
                        }
                        "dataObjects:batchSearch" => {
                            let objects = &state.collections[&cid];
                            let searches = body["searches"].as_array().cloned().unwrap_or_default();
                            let rankings: Vec<Vec<(String, f64)>> = searches
                                .iter()
                                .map(|search| {
                                    if let Some(vector_search) = search.get("vectorSearch") {
                                        let query: Vec<f64> = vector_search["vector"]["values"]
                                            .as_array()
                                            .map(|values| {
                                                values.iter().filter_map(Value::as_f64).collect()
                                            })
                                            .unwrap_or_default();
                                        let top_k =
                                            vector_search["topK"].as_u64().unwrap_or(10) as usize;
                                        ranked_vector_results(objects, &query, top_k)
                                    } else {
                                        let text_search = &search["textSearch"];
                                        let needle =
                                            text_search["searchText"].as_str().unwrap_or_default();
                                        let top_k =
                                            text_search["topK"].as_u64().unwrap_or(10) as usize;
                                        ranked_text_results(objects, needle, top_k)
                                    }
                                })
                                .collect();
                            let combine = &body["combine"];
                            let weights: Vec<f64> = combine["ranker"]["rrf"]["weights"]
                                .as_array()
                                .map(|values| values.iter().filter_map(Value::as_f64).collect())
                                .unwrap_or_default();
                            let top_k = combine["topK"].as_u64().unwrap_or(10) as usize;
                            let fused = rrf_fuse(&rankings, &weights, top_k);
                            Json(json!({ "results": [results_json(objects, &fused)] }))
                                .into_response()
                        }
                        _ => error_status(StatusCode::NOT_FOUND, "unknown action"),
                    }
                },
            ),
        )
        .route(
            &format!("{base}/collections/{{cid}}/dataObjects/{{oid}}"),
            axum::routing::patch(
                |State(state): State<Shared>,
                 Path((cid, oid)): Path<(String, String)>,
                 Json(body): Json<Value>| async move {
                    let mut state = state.lock().await;
                    let Some(objects) = state.collections.get_mut(&cid) else {
                        return error_status(StatusCode::NOT_FOUND, "collection not found");
                    };
                    if !objects.contains_key(&oid) {
                        return error_status(StatusCode::NOT_FOUND, "data object missing");
                    }
                    objects.insert(oid, body);
                    Json(json!({})).into_response()
                },
            )
            .merge(delete(
                |State(state): State<Shared>, Path((cid, oid)): Path<(String, String)>| async move {
                    let mut state = state.lock().await;
                    let Some(objects) = state.collections.get_mut(&cid) else {
                        return error_status(StatusCode::NOT_FOUND, "collection not found");
                    };
                    if objects.remove(&oid).is_none() {
                        return error_status(StatusCode::NOT_FOUND, "data object missing");
                    }
                    Json(json!({})).into_response()
                },
            )),
        )
        .with_state(state);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind mock");
    let address = listener.local_addr().expect("mock address");
    tokio::spawn(async move {
        axum::serve(listener, app).await.expect("serve mock");
    });
    format!("http://{address}")
}

fn build_store(endpoint: &str) -> AgentRetrievalStore {
    AgentRetrievalStore::with_credentials(
        AgentRetrievalConfig::new(PROJECT, LOCATION).with_endpoint(endpoint),
        api_key_credentials::Builder::new("test-api-key").build(),
    )
    .expect("build store")
}

#[tokio::test]
async fn test_agent_retrieval_vector_store_contract() {
    let state = Shared::default();
    let endpoint = start_mock(state.clone()).await;
    let store = build_store(&endpoint);
    assert_vector_store_contract(&store, ContractOptions::default()).await;
}

#[tokio::test]
async fn create_collection_declares_a_byoe_dense_vector_schema() {
    let state = Shared::default();
    let endpoint = start_mock(state.clone()).await;
    let store = build_store(&endpoint);

    store.create_collection("golden_wire", 768).await.expect("create");

    let state = state.lock().await;
    // BYOE: no vertexEmbeddingConfig — the embedder pipeline owns vectors.
    assert_eq!(
        state.create_collection_bodies,
        vec![json!({
            "displayName": "golden_wire",
            "vectorSchema": { "embedding": { "denseVector": { "dimensions": 768 } } },
        })],
    );
}

#[tokio::test]
async fn search_sends_the_documented_vector_search_shape() {
    let state = Shared::default();
    let endpoint = start_mock(state.clone()).await;
    let store = build_store(&endpoint);

    store.create_collection("golden_search", 3).await.expect("create");
    store.search("golden_search", &[0.5, 0.25, 0.25], 7).await.expect("search");

    let state = state.lock().await;
    assert_eq!(
        state.search_bodies,
        vec![json!({
            "vectorSearch": {
                "searchField": "embedding",
                "vector": { "values": [0.5f32, 0.25f32, 0.25f32] },
                "topK": 7,
                "outputFields": {
                    "dataFields": ["text", "documentId", "metadata"],
                    "vectorFields": ["embedding"],
                },
            },
        })],
    );
}

#[tokio::test]
async fn existing_collections_only_validates_instead_of_creating() {
    let state = Shared::default();
    let endpoint = start_mock(state.clone()).await;
    let store = AgentRetrievalStore::with_credentials(
        AgentRetrievalConfig::new(PROJECT, LOCATION)
            .with_endpoint(&endpoint)
            .with_existing_collections_only(true),
        api_key_credentials::Builder::new("test-api-key").build(),
    )
    .expect("build store");

    let error = store
        .create_collection("never_provisioned", 4)
        .await
        .expect_err("missing collection must fail in existing-only mode");
    assert!(error.to_string().contains("does not exist"), "{error}");
    assert!(state.lock().await.create_collection_bodies.is_empty(), "must not create");
}

#[tokio::test]
async fn hybrid_search_fuses_semantic_and_text_rankings() {
    let state = Shared::default();
    let endpoint = start_mock(state.clone()).await;
    let store = build_store(&endpoint);

    store.create_collection("hybrid", 2).await.expect("create");
    let chunks = vec![
        adk_rag::Chunk {
            id: "vector-hit".to_string(),
            text: "unrelated words".to_string(),
            embedding: vec![1.0, 0.0],
            metadata: std::collections::HashMap::new(),
            document_id: "d".to_string(),
        },
        adk_rag::Chunk {
            id: "text-hit".to_string(),
            text: "the searched phrase".to_string(),
            embedding: vec![0.0, 1.0],
            metadata: std::collections::HashMap::new(),
            document_id: "d".to_string(),
        },
    ];
    store.upsert("hybrid", &chunks).await.expect("upsert");

    let results = store
        .hybrid_search("hybrid", &[1.0, 0.0], "searched phrase", 2, (0.5, 0.5))
        .await
        .expect("hybrid search");

    let mut ids: Vec<String> = results.iter().map(|result| result.chunk.id.clone()).collect();
    ids.sort();
    assert_eq!(ids, vec!["text-hit".to_string(), "vector-hit".to_string()]);
}

/// **VectorStore contract, search invariants (Agent Retrieval)**
/// *For any* set of uniquely-identified chunks and any non-zero query, `search`
/// SHALL return at most `top_k` distinct stored IDs ordered by descending
/// score, and every inserted chunk SHALL be retrievable.
mod prop_agent_retrieval_search_invariants {
    use super::*;

    const DIM: usize = 8;

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(30))]

        #[test]
        fn search_invariants_hold(
            chunks in arb_unique_chunks(DIM, 20),
            query in arb_normalized_embedding(DIM),
            top_k in 1usize..25,
        ) {
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async {
                let state = Shared::default();
                let endpoint = start_mock(state).await;
                let store = build_store(&endpoint);
                check_search_invariants(&store, "contract", &chunks, &query, top_k).await
            })?;
        }
    }
}
