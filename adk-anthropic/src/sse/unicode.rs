use super::*;

fn event() -> String {
    let value = serde_json::json!({"type":"content_block_delta", "index":0, "delta":{"type":"text_delta", "text":"完整响应 🙂"}});
    format!("event: content_block_delta\ndata: {value}\n\n")
}

async fn check(chunks: Vec<Bytes>) {
    let source = stream::iter(chunks.into_iter().map(Ok));
    let results: Vec<_> = process_sse(source).collect().await;
    assert_eq!(results.len(), 1);
    let value = serde_json::to_value(results.into_iter().next().unwrap().unwrap()).unwrap();
    assert_eq!(value["delta"]["text"], "完整响应 🙂");
}

#[tokio::test]
async fn preserves_every_split() {
    let input = event().into_bytes();
    for split in 0..=input.len() {
        check(vec![
            Bytes::copy_from_slice(&input[..split]),
            Bytes::copy_from_slice(&input[split..]),
        ])
        .await;
    }
}

#[tokio::test]
async fn preserves_bytewise_delivery() {
    check(event().bytes().map(|byte| Bytes::from(vec![byte])).collect()).await;
}

#[tokio::test]
async fn rejects_invalid_bytes_once() {
    let mut input = b"event: content_block_delta\ndata: ".to_vec();
    input.extend_from_slice(&[0xff, 0xfe]);
    let results: Vec<_> = process_sse(stream::iter(vec![Ok(Bytes::from(input))])).collect().await;
    assert_eq!(results.len(), 1);
    assert!(results[0].as_ref().unwrap_err().to_string().contains("UTF-8"));
}

#[tokio::test]
async fn rejects_incomplete_final_character_once() {
    let results: Vec<_> =
        process_sse(stream::iter(vec![Ok(Bytes::from_static(b"event: ping\ndata: \xf0\x9f"))]))
            .collect()
            .await;
    assert_eq!(results.len(), 1);
    assert!(results[0].as_ref().unwrap_err().to_string().contains("UTF-8"));
}
