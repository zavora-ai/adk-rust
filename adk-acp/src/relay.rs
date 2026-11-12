//! A frame relay that lets a local stop end a connection the SDK's protocol router or
//! connector owns.
//!
//! A v1-only connection stops with `Builder::connect_with`, whose foreground returns when the
//! application stops. The SDK's version selectors own the connection they serve instead, so
//! both sides of adk-acp put this relay between the selector and the transport: the client's
//! frames reach the agent through it until `stop` completes, the agent's frames reach the
//! client until the agent closes, and each transport finishes gracefully.
//!
//! Enabled by the `unstable-protocol-v2` feature.

use agent_client_protocol::{Channel, ConnectionDriver, Error};
use futures::StreamExt;

/// Relay frames between a client-side endpoint and an agent-side endpoint until the
/// connection ends, each given as the channel and driver from
/// [`ConnectTo::into_channel_and_future`](agent_client_protocol::ConnectTo::into_channel_and_future).
///
/// Client frames are forwarded until the client's input ends, the agent closes, or `stop`
/// completes; frames already queued are forwarded first. The agent's input is then closed:
/// its driver is asked to finish (an agent process has its stdin closed and is waited for),
/// and it ends once it has answered what it received. The agent's frames are forwarded until
/// it closes, and then the client's transport is asked to flush and finish; a transport that
/// cannot finish on request is dropped once it holds every frame, as the SDK drops its own.
pub(crate) async fn relay_until(
    client: (Channel, Option<ConnectionDriver>),
    agent: (Channel, Option<ConnectionDriver>),
    stop: impl std::future::Future<Output = ()>,
) -> Result<(), Error> {
    let (Channel { rx: mut from_client, tx: to_client, .. }, client_driver) = client;
    let (Channel { rx: mut from_agent, tx: to_agent, .. }, agent_driver) = agent;
    let (agent_closed_tx, mut agent_closed) = tokio::sync::oneshot::channel::<()>();

    let inbound = async move {
        tokio::pin!(stop);
        loop {
            tokio::select! {
                biased;
                frame = from_client.next() => {
                    let Some(frame) = frame else { break };
                    if to_agent.unbounded_send(frame).is_err() {
                        break;
                    }
                }
                () = &mut stop => break,
                _ = &mut agent_closed => break,
            }
        }
        // The agent's end of input.
        drop(to_agent);
        // Held to the end so a client still writing does not see its frames refused.
        from_client
    };
    let outbound = async move {
        while let Some(frame) = from_agent.next().await {
            if to_client.unbounded_send(frame).is_err() {
                break;
            }
        }
        let _ = agent_closed_tx.send(());
    };

    let agent_side = async move {
        let Some(mut driver) = agent_driver else {
            return Ok(inbound.await);
        };
        tokio::pin!(inbound);
        let mut held = None;
        loop {
            tokio::select! {
                from_client = &mut inbound, if held.is_none() => {
                    held = Some(from_client);
                    let _ = driver.request_finish();
                }
                result = &mut driver => {
                    let from_client = match held {
                        Some(from_client) => from_client,
                        None => inbound.await,
                    };
                    return result.map(|()| from_client);
                }
            }
        }
    };
    let client_side = async move {
        let Some(mut driver) = client_driver else {
            outbound.await;
            return Ok(());
        };
        tokio::pin!(outbound);
        let mut forwarded = false;
        loop {
            tokio::select! {
                () = &mut outbound, if !forwarded => {
                    forwarded = true;
                    if !driver.request_finish() {
                        return Ok(());
                    }
                }
                result = &mut driver => {
                    if !forwarded {
                        outbound.await;
                    }
                    return result;
                }
            }
        }
    };

    let (agent_result, client_result) = tokio::join!(agent_side, client_side);
    agent_result.map(drop).and(client_result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_client_protocol::TransportFrame;

    fn endpoints() -> ((Channel, Option<ConnectionDriver>), Channel) {
        let (endpoint, peer) = Channel::duplex();
        ((endpoint, None), peer)
    }

    async fn frame(channel: &mut Channel) -> Option<TransportFrame> {
        channel.rx.next().await
    }

    fn message(id: u64) -> TransportFrame {
        TransportFrame::parse_json(&format!(
            r#"{{"jsonrpc":"2.0","method":"_ping","params":{{"n":{id}}}}}"#
        ))
    }

    #[tokio::test]
    async fn stop_forwards_queued_frames_then_closes_the_agent_input() {
        let (client, mut client_peer) = endpoints();
        let (agent, mut agent_peer) = endpoints();
        let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
        client_peer.tx.unbounded_send(message(1)).expect("queued");
        stop_tx.send(()).expect("stop");
        let relay = tokio::spawn(relay_until(client, agent, async move {
            let _ = stop_rx.await;
        }));

        assert!(frame(&mut agent_peer).await.is_some(), "the queued frame reaches the agent");
        assert!(frame(&mut agent_peer).await.is_none(), "then the agent's input ends");
        // The agent answers what it received, then closes.
        agent_peer.tx.unbounded_send(message(2)).expect("answer");
        drop(agent_peer);
        assert!(frame(&mut client_peer).await.is_some(), "the answer reaches the client");
        assert!(frame(&mut client_peer).await.is_none(), "then the client's input ends");
        relay.await.expect("relay task").expect("relay result");
    }

    #[tokio::test]
    async fn the_client_closing_ends_the_relay() {
        let (client, client_peer) = endpoints();
        let (agent, mut agent_peer) = endpoints();
        let relay = tokio::spawn(relay_until(client, agent, std::future::pending()));
        drop(client_peer);
        assert!(frame(&mut agent_peer).await.is_none(), "the agent sees the client's end");
        drop(agent_peer);
        relay.await.expect("relay task").expect("relay result");
    }
}
