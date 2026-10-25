//! DNS over RFC 2136 dynamic update with TSIG, read by AXFR (spec 3.5 §7.13; feature
//! `rfc2136`).

use std::net::SocketAddr;
use std::time::Duration;

use async_trait::async_trait;
use hickory_proto::op::{Message, MessageType, OpCode, Query, ResponseCode};
use hickory_proto::rr::rdata::{A, AAAA, CNAME, MX, NS, SRV, TXT};
use hickory_proto::rr::{DNSClass, Name, RData, Record, RecordType};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use super::tsig::{TsigKey, now};
use super::{ChangeReceipt, DnsDriver, DnsRecord, RecordChange, fqdn, validate_changes};
use crate::error::{DriverError, Result};

const SERVICE: &str = "dns";
/// The most messages a zone transfer may take.
const MAX_TRANSFER_MESSAGES: usize = 10_000;

/// A DNS host reached by RFC 2136 dynamic update, authenticated with TSIG (spec 3.5 §7.13).
///
/// Works with any authoritative server that accepts signed updates and zone transfers —
/// BIND, Knot, PowerDNS, NSD behind a primary — over TCP. Every request is signed with the
/// key, and every response's signature is verified before it is believed; a change set is
/// one UPDATE message, so the server applies it whole or not at all (RFC 2136 §3.7).
///
/// # Example
///
/// ```rust,no_run
/// use adk_drivers::dns::{DnsDriver, Rfc2136Dns};
/// use adk_drivers::dns::tsig::TsigKey;
///
/// # async fn example(secret: &str) -> adk_drivers::Result<()> {
/// let key = TsigKey::hmac_sha256("site-update", secret)?;
/// let dns = Rfc2136Dns::new("192.0.2.53:53".parse().unwrap(), key);
/// let records = dns.records("example.com").await?;
/// # let _ = records;
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone)]
pub struct Rfc2136Dns {
    server: SocketAddr,
    key: TsigKey,
    timeout: Duration,
}

impl Rfc2136Dns {
    /// A driver for the primary at `server`, signing with `key`.
    pub fn new(server: SocketAddr, key: TsigKey) -> Self {
        Self { server, key, timeout: Duration::from_secs(10) }
    }

    /// How long one exchange may take; 10 s by default.
    #[must_use]
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    async fn connect(&self) -> Result<TcpStream> {
        tokio::time::timeout(self.timeout, TcpStream::connect(self.server))
            .await
            .map_err(|_| {
                DriverError::unavailable(SERVICE, format!("{} did not answer in time", self.server))
            })?
            .map_err(|error| DriverError::unavailable(SERVICE, format!("{}: {error}", self.server)))
    }

    async fn write(&self, stream: &mut TcpStream, wire: &[u8]) -> Result<()> {
        let length = u16::try_from(wire.len())
            .map_err(|_| DriverError::invalid("the DNS message is too large"))?;
        let mut framed = Vec::with_capacity(wire.len() + 2);
        framed.extend_from_slice(&length.to_be_bytes());
        framed.extend_from_slice(wire);
        tokio::time::timeout(self.timeout, stream.write_all(&framed))
            .await
            .map_err(|_| DriverError::unavailable(SERVICE, "the write timed out"))?
            .map_err(|error| DriverError::unavailable(SERVICE, error))
    }

    async fn read(&self, stream: &mut TcpStream) -> Result<Vec<u8>> {
        let read = async {
            let length = stream.read_u16().await?;
            let mut wire = vec![0; usize::from(length)];
            stream.read_exact(&mut wire).await?;
            Ok::<_, std::io::Error>(wire)
        };
        tokio::time::timeout(self.timeout, read)
            .await
            .map_err(|_| DriverError::unavailable(SERVICE, "the server did not answer in time"))?
            .map_err(|error| DriverError::unavailable(SERVICE, error))
    }

    /// Signs `message`, returning its wire form and MAC.
    fn sign(&self, message: &Message) -> Result<(Vec<u8>, Vec<u8>)> {
        let wire = message.to_vec().map_err(|error| DriverError::invalid(error.to_string()))?;
        self.key.sign(&wire, None, now())
    }
}

