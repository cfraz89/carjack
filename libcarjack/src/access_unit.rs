//! Splitting an encoder's Annex-B access unit into what the screen stream sends.

use catplay_carplay::video::NalChunk;

use crate::airplay::VideoCodec;

/// The NAL length prefix the wire's sample entry declares, and the width of the start codes the
/// parsers emit in its place - catplay swaps one for the other in place.
pub const NAL_SIZE_LEN: usize = 4;

/// Is this NAL a coded slice? A receiver reads the first NAL of a sample to decide whether the
/// stream can open there, so nothing else belongs in one.
fn is_vcl(nal_header: u8, codec: VideoCodec) -> bool {
    match codec {
        VideoCodec::Hevc => (nal_header >> 1) & 0x3f < 32,
        VideoCodec::H264 => matches!(nal_header & 0x1f, 1..=5),
    }
}

/// VPS/SPS/PPS - what the config frame configures the accessory's decoder from.
fn is_parameter_set(nal_header: u8, codec: VideoCodec) -> bool {
    match codec {
        VideoCodec::Hevc => matches!((nal_header >> 1) & 0x3f, 32..=34),
        VideoCodec::H264 => matches!(nal_header & 0x1f, 7 | 8),
    }
}

/// One access unit, split the way the screen stream wants it.
pub struct AccessUnit {
    /// The coded slices, Annex-B.
    pub data: Vec<u8>,
    /// Where each slice's start code sits in `data` - catplay's NAL cache, which saves it
    /// rescanning the sample when it swaps those four bytes for the wire's length prefix.
    pub nals: Vec<NalChunk>,
    /// The parameter sets that preceded them, Annex-B in decode order. Empty on a delta frame.
    pub parameter_sets: Vec<u8>,
}

/// Split a parsed access unit into the sample the accessory decodes and the parameter sets its
/// decoder is configured from, copying each NAL with the start code the parser gave it. Access
/// unit delimiters and SEI are dropped.
pub fn split_access_unit(data: &[u8], codec: VideoCodec) -> AccessUnit {
    let mut au = AccessUnit {
        data: Vec::with_capacity(data.len()),
        nals: Vec::new(),
        parameter_sets: Vec::new(),
    };

    let mut cursor = next_start_code(data, 0);
    while let Some((start, prefix_len)) = cursor {
        let payload = start + prefix_len;
        cursor = next_start_code(data, payload);
        let end = cursor.map_or(data.len(), |(at, _)| at);
        match data.get(payload) {
            Some(&header) if is_vcl(header, codec) => {
                au.nals.push(NalChunk {
                    prefix_start: au.data.len(),
                    prefix_len,
                    data_size: end - payload,
                });
                au.data.extend_from_slice(&data[start..end]);
            }
            Some(&header) if is_parameter_set(header, codec) => {
                au.parameter_sets.extend_from_slice(&data[start..end]);
            }
            _ => {}
        }
    }

    if au.data.is_empty() {
        tracing::warn!(
            len = data.len(),
            "No coded slice in the parser's access unit; the accessory has nothing to decode"
        );
    }
    au
}

