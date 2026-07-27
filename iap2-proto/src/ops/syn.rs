use std::time::Duration;

use backon::{ConstantBuilder, Retryable};

use crate::{
    error::Iap2Error,
    iap2::{Iap2, LINK_SESSION_ID, Stream},
    link::LinkParams,
    packet::ControlByte,
    stream::{SessionTable, SessionType, SynPayload},
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
        // A previous attempt may have left a packet in flight or an ack owed; starting a fresh
        // handshake on top of that stale state would keep the send window occupied forever.
        self.0.link.lock().await.reset();

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

        // Echoing the accessory's proposal back is how we accept it wholesale - so from here on
        // its numbers are binding on us, and the link layer has to actually honour them. They
        // used to be parsed and discarded: we agreed to a one-packet window and then sent three
        // at a time (`cfmoto_11.pcapng`, frames 851-853).
        let params = LinkParams::from(&accessory_sessions);
        tracing::debug!(?params, "Negotiated link parameters");
        self.0.link.lock().await.params = params;

        let our_syn = accessory_sessions.clone();

        let control = ControlByte {
            syn: true,
            ack: true,
            ..Default::default()
        };
        self.0
            .send_packet(control, LINK_SESSION_ID, our_syn.to_bytes())
            .await?;
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

        // The accessory's SYN is the sole authority on which session IDs exist and what
        // type each one is - we must address our messages to *its* numbering. Do not
        // inject entries of our own here: SessionTable::add replaces by id, so a
        // hardcoded {id:2, ExternalAccessory} silently overwrote the CFMOTO head unit's
        // real {id:2, Control} and left us sending identification on a session that did
        // not exist (see docs/investigation-log.md, cfmoto_5.pcapng).
        let mut table = SessionTable::new();
        for s in accessory_sessions.sessions {
            table.add(s);
        }
        if table.find_type(SessionType::Control).is_none() {
            return Err(Iap2Error::ControlSessionError(
                "Accessory SYN declared no control session",
            ));
        }
        tracing::debug!(?table, "SYN exchange complete");
        Ok(table)
    }
}
