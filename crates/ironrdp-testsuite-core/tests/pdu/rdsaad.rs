//! RDS AAD Auth PDUs ([MS-RDPBCGR] 2.2.18).

use ironrdp_core::{ReadCursor, decode, encode_vec};
use ironrdp_pdu::PduHint as _;
use ironrdp_pdu::rdp::rdsaad::{
    AuthenticationRequestPdu, AuthenticationResult, AuthenticationResultPdu, MAX_PDU_SIZE, RDSAAD_HINT, ServerNoncePdu,
};

const SERVER_NONCE_WIRE: &[u8] = b"{\"ts_nonce\":\"AwABAAEAAAACAOz_BQD0_w\"}\0";

const AUTH_REQUEST_WIRE: &[u8] = b"{\"rdp_assertion\":\"eyJhbGciOiJSUzI1NiJ9.eyJ0cyI6IjEifQ.c2ln\"}\0";

const AUTH_RESULT_WIRE: &[u8] = b"{\"authentication_result\":2147942405}\0";

#[test]
fn server_nonce_decode() {
    let pdu = decode::<ServerNoncePdu>(SERVER_NONCE_WIRE).unwrap();
    assert_eq!(pdu.ts_nonce, "AwABAAEAAAACAOz_BQD0_w");
}

#[test]
fn server_nonce_encode() {
    let pdu = ServerNoncePdu {
        ts_nonce: "AwABAAEAAAACAOz_BQD0_w".to_owned(),
    };
    assert_eq!(encode_vec(&pdu).unwrap(), SERVER_NONCE_WIRE);
}

#[test]
fn authentication_request_decode() {
    let pdu = decode::<AuthenticationRequestPdu>(AUTH_REQUEST_WIRE).unwrap();
    assert_eq!(pdu.rdp_assertion, "eyJhbGciOiJSUzI1NiJ9.eyJ0cyI6IjEifQ.c2ln");
}

#[test]
fn authentication_request_encode() {
    let pdu = AuthenticationRequestPdu {
        rdp_assertion: "eyJhbGciOiJSUzI1NiJ9.eyJ0cyI6IjEifQ.c2ln".to_owned(),
    };
    assert_eq!(encode_vec(&pdu).unwrap(), AUTH_REQUEST_WIRE);
}

#[test]
fn authentication_request_debug_redacts_assertion() {
    let pdu = AuthenticationRequestPdu {
        rdp_assertion: "secret".to_owned(),
    };
    assert!(!format!("{pdu:?}").contains("secret"));
}

#[test]
fn authentication_result_decode() {
    let pdu = decode::<AuthenticationResultPdu>(AUTH_RESULT_WIRE).unwrap();
    assert_eq!(pdu.authentication_result, AuthenticationResult::E_ACCESSDENIED);
    assert!(!pdu.authentication_result.is_success());
}

#[test]
fn authentication_result_encode() {
    let pdu = AuthenticationResultPdu {
        authentication_result: AuthenticationResult::E_ACCESSDENIED,
    };
    assert_eq!(encode_vec(&pdu).unwrap(), AUTH_RESULT_WIRE);
}

#[test]
fn authentication_result_accepts_number_and_string_forms() {
    for (wire, expected) in [
        (&b"{\"authentication_result\":0}\0"[..], AuthenticationResult::S_OK),
        (b"{\"authentication_result\":\"0\"}\0", AuthenticationResult::S_OK),
        (
            b"{\"authentication_result\":-2146893048}\0",
            AuthenticationResult::SEC_E_INVALID_TOKEN,
        ),
        (
            b"{\"authentication_result\":\"2148074248\"}\0",
            AuthenticationResult::SEC_E_INVALID_TOKEN,
        ),
        (
            b"{\"authentication_result\":\"0x80090308\"}\0",
            AuthenticationResult::SEC_E_INVALID_TOKEN,
        ),
        (
            b"{\"authentication_result\":\"0XD000006D\"}\0",
            AuthenticationResult::STATUS_LOGON_FAILURE,
        ),
    ] {
        let pdu = decode::<AuthenticationResultPdu>(wire)
            .unwrap_or_else(|e| panic!("{} must decode: {e}", String::from_utf8_lossy(wire)));
        assert_eq!(pdu.authentication_result, expected, "{}", String::from_utf8_lossy(wire));
    }
}

