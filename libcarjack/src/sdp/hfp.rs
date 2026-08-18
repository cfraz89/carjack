use btuuid::browse_group_identifiers::PUBLIC_BROWSE_ROOT;
use btuuid::protocol_identifiers::{L2_CAP, RFCOMM};
use btuuid::service_class::{AG_HANDS_FREE, GENERIC_AUDIO, HANDS_FREE};

use crate::sdp::types::{attribute, SdpAttributeId::*, SdpRecord, HANDS_FREE_VERSION_1_7};

/// An iPhone's, minus codec negotiation - we have no codec to negotiate.
pub const AG_FEATURES: u16 = 3567;

/// The subset SDP carries, narrower than [`AG_FEATURES`].
const SDP_FEATURES: u16 = 0x2f;

const REJECTS_CALLS: u8 = 0x01;

/// The hands-free gateway record an iPhone publishes, field for field.
pub fn hfp_ag_service_record(channel: u8) -> SdpRecord {
    SdpRecord {
        attribute: vec![
            attribute(ServiceClassIdList, (AG_HANDS_FREE, GENERIC_AUDIO)),
            attribute(ProtocolDescriptorList, ((L2_CAP,), (RFCOMM, channel))),
            attribute(BrowseGroupList, (PUBLIC_BROWSE_ROOT,)),
            attribute(
                BluetoothProfileDescriptorList,
                ((HANDS_FREE, HANDS_FREE_VERSION_1_7),),
            ),
            attribute(ServiceName, "Handsfree Gateway"),
            attribute(Network, REJECTS_CALLS),
            attribute(SupportedFeatures, SDP_FEATURES),
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::hfp_ag_service_record;

    /// The unit reads the profile descriptor and the two feature attributes off this record before
    /// it opens the AT channel.
    #[test]
    fn service_record_declares_hands_free_1_7() {
        let record = hfp_ag_service_record(8).to_xml();
        for id in [
            "0x0001", "0x0004", "0x0005", "0x0009", "0x0100", "0x0301", "0x0311",
        ] {
            assert!(
                record.contains(&format!("<attribute id=\"{id}\">")),
                "missing attribute {id}"
            );
        }
        for fragment in [
            "<uuid value=\"0x111f\"/>",
            "<uuid value=\"0x1203\"/>",
            "<uuid value=\"0x111e\"/>",
            "<uint16 value=\"0x0107\"/>",
            "<uint8 value=\"0x08\"/>",
            "<uint16 value=\"0x002f\"/>",
            "<text value=\"Handsfree Gateway\"/>",
        ] {
            assert!(record.contains(fragment), "missing {fragment}");
        }
    }
}
