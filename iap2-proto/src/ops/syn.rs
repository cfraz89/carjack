use std::time::Duration;

use backon::{ConstantBuilder, Retryable};

use crate::{
    error::Iap2Error,
    iap2::{Iap2, Stream},
    packet::ControlByte,
    stream::{SessionEntry, SessionTable, SessionType, SynPayload},
};
use tap::prelude::*;

pub struct Syn<'a, S>(pub &'a Iap2<S>);

impl<S: Stream> Syn<'_, S> {
    pub async fn perform(&self) -> Result<SessionTable, Iap2Error> {
        (|| async {
            self.perform_inner()
                .await
                .tap_err(|e| tracing::error!(?e, "Error performing syn"))
        })
        .retry(
            ConstantBuilder::new()
                .with_delay(Duration::from_secs(1))
                .with_max_times(5),
        )
        .await
    }

    async fn perform_inner(&self) -> Result<SessionTable, Iap2Error> {
        tracing::debug!("Waiting for SYN from accessory");

        let packet = tokio::time::timeout(Duration::from_secs(5), self.0.receive_packet())
            .await
            .map_err(|_| Iap2Error::Timeout(String::from("Timeout waiting for SYN")))??;

        if !packet.header.control.syn || packet.header.control.ack {
            return Err(Iap2Error::UnexpectedPacket(
                String::from("Expected SYN (not SYN+ACK)"),
                packet,
            ));
        }

        let accessory_sessions = SynPayload::from_bytes(&packet.payload)
            .map_err(|_| Iap2Error::Other(String::from("Bad SYN payload")))?;
        tracing::debug!(?accessory_sessions, "Received SYN from accessory");

        let our_syn = accessory_sessions.clone();

        let control = ControlByte {
            syn: true,
            ack: true,
            ..Default::default()
        };
        self.0.send_packet(control, 0, our_syn.to_bytes()).await?;
        tracing::debug!("Sent SYN+ACK, waiting for ACK");

        let ack_packet = tokio::time::timeout(Duration::from_secs(5), self.0.receive_packet())
            .await
            .map_err(|_| {
                Iap2Error::Timeout(String::from("Timeout waiting for ACK after SYN+ACK"))
            })??;

        if ack_packet.header.control.syn || !ack_packet.header.control.ack {
            return Err(Iap2Error::UnexpectedPacket(
                String::from("Expected ACK after SYN+ACK"),
                ack_packet,
            ));
        }
        tracing::debug!("Received ACK, handshake complete");

        let mut table = SessionTable::new();
        for s in accessory_sessions.sessions {
            table.add(s);
        }
        for s in [
            SessionEntry {
                id: 0,
                session_type: SessionType::Control,
                version: 1,
            },
            SessionEntry {
                id: 2,
                session_type: SessionType::ExternalAccessory,
                version: 1,
            },
        ] {
            table.add(s);
        }
        tracing::debug!(?table, "SYN exchange complete");
        Ok(table)
    }
}
