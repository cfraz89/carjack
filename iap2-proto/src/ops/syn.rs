use std::time::Duration;

use backon::{ConstantBuilder, Retryable};

use crate::{
    error::Iap2Error,
    iap2::{Iap2, Stream},
    packet::ControlByte,
    stream::{SessionEntry, SessionTable, SessionType, SynPayload},
};

pub struct Syn<'a, S>(pub &'a Iap2<S>);

impl<S: Stream> Syn<'_, S> {
    pub async fn perform(&self) -> Result<SessionTable, Iap2Error> {
        (|| async { self.perform_inner().await })
            .retry(ConstantBuilder::new().with_delay(Duration::from_secs(1)).with_max_times(5))
            .await
    }

    async fn perform_inner(&self) -> Result<SessionTable, Iap2Error> {
        tracing::debug!("Waiting for SYN from accessory");

        let packet = tokio::time::timeout(Duration::from_secs(5), self.0.receive_packet())
            .await
            .map_err(|_| Iap2Error::SynFailed("Timeout waiting for SYN"))?
            .ok_or(Iap2Error::SynFailed("No response"))??;

        if !packet.header.control.syn || packet.header.control.ack {
            return Err(Iap2Error::SynFailed("Expected SYN (not SYN+ACK)"));
        }

        let accessory_sessions = SynPayload::from_bytes(&packet.payload)
            .map_err(|_| Iap2Error::SynFailed("Bad SYN payload"))?;
        tracing::debug!(?accessory_sessions, "Received SYN from accessory");

        let mut our_syn = SynPayload::new();
        our_syn.sessions = vec![
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
        ];

        let control = ControlByte {
            syn: true,
            ack: true,
            ..Default::default()
        };
        self.0.send_packet(control, 0, our_syn.to_bytes()).await?;
        tracing::debug!("Sent SYN+ACK");

        let mut table = SessionTable::new();
        for s in accessory_sessions.sessions {
            table.add(s);
        }
        for s in our_syn.sessions {
            table.add(s);
        }
        tracing::debug!(?table, "SYN exchange complete");
        Ok(table)
    }
}
