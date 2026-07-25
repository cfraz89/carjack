use crate::{
    error::Iap2Error,
    iap2::{Iap2, Stream, LINK_SESSION_ID},
    packet::ControlByte,
};

/// Resets the protocol state
pub struct Reset<'a, S>(&'a Iap2<S>);

impl<'a, S: Stream> Reset<'a, S> {
    ///TODO wait for something?
    pub async fn reset(&self) -> Result<(), Iap2Error> {
        self.0
            .send_packet(
                ControlByte {
                    rst: true,
                    ..Default::default()
                },
                LINK_SESSION_ID,
                Vec::new(),
            )
            .await
    }
}