fn dns_name(name: &str) -> Result<Name> {
    let mut parsed = Name::from_ascii(name)
        .map_err(|error| DriverError::invalid(format!("`{name}` is not a domain name: {error}")))?;
    parsed.set_fqdn(true);
    Ok(parsed)
}

fn random_id() -> u16 {
    let bytes = uuid::Uuid::new_v4().into_bytes();
    u16::from_be_bytes([bytes[0], bytes[1]])
}

/// The error a response code means.
fn check_rcode(message: &Message, zone: &str) -> Result<()> {
    match message.metadata.response_code {
        ResponseCode::NoError => Ok(()),
        ResponseCode::NotAuth => Err(DriverError::Unauthorized {
            service: SERVICE.into(),
            reason: format!("the server is not authoritative for `{zone}`, or refused the key"),
        }),
        ResponseCode::Refused => Err(DriverError::refused(format!(
            "the server refused the request for `{zone}`; check that its policy allows the key"
        ))),
        ResponseCode::NotZone | ResponseCode::NXDomain => {
            Err(DriverError::not_found(format!("zone `{zone}` on the server")))
        }
        code => Err(DriverError::unavailable(SERVICE, format!("the server answered {code}"))),
    }
}

/// The RDATA of a canonical record of a changeable type.
fn rdata_of(record: &DnsRecord) -> Result<RData> {
    let bad = || {
        DriverError::invalid(format!("`{}` is not a valid {} value", record.value, record.rtype))
    };
    let mut words = record.value.split_whitespace();
    let mut number = || words.next().and_then(|word| word.parse::<u16>().ok()).ok_or_else(bad);
    Ok(match record.rtype.as_str() {
        "A" => RData::A(A(record.value.parse().map_err(|_| bad())?)),
        "AAAA" => RData::AAAA(AAAA(record.value.parse().map_err(|_| bad())?)),
        "CNAME" => RData::CNAME(CNAME(dns_name(&record.value)?)),
        "NS" => RData::NS(NS(dns_name(&record.value)?)),
        "MX" => {
            let preference = number()?;
            let exchange = record.value.split_whitespace().nth(1).ok_or_else(bad)?;
            RData::MX(MX::new(preference, dns_name(exchange)?))
        }
        "SRV" => {
            let (priority, weight, port) = (number()?, number()?, number()?);
            let target = record.value.split_whitespace().nth(3).ok_or_else(bad)?;
            RData::SRV(SRV::new(priority, weight, port, dns_name(target)?))
        }
        "TXT" => RData::TXT(TXT::new(txt_strings(&record.value))),
        other => {
            return Err(DriverError::invalid(format!(
                "{other} records are not changed by a driver"
            )));
        }
    })
}

/// `text` as character-strings of at most 255 bytes, split on character boundaries.
fn txt_strings(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    for c in text.chars() {
        if current.len() + c.len_utf8() > 255 {
            out.push(std::mem::take(&mut current));
        }
        current.push(c);
    }
    if !current.is_empty() || out.is_empty() {
        out.push(current);
    }
    out
}

/// A record of a zone transfer in canonical form.
fn record_of(record: &Record) -> DnsRecord {
    let value = match &record.data {
        RData::TXT(txt) => {
            txt.txt_data.iter().map(|part| String::from_utf8_lossy(part)).collect::<String>()
        }
        data => data.to_string(),
    };
    let raw = DnsRecord::new(
        record.name.to_ascii().to_ascii_lowercase(),
        record.record_type().to_string(),
        record.ttl,
        value,
    );
    raw.normalized().unwrap_or(raw)
}

#[async_trait]
impl DnsDriver for Rfc2136Dns {
    fn name(&self) -> &str {
        "rfc2136"
    }

