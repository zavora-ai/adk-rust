use adk_anthropic::{Anthropic, Error, KnownModel, MessageCreateParams};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

async fn endpoint(status: u16, retry_after: &str) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let retry_after = retry_after.to_owned();
    tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut buffer = [0; 8192];
        let mut received = Vec::new();
        loop {
            let count = socket.read(&mut buffer).await.unwrap();
            assert!(count > 0);
            received.extend_from_slice(&buffer[..count]);
            if let Some(end) = received.windows(4).position(|part| part == b"\r\n\r\n") {
                let length = String::from_utf8_lossy(&received[..end])
                    .lines()
                    .find_map(|line| {
                        let (key, value) = line.split_once(':')?;
                        key.eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse::<usize>().unwrap())
                    })
                    .unwrap();
                if received.len() >= end + 4 + length {
                    break;
                }
            }
        }
        let body = r#"{"error":{"type":"api_error","message":"Fixture failure"}}"#;
        let response = format!(
            "HTTP/1.1 {status} Fixture\r\nContent-Length: {}\r\nContent-Type: application/json\r\nRetry-After: {retry_after}\r\nRequest-Id: request-fixture\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        socket.write_all(response.as_bytes()).await.unwrap();
    });
    format!("http://{address}")
}

#[tokio::test]
async fn preserves_specialized_errors_and_statuses() {
    for status in [400, 401, 403, 404, 408, 429, 500, 502, 503, 504, 529, 599] {
        let client = Anthropic::new_with_base_url("fixture-key", endpoint(status, "17").await)
            .unwrap()
            .with_max_retries(0);
        let error = client
            .send(MessageCreateParams::simple("Hello", KnownModel::ClaudeSonnet46))
            .await
            .unwrap_err();
        assert_eq!(error.status_code(), Some(status));
        match status {
            400 => assert!(matches!(error, Error::BadRequest { .. })),
            401 => assert!(error.is_authentication()),
            403 => assert!(error.is_permission()),
            404 => assert!(error.is_not_found()),
            408 => assert!(error.is_timeout()),
            429 | 529 => assert!(error.is_rate_limit()),
            500 => assert!(matches!(error, Error::InternalServer { .. })),
            502..=504 => assert!(matches!(error, Error::ServiceUnavailable { .. })),
            _ => assert!(matches!(error, Error::Api { .. })),
        }
        if matches!(status, 429 | 500..=599) {
            assert_eq!(error.retry_after(), Some(17));
        }
        if matches!(status, 500 | 599) {
            assert_eq!(error.request_id(), Some("request-fixture"));
        }
    }
}

#[tokio::test]
async fn parses_http_date_delays() {
    let date =
        httpdate::fmt_http_date(std::time::SystemTime::now() + std::time::Duration::from_secs(90));
    for header in [date.as_str(), "Wed, 21 Oct 2015 07:28:00 GMT", "invalid"] {
        let client = Anthropic::new_with_base_url("fixture-key", endpoint(503, header).await)
            .unwrap()
            .with_max_retries(0);
        let error = client
            .send(MessageCreateParams::simple("Hello", KnownModel::ClaudeSonnet46))
            .await
            .unwrap_err();
        if header == date {
            assert!((1..=90).contains(&error.retry_after().unwrap()));
        } else if header == "invalid" {
            assert_eq!(error.retry_after(), None);
        } else {
            assert_eq!(error.retry_after(), Some(0));
        }
    }
}

#[tokio::test]
async fn rejects_invalid_delay_numbers() {
    for header in ["+2", "-2", "1.5", "18446744073709552", "18446744073709551616"] {
        let client = Anthropic::new_with_base_url("fixture-key", endpoint(429, header).await)
            .unwrap()
            .with_max_retries(0);
        let error = client
            .send(MessageCreateParams::simple("Hello", KnownModel::ClaudeSonnet46))
            .await
            .unwrap_err();
        assert!(error.is_rate_limit());
        assert!(error.is_retryable());
        assert_eq!(error.status_code(), Some(429));
        assert_eq!(error.retry_after(), None, "{header}");
    }
    for (header, seconds) in [("0", 0), ("61", 61), ("18446744073709551", u64::MAX / 1000)] {
        let client = Anthropic::new_with_base_url("fixture-key", endpoint(529, header).await)
            .unwrap()
            .with_max_retries(0);
        let error = client
            .send(MessageCreateParams::simple("Hello", KnownModel::ClaudeSonnet46))
            .await
            .unwrap_err();
        assert!(error.is_rate_limit());
        assert!(error.is_retryable());
        assert_eq!(error.status_code(), Some(529));
        assert_eq!(error.retry_after(), Some(seconds));
        let delay = std::time::Duration::from_secs(seconds);
        assert!(u64::try_from(delay.as_millis()).is_ok());
    }
}