/// Where the next Annex-B start code begins, and how long it is.
fn next_start_code(data: &[u8], from: usize) -> Option<(usize, usize)> {
    (from..data.len().saturating_sub(2)).find_map(|i| match data[i..] {
        [0, 0, 1, ..] => Some((i, 3)),
        [0, 0, 0, 1, ..] => Some((i, 4)),
        _ => None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The head of a real `vah265enc` access unit: an AUD, then VPS/SPS/PPS, then the IDR slice.
    /// Truncated - only the parameter sets matter here.
    const HEVC_KEYFRAME_HEAD: &[&[u8]] = &[
        &[0x46, 0x01, 0x10],                               // 35, AUD
        &[0x40, 0x01, 0x0c, 0x01, 0xff, 0xff, 0x01, 0x60], // 32, VPS
        &[0x42, 0x01, 0x01, 0x01, 0x60, 0x00, 0x00, 0x03], // 33, SPS
        &[0x44, 0x01, 0xc0, 0x76, 0x4c, 0x08, 0x90],       // 34, PPS
        &[0x26, 0x01, 0xaf, 0xe0, 0xfa, 0x20, 0xab, 0x43], // 19, IDR slice
    ];

    const START_CODE: [u8; 4] = [0, 0, 0, 1];

    fn annexb(nals: &[&[u8]]) -> Vec<u8> {
        let mut out = Vec::new();
        for nal in nals {
            out.extend_from_slice(&START_CODE);
            out.extend_from_slice(nal);
        }
        out
    }

    /// The sample must open on its slice: a receiver reads the first NAL to decide whether the
    /// frame opens the stream, and the parser puts an AUD and the parameter sets before every one.
    #[test]
    fn the_sample_keeps_only_the_slices() {
        let au = split_access_unit(&annexb(HEVC_KEYFRAME_HEAD), VideoCodec::Hevc);
        assert_eq!(au.data, annexb(&HEVC_KEYFRAME_HEAD[4..]));
        assert_eq!(au.parameter_sets, annexb(&HEVC_KEYFRAME_HEAD[1..4]));

        let delta: &[&[u8]] = &[&[0x46, 0x01, 0x10], &[0x02, 0x01, 0xd0, 0x09]]; // AUD, then TrailR
        let au = split_access_unit(&annexb(delta), VideoCodec::Hevc);
        assert_eq!(au.data, annexb(&delta[1..]));
        assert!(au.parameter_sets.is_empty(), "a delta configures nothing");
    }

    /// The offsets are catplay's NAL cache: it verifies each start code and swaps those four bytes
    /// for a length prefix, so a wrong one either fails the sample or corrupts it.
    #[test]
    fn the_offsets_point_at_the_start_codes() {
        let slices: &[&[u8]] = &[&[0x26, 0x01, 0xaf, 0xe0], &[0x02, 0x01, 0xd0]];
        let au = split_access_unit(&annexb(slices), VideoCodec::Hevc);

        assert_eq!(au.nals.len(), 2);
        for (nal, slice) in au.nals.iter().zip(slices) {
            assert_eq!(nal.prefix_len, NAL_SIZE_LEN);
            assert_eq!(nal.data_size, slice.len());
            let start = nal.prefix_start;
            assert_eq!(&au.data[start..start + 4], &START_CODE);
            assert_eq!(&au.data[start + 4..start + 4 + nal.data_size], *slice);
        }
    }

    /// Three-byte start codes are legal Annex-B and the parsers do not emit them, so they are
    /// copied through as they came. What must hold either way is that the offsets describe the
    /// buffer we actually wrote - catplay reads the sample through them.
    #[test]
    fn the_offsets_describe_a_three_byte_start_code_too() {
        let slice: &[u8] = &[0x26, 0x01, 0xaf, 0xe0];
        let mut stream = vec![0, 0, 1];
        stream.extend_from_slice(HEVC_KEYFRAME_HEAD[1]); // VPS
        stream.extend_from_slice(&[0, 0, 1]);
        stream.extend_from_slice(slice);

        let au = split_access_unit(&stream, VideoCodec::Hevc);
        let nal = au.nals[0];
        assert_eq!(nal.prefix_len, 3);
        assert_eq!(
            &au.data[nal.prefix_start + nal.prefix_len..][..nal.data_size],
            slice
        );
    }

    /// HEVC and H264 keep their NAL type in different bits, and reading one as the other drops
    /// every slice - an empty sample, and a stream the accessory has nothing to decode.
    #[test]
    fn slices_are_recognised_per_codec() {
        let hevc = annexb(&HEVC_KEYFRAME_HEAD[4..]);
        assert!(split_access_unit(&hevc, VideoCodec::H264).data.is_empty());

        let h264: &[&[u8]] = &[
            &[0x09, 0x10],
            &[0x67, 0x42, 0xe0],
            &[0x68, 0xce],
            &[0x65, 0x88, 0x84],
        ];
        let au = split_access_unit(&annexb(h264), VideoCodec::H264);
        assert_eq!(au.data, annexb(&h264[3..]));
        assert_eq!(au.parameter_sets, annexb(&h264[1..3]), "SPS then PPS");
    }

    /// The VPS/SPS/PPS this pipeline's own parser (`vah265enc ! h265parse`) used to hand over
    /// inside an `hvcC`. Taking them from the byte-stream instead has to yield the same bytes -
    /// the config frame is written from these, and it is matched to an iPhone's byte for byte.
    const HVCC_PARAMETER_SETS: &str = "0000000140010c01ffff0160000003009000000300000300781302\
        4000000001420101016000000300900000030000030078a0064200501684e491b6affc000400040400000300\
        04000003007820000000014401c0764c0890";

    fn hex(s: &str) -> Vec<u8> {
        s.as_bytes()
            .chunks(2)
            .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
            .collect()
    }

    #[test]
    fn the_byte_stream_yields_the_parameter_sets_the_config_frame_needs() {
        let reference = hex(HVCC_PARAMETER_SETS);
        let mut stream = reference.clone();
        stream.extend_from_slice(&annexb(&[HEVC_KEYFRAME_HEAD[4]]));

        let au = split_access_unit(&stream, VideoCodec::Hevc);
        assert_eq!(
            au.parameter_sets, reference,
            "expected VPS, SPS then PPS, Annex-B, in decode order"
        );
        assert_eq!(au.data, annexb(&HEVC_KEYFRAME_HEAD[4..]));
    }
}
