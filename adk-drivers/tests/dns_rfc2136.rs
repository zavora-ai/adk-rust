//! The RFC 2136 adapter against an authoritative server on loopback that applies signed
//! updates and serves signed zone transfers (spec 3.5 §7.13, §11), and the server's TSIG
//! checked by BIND's own `nsupdate` and `dig` where they are installed.

#![cfg(feature = "rfc2136")]

use std::net::SocketAddr;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use adk_drivers::DriverError;
use adk_drivers::conformance::{DnsHarness, dns_driver_contract};
use adk_drivers::dns::tsig::TsigKey;
use adk_drivers::dns::{ChangeReceipt, DnsDriver, DnsRecord, RecordChange, Rfc2136Dns};
use async_trait::async_trait;
use hickory_proto::op::{Message, MessageType, OpCode, ResponseCode};
use hickory_proto::rr::rdata::{A, NS, SOA};
use hickory_proto::rr::{DNSClass, Name, RData, Record, RecordType};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;

const ZONE: &str = "example.test.";
const KEY_NAME: &str = "update-key";
/// A 32-byte HMAC-SHA256 secret, as `tsig-keygen` writes one.
const SECRET: &str = "BwgJCgsMDQ4PEBESExQVFhcYGRobHB0eHyAhIiMkJSY=";

fn name(text: &str) -> Name {
    Name::from_ascii(text).unwrap()
}

fn now() -> u64 {
    u64::try_from(chrono::Utc::now().timestamp()).unwrap()
}

/// The zone the fake server is authoritative for.
struct Zone {
    serial: u32,
    records: Vec<Record>,
}

impl Zone {
    fn new() -> Self {
        let records = vec![
            Record::from_rdata(name(ZONE), 3600, RData::NS(NS(name("ns1.example.test.")))),
            Record::from_rdata(
                name("ns1.example.test."),
                3600,
                RData::A(A("192.0.2.53".parse().unwrap())),
            ),
            Record::from_rdata(
                name("www.example.test."),
                300,
                RData::A(A("192.0.2.1".parse().unwrap())),
            ),
        ];
        Self { serial: 1, records }
    }

    fn soa(&self) -> Record {
        Record::from_rdata(
            name(ZONE),
            3600,
            RData::SOA(SOA::new(
                name("ns1.example.test."),
                name("hostmaster.example.test."),
                self.serial,
                3600,
                600,
                86_400,
                300,
            )),
        )
    }

    /// Applies an update section, all or nothing (RFC 2136 §3.4).
    fn apply(&mut self, updates: &[Record]) -> ResponseCode {
        let zone = name(ZONE);
        if updates.iter().any(|record| !zone.zone_of(&record.name)) {
            return ResponseCode::NotZone;
        }
        let mut records = self.records.clone();
        for update in updates {
            match update.dns_class {
                DNSClass::IN => {
                    if !records
                        .iter()
                        .any(|known| known.name == update.name && known.data == update.data)
                    {
                        records.push(update.clone());
                    }
                }
                DNSClass::ANY => {
                    let rtype = update.record_type();
                    records.retain(|known| {
                        !(known.name == update.name
                            && (rtype == RecordType::ANY || known.record_type() == rtype))
                    });
                }
                DNSClass::NONE => {
                    records
                        .retain(|known| !(known.name == update.name && known.data == update.data));
                }
                _ => return ResponseCode::FormErr,
            }
        }
        self.records = records;
        self.serial += 1;
        ResponseCode::NoError
    }
}

/// An authoritative server on loopback for [`ZONE`], accepting only messages signed with
/// [`KEY_NAME`].
struct FakeDns {
    addr: SocketAddr,
    zone: Arc<Mutex<Zone>>,
    task: JoinHandle<()>,
}