#[test]
fn authentication_result_rejects_non_hresult_values() {
    for wire in [
        &b"{\"authentication_result\":1.5}\0"[..],
        b"{\"authentication_result\":1e3}\0",
        b"{\"authentication_result\":4294967296}\0",
        b"{\"authentication_result\":-2147483649}\0",
        b"{\"authentication_result\":\"\"}\0",
        b"{\"authentication_result\":\"0x\"}\0",
        b"{\"authentication_result\":\"abc\"}\0",
        b"{\"authentication_result\":null}\0",
        b"{\"authentication_result\":{}}\0",
    ] {
        assert!(
            decode::<AuthenticationResultPdu>(wire).is_err(),
            "{} must be rejected",
            String::from_utf8_lossy(wire)
        );
    }
}

#[test]
fn authentication_result_display() {
    assert_eq!(AuthenticationResult::S_OK.to_string(), "S_OK (0x00000000)");
    assert_eq!(
        AuthenticationResult::E_ACCESSDENIED.to_string(),
        "E_ACCESSDENIED (0x80070005)"
    );
    assert_eq!(AuthenticationResult(0x8000_4005).to_string(), "0x80004005");
}

#[test]
fn decode_ignores_unknown_members_and_whitespace() {
    let wire = b" { \"extra\" : [1, {\"x\": null}], \"ts_nonce\" : \"n\\u00e9\" } \0";
    let pdu = decode::<ServerNoncePdu>(wire).unwrap();
    assert_eq!(pdu.ts_nonce, "n\u{e9}");
}

#[test]
fn decode_stops_at_the_terminator() {
    let mut wire = SERVER_NONCE_WIRE.to_vec();
    wire.extend_from_slice(b"next");

    let mut cursor = ReadCursor::new(&wire);
    <ServerNoncePdu as ironrdp_core::Decode<'_>>::decode(&mut cursor).unwrap();
    assert_eq!(cursor.remaining(), b"next");
}

#[test]
fn decode_rejects_missing_terminator() {
    let wire = &SERVER_NONCE_WIRE[..SERVER_NONCE_WIRE.len() - 1];
    assert!(decode::<ServerNoncePdu>(wire).is_err());
}

#[test]
fn decode_rejects_oversized_pdu() {
    let mut wire = b"{\"ts_nonce\":\"".to_vec();
    wire.resize(MAX_PDU_SIZE, b'a');
    wire.extend_from_slice(b"\"}\0");
    assert!(decode::<ServerNoncePdu>(&wire).is_err());
}

#[test]
fn decode_accepts_pdu_of_maximum_size() {
    let mut wire = b"{\"ts_nonce\":\"".to_vec();
    wire.resize(MAX_PDU_SIZE - 3, b'a');
    wire.extend_from_slice(b"\"}\0");
    assert_eq!(wire.len(), MAX_PDU_SIZE);
    assert!(decode::<ServerNoncePdu>(&wire).is_ok());
}

#[test]
fn decode_rejects_invalid_utf8() {
    let wire = b"{\"ts_nonce\":\"\xff\"}\0";
    assert!(decode::<ServerNoncePdu>(wire).is_err());
}

#[test]
fn decode_rejects_missing_or_mistyped_member() {
    for wire in [
        &b"{}\0"[..],
        b"{\"nonce\":\"n\"}\0",
        b"{\"ts_nonce\":1}\0",
        b"{\"ts_nonce\":\"a\",\"ts_nonce\":\"b\"}\0",
        b"\0",
        b"not json\0",
    ] {
        assert!(
            decode::<ServerNoncePdu>(wire).is_err(),
            "{} must be rejected",
            String::from_utf8_lossy(wire)
        );
    }
}

#[test]
fn round_trip_escapes_special_characters() {
    let original = ServerNoncePdu {
        ts_nonce: "quote\" backslash\\ nul\0 newline\n \u{e9}\u{1f600}".to_owned(),
    };
    let encoded = encode_vec(&original).unwrap();
    assert_eq!(encoded.iter().filter(|&&b| b == 0).count(), 1);
    assert_eq!(decode::<ServerNoncePdu>(&encoded).unwrap(), original);
}

#[test]
fn hint_frames_up_to_the_terminator() {
    assert_eq!(RDSAAD_HINT.find_size(b"").unwrap(), None);
    assert_eq!(RDSAAD_HINT.find_size(b"{\"ts_nonce\":").unwrap(), None);
    assert_eq!(
        RDSAAD_HINT.find_size(SERVER_NONCE_WIRE).unwrap(),
        Some((true, SERVER_NONCE_WIRE.len()))
    );

    let mut two = SERVER_NONCE_WIRE.to_vec();
    two.extend_from_slice(AUTH_RESULT_WIRE);
    assert_eq!(
        RDSAAD_HINT.find_size(&two).unwrap(),
        Some((true, SERVER_NONCE_WIRE.len()))
    );
}

