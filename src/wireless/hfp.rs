//! A hands-free gateway that answers as a phone. The unit probes device identity over AT
//! (`AT+CGMI`, `AT+CGMM`, `AT+CCLK?`) before it will treat the bond as CarPlay-capable, and the
//! host's own gateway has no modem behind it, so it answers `ERROR` to all three.

use bluer::rfcomm::{self, ProfileHandle, Role};
use chrono::Local;
use libcarjack::sdp::hfp::{AG_FEATURES, hfp_ag_service_record};
use libcarjack::uuid::HANDS_FREE_AG_UUID;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_stream::StreamExt;

use carjack::config::Hfp;

const HFP_CHANNEL: u16 = 8;

const OK: &str = "OK";
const ERROR: &str = "ERROR";

/// The indicator set and its ordering, which the unit reads once and then indexes `+CIND?` by.
const INDICATORS: &str = concat!(
    r#"+CIND: ("service",(0-1)),("call",(0-1)),("callsetup",(0-3)),"#,
    r#"("battchg",(0-5)),("signal",(0-5)),("roam",(0-1)),("callheld",(0-2))"#
);

/// In [`INDICATORS`] order.
const INDICATOR_VALUES: &str = "+CIND: 1,0,0,3,3,0,0";

pub fn profile() -> rfcomm::Profile {
    rfcomm::Profile {
        uuid: HANDS_FREE_AG_UUID,
        name: Some(String::from("Handsfree Gateway")),
        role: Some(Role::Server),
        channel: Some(HFP_CHANNEL),
        require_authentication: Some(true),
        require_authorization: Some(false),
        service_record: Some(hfp_ag_service_record(HFP_CHANNEL as u8).to_xml()),
        ..Default::default()
    }
}

/// The task owns the handle, so the registration lives as long as the process.
pub fn serve(mut handle: ProfileHandle, identity: Hfp) {
    tokio::spawn(async move {
        while let Some(rq) = handle.next().await {
            let from = rq.device();
            match rq.accept() {
                Ok(stream) => {
                    tracing::debug!(%from, "Hands-free gateway connected");
                    let identity = identity.clone();
                    tokio::spawn(async move {
                        let res = converse(stream, &identity).await;
                        tracing::debug!(?res, %from, "Hands-free gateway link ended");
                    });
                }
                Err(e) => tracing::warn!(?e, %from, "Could not accept a hands-free connection"),
            }
        }
    });
}

async fn converse(mut stream: rfcomm::Stream, identity: &Hfp) -> Result<(), std::io::Error> {
    let mut buf = [0u8; 512];
    let mut pending = String::new();
    loop {
        let read = stream.read(&mut buf).await?;
        if read == 0 {
            return Ok(());
        }
        pending.push_str(&String::from_utf8_lossy(&buf[..read]));
        // Commands are `\r`-terminated; whatever trails the last one is a partial read.
        while let Some(end) = pending.find('\r') {
            let command: String = pending.drain(..=end).collect();
            let command = command.trim();
            if command.is_empty() {
                continue;
            }
            let lines = respond(
                command,
                identity,
                &Local::now().format("%y/%m/%d, %H:%M:%S").to_string(),
            );
            tracing::debug!(%command, ?lines, "Hands-free AT");
            for line in lines {
                stream
                    .write_all(format!("\r\n{line}\r\n").as_bytes())
                    .await?;
            }
        }
    }
}