impl Drop for FakeDns {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl FakeDns {
    async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let zone = Arc::new(Mutex::new(Zone::new()));
        let key = TsigKey::hmac_sha256(KEY_NAME, SECRET).unwrap();
        let served = Arc::clone(&zone);
        let task = tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else { return };
                let (zone, key) = (Arc::clone(&served), key.clone());
                tokio::spawn(async move {
                    let _ = serve(stream, zone, key).await;
                });
            }
        });
        Self { addr, zone, task }
    }

    fn records(&self) -> Vec<Record> {
        self.zone.lock().unwrap_or_else(PoisonError::into_inner).records.clone()
    }
}

async fn serve(mut stream: TcpStream, zone: Arc<Mutex<Zone>>, key: TsigKey) -> std::io::Result<()> {
    loop {
        let length = stream.read_u16().await?;
        let mut wire = vec![0; usize::from(length)];
        stream.read_exact(&mut wire).await?;
        let answer = answer(&wire, &zone, &key);
        stream.write_all(&(answer.len() as u16).to_be_bytes()).await?;
        stream.write_all(&answer).await?;
    }
}

fn answer(wire: &[u8], zone: &Mutex<Zone>, key: &TsigKey) -> Vec<u8> {
    let request = Message::from_vec(wire).unwrap();
    let op_code = request.metadata.op_code;
    let mut response = Message::response(request.metadata.id, op_code);
    response.queries = request.queries.clone();
    let verified = key.verify(wire, None, true, now());
    let Ok(request_mac) = verified else {
        response.metadata.response_code = ResponseCode::NotAuth;
        return response.to_vec().unwrap();
    };
    let ours = request.queries.first().is_some_and(|query| query.name() == &name(ZONE));
    let mut zone = zone.lock().unwrap_or_else(PoisonError::into_inner);
    match (op_code, ours) {
        (_, false) => response.metadata.response_code = ResponseCode::NotAuth,
        (OpCode::Update, true) => {
            response.metadata.response_code = zone.apply(&request.authorities)
        }
        (_, true) => {
            response.metadata.authoritative = true;
            response.answers.push(zone.soa());
            response.answers.extend(zone.records.iter().cloned());
            response.answers.push(zone.soa());
        }
    }
    let unsigned = response.to_vec().unwrap();
    key.sign(&unsigned, Some(&request_mac), now()).unwrap().0
}

/// The adapter and the server it talks to, kept alive together.
struct Served {
    driver: Rfc2136Dns,
    _server: FakeDns,
}

#[async_trait]
impl DnsDriver for Served {
    fn name(&self) -> &str {
        self.driver.name()
    }

    async fn records(&self, zone: &str) -> adk_drivers::Result<Vec<DnsRecord>> {
        self.driver.records(zone).await
    }

    async fn change(
        &self,
        zone: &str,
        changes: &[RecordChange],
    ) -> adk_drivers::Result<ChangeReceipt> {
        self.driver.change(zone, changes).await
    }
}

struct Harness;

#[async_trait]
impl DnsHarness for Harness {
    fn zone(&self) -> String {
        ZONE.into()
    }

    async fn driver(&self) -> Arc<dyn DnsDriver> {
        let server = FakeDns::start().await;
        let driver = Rfc2136Dns::new(server.addr, TsigKey::hmac_sha256(KEY_NAME, SECRET).unwrap());
        Arc::new(Served { driver, _server: server })
    }
}

#[tokio::test]
async fn rfc2136_keeps_the_dns_contract() {
    dns_driver_contract(&Harness).await;
}

#[tokio::test]
async fn a_wrong_key_reads_and_changes_nothing() {
    let server = FakeDns::start().await;
    let wrong = TsigKey::hmac_sha256(KEY_NAME, "c2VjcmV0LXNlY3JldC1zZWNyZXQtc2VjcmV0IQ==").unwrap();
    let dns = Rfc2136Dns::new(server.addr, wrong);
    let error = dns.records(ZONE).await.expect_err("a transfer with the wrong key is refused");
    assert!(matches!(error, DriverError::Unauthorized { .. }), "{error:?}");
    let add =
        RecordChange::Add { record: DnsRecord::new("evil.example.test", "A", 300, "203.0.113.66") };
    assert!(dns.change(ZONE, &[add]).await.is_err());
    assert!(
        !server.records().iter().any(|record| record.name == name("evil.example.test.")),
        "nothing was added"
    );
}