    async fn records(&self, zone: &str) -> Result<Vec<DnsRecord>> {
        let zone = fqdn(zone)?;
        let mut request = Message::new(random_id(), MessageType::Query, OpCode::Query);
        request.add_query(Query::query(dns_name(&zone)?, RecordType::AXFR));
        let (wire, request_mac) = self.sign(&request)?;
        let mut stream = self.connect().await?;
        self.write(&mut stream, &wire).await?;
        let mut records = Vec::new();
        let mut soas = 0;
        let mut prior = request_mac;
        for index in 0..MAX_TRANSFER_MESSAGES {
            let wire = self.read(&mut stream).await?;
            prior = self.key.verify(&wire, Some(&prior), index == 0, now())?;
            let message = Message::from_vec(&wire)
                .map_err(|error| DriverError::protocol(SERVICE, error.to_string()))?;
            check_rcode(&message, &zone)?;
            for record in &message.answers {
                if record.record_type() == RecordType::SOA {
                    soas += 1;
                    if soas == 2 {
                        records.sort();
                        records.dedup();
                        return Ok(records);
                    }
                }
                records.push(record_of(record));
            }
        }
        Err(DriverError::protocol(SERVICE, "the zone transfer did not end"))
    }

    async fn change(&self, zone: &str, changes: &[RecordChange]) -> Result<ChangeReceipt> {
        let changes = validate_changes(zone, changes)?;
        let zone = fqdn(zone)?;
        let mut request = Message::new(random_id(), MessageType::Query, OpCode::Update);
        let mut zone_query = Query::query(dns_name(&zone)?, RecordType::SOA);
        zone_query.set_query_class(DNSClass::IN);
        request.add_query(zone_query);
        for change in &changes {
            match change {
                RecordChange::Add { record } => {
                    request.authorities.push(Record::from_rdata(
                        dns_name(&record.name)?,
                        record.ttl,
                        rdata_of(record)?,
                    ));
                }
                RecordChange::Remove { record } => {
                    let mut delete =
                        Record::from_rdata(dns_name(&record.name)?, 0, rdata_of(record)?);
                    delete.dns_class = DNSClass::NONE;
                    request.authorities.push(delete);
                }
                RecordChange::Replace { name, rtype, ttl, values } => {
                    let owner = dns_name(name)?;
                    let record_type = rtype.parse::<RecordType>().map_err(|error| {
                        DriverError::invalid(format!("`{rtype}` is not a record type: {error}"))
                    })?;
                    let mut delete = Record::update0(owner.clone(), 0, record_type);
                    delete.dns_class = DNSClass::ANY;
                    request.authorities.push(delete);
                    for value in values {
                        let record =
                            DnsRecord::new(name.as_str(), rtype.as_str(), *ttl, value.as_str());
                        request.authorities.push(Record::from_rdata(
                            owner.clone(),
                            *ttl,
                            rdata_of(&record)?,
                        ));
                    }
                }
            }
        }
        let (wire, request_mac) = self.sign(&request)?;
        let mut stream = self.connect().await?;
        self.write(&mut stream, &wire).await?;
        let answer = self.read(&mut stream).await?;
        self.key.verify(&answer, Some(&request_mac), true, now())?;
        let message = Message::from_vec(&answer)
            .map_err(|error| DriverError::protocol(SERVICE, error.to_string()))?;
        if message.metadata.id != request.metadata.id {
            return Err(DriverError::protocol(SERVICE, "the answer is for another request"));
        }
        check_rcode(&message, &zone)?;
        Ok(ChangeReceipt { zone, applied: changes.len(), serial: None })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_text_splits_into_character_strings_of_at_most_255_bytes() {
        let text = "é".repeat(200);
        let parts = txt_strings(&text);
        assert!(parts.iter().all(|part| part.len() <= 255));
        assert_eq!(parts.concat(), text);
        assert_eq!(txt_strings(""), [""]);
    }

    #[test]
    fn records_become_rdata_of_their_type() {
        let mx = DnsRecord::new("example.com.", "MX", 300, "10 mail.example.com.");
        assert!(matches!(rdata_of(&mx).unwrap(), RData::MX(_)));
        let srv =
            DnsRecord::new("_sip._tcp.example.com.", "SRV", 300, "10 5 5060 sip.example.com.");
        assert!(matches!(rdata_of(&srv).unwrap(), RData::SRV(_)));
        let bad = DnsRecord::new("example.com.", "A", 300, "not-an-address");
        assert!(rdata_of(&bad).is_err());
    }
}