fn respond(command: &str, identity: &Hfp, now: &str) -> Vec<String> {
    let body = command.strip_prefix("AT").unwrap_or(command);
    // `AT+CIND=?` and `AT+CIND?` are different commands; the suffix is what separates them.
    let (name, arg) = match body.find(['=', '?']) {
        Some(at) => body.split_at(at),
        None => (body, ""),
    };
    let ok = |lines: &[String]| -> Vec<String> {
        lines.iter().cloned().chain([String::from(OK)]).collect()
    };
    match (name, arg) {
        ("+BRSF", _) => ok(&[format!("+BRSF:{AG_FEATURES}")]),
        ("+CIND", "=?") => ok(&[String::from(INDICATORS)]),
        ("+CIND", "?") => ok(&[String::from(INDICATOR_VALUES)]),
        ("+CHLD", "=?") => ok(&[String::from("+CHLD: (0,1,1x,2,2x,3)")]),
        ("+BIND", "=?") => ok(&[String::from("+BIND:(1,2)")]),
        ("+BIND", "?") => ok(&[String::from("+BIND:1,1"), String::from("+BIND:2,1")]),
        ("+CGMI", _) => ok(&[format!("+CGMI: \"{}\"", identity.manufacturer)]),
        ("+CGMM", _) => ok(&[format!("+CGMM: {}", identity.model)]),
        ("+CCLK", "?") => ok(&[format!("+CCLK: \"{now}\"")]),
        ("+CNUM", _) => match &identity.number {
            Some(number) => ok(&[format!("+CNUM: ,\"{number}\",145,,4")]),
            None => ok(&[]),
        },
        ("+COPS", "?") => match &identity.operator {
            Some(operator) => ok(&[format!("+COPS: 0,0,\"{operator}\"")]),
            None => ok(&[]),
        },
        (
            "+BIND" | "+CMER" | "+CLIP" | "+CCWA" | "+CMEE" | "+NREC" | "+VGS" | "+VGM" | "+COPS",
            _,
        ) => ok(&[]),
        _ => vec![String::from(ERROR)],
    }
}

#[cfg(test)]
mod tests {
    use super::respond;
    use carjack::config::Hfp;

    /// Every command the CFMOTO sends, in order, with the answers an iPhone gives.
    #[test]
    fn answers_the_units_identity_probe_as_a_phone() {
        let identity = Hfp {
            number: Some(String::from("61420838810")),
            operator: Some(String::from("Belong")),
            ..Hfp::default()
        };
        let now = "26/08/16, 20:37:59";
        for (command, expected) in [
            ("AT+BRSF=383", vec!["+BRSF:3567", "OK"]),
            ("AT+CIND?", vec!["+CIND: 1,0,0,3,3,0,0", "OK"]),
            ("AT+CMER=3,0,0,1", vec!["OK"]),
            ("AT+CHLD=?", vec!["+CHLD: (0,1,1x,2,2x,3)", "OK"]),
            ("AT+BIND=1,2", vec!["OK"]),
            ("AT+BIND=?", vec!["+BIND:(1,2)", "OK"]),
            ("AT+BIND?", vec!["+BIND:1,1", "+BIND:2,1", "OK"]),
            ("AT+CLIP=1", vec!["OK"]),
            ("AT+CCWA=1", vec!["OK"]),
            ("AT+CMEE=1", vec!["OK"]),
            ("AT+CNUM", vec!["+CNUM: ,\"61420838810\",145,,4", "OK"]),
            ("AT+COPS?", vec!["+COPS: 0,0,\"Belong\"", "OK"]),
            ("AT+NREC=0", vec!["OK"]),
            ("AT+CGMI", vec!["+CGMI: \"Apple Inc.\"", "OK"]),
            ("AT+VGS=12", vec!["OK"]),
            ("AT+CGMM", vec!["+CGMM: iPhone16,1", "OK"]),
            ("AT+CCLK?", vec!["+CCLK: \"26/08/16, 20:37:59\"", "OK"]),
            ("AT+VGM=12", vec!["OK"]),
        ] {
            assert_eq!(respond(command, &identity, now), expected, "{command}");
        }
    }

    /// The indicator list has to be handed over before `+CIND?` means anything.
    #[test]
    fn indicator_list_orders_the_indicator_values() {
        let answer = respond("AT+CIND=?", &Hfp::default(), "");
        assert!(answer[0].starts_with("+CIND: (\"service\",(0-1)),(\"call\","));
        assert_eq!(answer[0].matches("),(").count(), 6);
    }

    /// Without a number or operator configured we still have to answer - the host gateway's
    /// `ERROR` is what we are replacing.
    #[test]
    fn unset_subscriber_details_answer_ok_rather_than_error() {
        let identity = Hfp::default();
        assert_eq!(respond("AT+CNUM", &identity, ""), ["OK"]);
        assert_eq!(respond("AT+COPS?", &identity, ""), ["OK"]);
    }

    #[test]
    fn unknown_commands_are_rejected() {
        assert_eq!(respond("AT+CPBS=?", &Hfp::default(), ""), ["ERROR"]);
    }
}