/// Runs a BIND tool with `args` and `stdin`, killed when the test ends.
async fn bind_tool(program: &str, args: &[&str], stdin: &str) -> Option<std::process::Output> {
    let mut command = tokio::process::Command::new(program);
    command
        .args(args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return None,
        Err(error) => panic!("{program} did not start: {error}"),
    };
    let mut input = child.stdin.take().unwrap();
    input.write_all(stdin.as_bytes()).await.unwrap();
    drop(input);
    Some(
        tokio::time::timeout(Duration::from_secs(30), child.wait_with_output())
            .await
            .expect("finishes")
            .unwrap(),
    )
}

/// BIND's `nsupdate` signs an update the server accepts and accepts the server's signed
/// answer; BIND's `dig` accepts the server's signed zone transfer, which holds what the
/// adapter added. The server signs and verifies with the crate's TSIG, so this checks it
/// against an independent implementation. Skipped where BIND's tools are not installed.
#[tokio::test]
async fn bind_tools_accept_the_tsig_the_adapter_uses() {
    let server = FakeDns::start().await;
    let key = format!("hmac-sha256:{KEY_NAME}:{SECRET}");
    let port = server.addr.port().to_string();
    let script = format!(
        "server 127.0.0.1 {port}\nzone {ZONE}\nupdate add bind.example.test. 300 TXT \"from nsupdate\"\nsend\n"
    );
    let Some(output) = bind_tool("nsupdate", &["-v", "-y", &key], &script).await else {
        eprintln!("nsupdate is not installed; skipping the BIND interoperability check");
        return;
    };
    assert!(output.status.success(), "nsupdate: {}", String::from_utf8_lossy(&output.stderr));
    let dns = Rfc2136Dns::new(server.addr, TsigKey::hmac_sha256(KEY_NAME, SECRET).unwrap());
    let records = dns.records(ZONE).await.unwrap();
    assert!(
        records.contains(&DnsRecord::new("bind.example.test.", "TXT", 300, "from nsupdate")),
        "{records:?}"
    );

    let add = RecordChange::Add {
        record: DnsRecord::new("adapter.example.test", "A", 300, "192.0.2.77"),
    };
    dns.change(ZONE, &[add]).await.unwrap();
    let Some(output) =
        bind_tool("dig", &["@127.0.0.1", "-p", &port, "-y", &key, ZONE, "AXFR"], "").await
    else {
        return;
    };
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "dig: {text}");
    assert!(
        !text.contains("tsig verify failure") && !text.contains("Transfer failed"),
        "dig: {text}"
    );
    assert!(text.contains("adapter.example.test.") && text.contains("192.0.2.77"), "dig: {text}");
}

#[test]
fn a_signed_message_verifies_once_and_not_when_altered() {
    let key = TsigKey::hmac_sha256(KEY_NAME, SECRET).unwrap();
    let mut message = Message::new(4242, MessageType::Query, OpCode::Update);
    message.add_query(hickory_proto::op::Query::query(name(ZONE), RecordType::SOA));
    let (signed, mac) = key.sign(&message.to_vec().unwrap(), None, now()).unwrap();
    assert_eq!(key.verify(&signed, None, true, now()).unwrap(), mac);
    let mut altered = signed.clone();
    altered[14] ^= 1;
    assert!(key.verify(&altered, None, true, now()).is_err(), "a changed question fails");
    assert!(key.verify(&signed, None, true, now() + 3_600).is_err(), "a stale time fails");
    let other = TsigKey::hmac_sha256("other-key", SECRET).unwrap();
    assert!(other.verify(&signed, None, true, now()).is_err(), "another key fails");
}