#[test]
fn hint_enforces_maximum_size() {
    let mut bytes = vec![b'a'; MAX_PDU_SIZE - 1];
    assert_eq!(RDSAAD_HINT.find_size(&bytes).unwrap(), None);

    bytes.push(0);
    assert_eq!(RDSAAD_HINT.find_size(&bytes).unwrap(), Some((true, MAX_PDU_SIZE)));

    bytes[MAX_PDU_SIZE - 1] = b'a';
    assert!(RDSAAD_HINT.find_size(&bytes).is_err());
}

// The PDUs share one JSON reader and writer; the tests below go through `ServerNoncePdu`.

fn nonce_from_json(json: &str) -> Result<String, ironrdp_core::DecodeError> {
    let mut wire = json.as_bytes().to_vec();
    wire.push(0);
    decode::<ServerNoncePdu>(&wire).map(|pdu| pdu.ts_nonce)
}

#[test]
fn json_skips_nested_values() {
    let nonce = nonce_from_json(r#"{"n":{"x":[1,{"y":null},[]],"z":{}},"b":[true,false,-0.5E-2],"ts_nonce":"v"}"#);
    assert_eq!(nonce.unwrap(), "v");
}

#[test]
fn json_decodes_escapes() {
    let nonce = nonce_from_json(r#"{"ts_nonce":"\"\\\/\b\f\n\r\t\u00e9\ud83d\ude00"}"#);
    assert_eq!(nonce.unwrap(), "\"\\/\u{8}\u{c}\n\r\t\u{e9}\u{1f600}");
}

#[test]
fn json_keeps_raw_utf8() {
    assert_eq!(
        nonce_from_json("{\"ts_nonce\":\"\u{e9}\u{1f600}\"}").unwrap(),
        "\u{e9}\u{1f600}"
    );
}

#[test]
fn json_rejects_malformed_input() {
    for json in [
        "",
        "[]",
        "\"s\"",
        "{",
        "{\"ts_nonce\"}",
        "{\"ts_nonce\":}",
        "{\"ts_nonce\":\"n\",}",
        "{\"ts_nonce\":\"n\"}x",
        "{\"ts_nonce\":\"n\",\"a\":01}",
        "{\"ts_nonce\":\"n\",\"a\":1.}",
        "{\"ts_nonce\":\"n\",\"a\":1e}",
        "{\"ts_nonce\":\"n\",\"a\":-}",
        "{\"ts_nonce\":\"n\",\"a\":tree}",
        "{\"ts_nonce\":\"n\",\"a\":[1 2]}",
        "{\"ts_nonce\":\"n}",
        "{\"ts_nonce\":\"\\x\"}",
        "{\"ts_nonce\":\"\\u12\"}",
        "{\"ts_nonce\":\"\\ud800\"}",
        "{\"ts_nonce\":\"\\udc00\"}",
        "{\"ts_nonce\":\"\\ud800\\u0041\"}",
        "{\"ts_nonce\":\"\n\"}",
        "{'ts_nonce':'n'}",
    ] {
        assert!(nonce_from_json(json).is_err(), "{json:?} must be rejected");
    }
}

/// Nesting is bounded so that hostile input cannot exhaust the stack.
#[test]
fn json_limits_nesting_depth() {
    // 16 levels, the top-level object included.
    let within = format!(r#"{{"a":{}{},"ts_nonce":"n"}}"#, "[".repeat(15), "]".repeat(15));
    assert_eq!(nonce_from_json(&within).unwrap(), "n");

    let beyond = format!(r#"{{"a":{}{},"ts_nonce":"n"}}"#, "[".repeat(16), "]".repeat(16));
    assert!(nonce_from_json(&beyond).is_err());
}

#[test]
fn json_escapes_on_encode() {
    for (nonce, expected) in [
        ("plain", &b"{\"ts_nonce\":\"plain\"}\0"[..]),
        ("a\"b\\c/", b"{\"ts_nonce\":\"a\\\"b\\\\c/\"}\0"),
        (
            "\n\r\t\u{8}\u{c}\u{0}\u{1f}",
            b"{\"ts_nonce\":\"\\n\\r\\t\\b\\f\\u0000\\u001f\"}\0",
        ),
        ("\u{e9}", b"{\"ts_nonce\":\"\xc3\xa9\"}\0"),
    ] {
        let pdu = ServerNoncePdu {
            ts_nonce: nonce.to_owned(),
        };
        assert_eq!(encode_vec(&pdu).unwrap(), expected, "{nonce:?}");
    }
}
