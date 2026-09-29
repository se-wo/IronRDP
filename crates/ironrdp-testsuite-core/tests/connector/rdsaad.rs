//! RDS AAD Auth client sequence ([MS-RDPBCGR] 5.4.5.4.1).

use std::sync::Arc;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use ironrdp_connector::rdsaad::{
    self, PopSigner as _, RdsAadCredentials, RdsAadSequence, RdsAadState, RsaPopKey, RsaPublicJwk,
};
use ironrdp_connector::{ClientConnector, ClientConnectorState, Credentials, Sequence as _};
use ironrdp_core::{WriteBuf, decode, encode_vec};
use ironrdp_pdu::nego::{ConnectionConfirm, ConnectionRequest, ResponseFlags, SecurityProtocol};
use ironrdp_pdu::rdp::rdsaad::AuthenticationRequestPdu;
use ironrdp_pdu::x224::X224;

/// RSA-2048 key generated with `openssl genpkey -algorithm RSA -pkeyopt rsa_keygen_bits:2048`.
const POP_KEY_PEM: &str = include_str!("../../test_data/rdsaad/pop-key.pem");

/// `n` of `POP_KEY_PEM`, extracted with `openssl rsa -noout -text`.
const POP_KEY_N: &str = "ukhQLkgVkbsGcI_qxgUYbpibULweYEUWhZs0sWyXasCLpRC_Ayyih8VHnF3Ab0eB1gUnbU2kIc1v5hY4Fi8Zhiyz1Aw9cOtjud8zvF7APANRbmN89jbjh6Z84xFwwPEte6gzh74yh598zZxVDceeGB7wl1SyArT-INSg20gKG9T2uwfCLbTn9UPA_H-ZC6A9S1MDFDXH1pJVUp_b9DXmUDN-qZmGwWWUoivANIJQpEcku9Fb8e8XpJr0i9je79LWnLkq55h-LDdQJkIeFKU5QW24ovd9xlP49_ZPzs-rYuFs2enxZf8qnG7J8t54Hxxrnf6ByKdpncxKv9sfK24POQ";

/// `base64url({"kid":"<RFC 7638 thumbprint of POP_KEY_PEM>"})`, computed with Python's `hashlib`.
const POP_KEY_REQ_CNF: &str = "eyJraWQiOiJpc0duVFZVNGFoY3hOZzFIOS00YWV0TE4tVnhFNGFIN2R3RHppcjNJVUpRIn0";

const SERVER_NONCE: &[u8] = b"{\"ts_nonce\":\"server-nonce\"}\0";

const RESOURCE_URI: &str = "ms-device-service://termsrv.wvd.microsoft.com/name/pc01";

fn pop_key() -> RsaPopKey {
    RsaPopKey::from_pem(POP_KEY_PEM).unwrap()
}

fn credentials() -> Credentials {
    Credentials::RdsAad(RdsAadCredentials {
        access_token: "access-token".to_owned(),
        resource_uri: RESOURCE_URI.to_owned(),
        pop_signer: Arc::new(pop_key()),
    })
}

fn json_part(part: &str) -> serde_json::Value {
    serde_json::from_slice(&URL_SAFE_NO_PAD.decode(part).unwrap()).unwrap()
}

/// Runs the sequence up to the Authentication Request PDU and returns the RDP Assertion.
fn send_assertion(sequence: &mut RdsAadSequence) -> String {
    assert!(sequence.next_pdu_hint().is_some());
    sequence.process_server_nonce(SERVER_NONCE, 1_700_000_000).unwrap();
    assert!(sequence.next_pdu_hint().is_none());

    let mut output = WriteBuf::new();
    let written = sequence.sign(&mut output).unwrap();
    assert_eq!(written.size(), Some(output.filled_len()));
    assert_eq!(sequence.state(), &RdsAadState::WaitAuthenticationResult);
    assert!(sequence.next_pdu_hint().is_some());

    decode::<AuthenticationRequestPdu>(output.filled())
        .unwrap()
        .rdp_assertion
}

#[test]
fn jwk_thumbprint_matches_rfc7638_example() {
    // RFC 7638 section 3.1.
    let jwk = RsaPublicJwk::from_base64url(
        "0vx7agoebGcQSuuPiLJXZptN9nndrQmbXEps2aiAFbWhM78LhWx4cbbfAAtVT86zwu1RK7aPFFxuhDR1L6tSoc_BJECPebWKRXjBZCiFV4n3oknjhMstn64tZ_2W-5JsGY4Hc5n9yBXArwl93lqt7_RN5w6Cf0h4QyQ5v-65YGjQR0_FDW2QvzqY368QQMicAtaSqzs8KJZgnYb9c7d0zgdAZHzu6qMQvRL5hajrn1n91CbOpbISD08qNLyrdkt-bFTWhAI4vMQFh6WeZu0fM4lFd2NcRwr3XPksINHaQ-G_xBniIqbw0Ls1jF44-csFCur-kEgU8awapJzKnqDKgw".to_owned(),
        "AQAB".to_owned(),
    )
    .unwrap();

    assert_eq!(jwk.thumbprint(), "NzbLsXh8uDCcd-6MNwXF4W_7noWXFZAfHkxZsRGC9Xs");
}

#[test]
fn jwk_rejects_invalid_members() {
    for (n, e) in [("", "AQAB"), ("AQAB", ""), ("not base64!", "AQAB"), ("AAEC", "AQAB")] {
        assert!(
            RsaPublicJwk::from_base64url(n.to_owned(), e.to_owned()).is_err(),
            "n={n:?} e={e:?}"
        );
    }

    assert!(RsaPublicJwk::from_unsigned_be(&[0, 0], &[1, 0, 1]).is_err());

    // Leading zero octets are stripped.
    let jwk = RsaPublicJwk::from_unsigned_be(&[0, 0xC3, 0x01], &[0, 1, 0, 1]).unwrap();
    assert_eq!(jwk.n(), "wwE");
    assert_eq!(jwk.e(), "AQAB");
}

#[test]
fn pop_key_matches_openssl() {
    let key = pop_key();

    assert_eq!(key.public_jwk().n(), POP_KEY_N);
    assert_eq!(key.public_jwk().e(), "AQAB");
    assert_eq!(key.public_jwk().req_cnf(), POP_KEY_REQ_CNF);

    // `openssl dgst -sha256 -sign pop-key.pem` over the same input. RSASSA-PKCS1-v1_5 is
    // deterministic, so the signatures match byte for byte.
    let signature = key.sign_rs256(b"eyJhbGciOiJSUzI1NiJ9.eyJ0cyI6IjEifQ").unwrap();
    assert_eq!(
        URL_SAFE_NO_PAD.encode(signature),
        "WgJ5GGcrhdHGrc3NLgi_gZ1SuleToaWOOSBQBYKqoE1zbJ7yiuY05JIp7LbbJQuFLntd0XvpqTkCdFK2DuWtR2JIx5rXwvj9r_q3Rb2OlPlYe84c3yGjZsv2E-_p1Erx-e4h1bKDVFcE0NUpDWiMte0q56t7Lss5T7_eU07nevvVM6JU83swUj4Z4AA5jvkLfwhqm9ghHkWNjNWBR6JB6z5Cig2uQpjU2vEG2TUAuz2Gq0q_KGnKZqOAw0HJMboGKPwAcAcsDzCss_qwJITBmOBSb-cn25MMK23HEjw_Ea__shRgW4FsFpaucyoKr7mhPNGoF0A_FXeOrOZDZ4c4-Q"
    );
}

#[test]
fn pop_key_pem_round_trip() {
    let key = pop_key();
    let reloaded = RsaPopKey::from_pem(&key.to_pem().unwrap()).unwrap();
    assert_eq!(reloaded.public_jwk(), key.public_jwk());
}

#[test]
fn pop_key_debug_hides_private_key() {
    let debug = format!("{:?}", pop_key());
    assert!(debug.contains(POP_KEY_N));
    assert!(!debug.contains("PRIVATE"));
}

#[test]
fn credentials_debug_hides_access_token() {
    let debug = format!("{:?}", credentials());
    assert!(!debug.contains("access-token"));
    assert!(debug.contains(RESOURCE_URI));
}

#[test]
fn resource_uri_uses_the_device_name() {
    assert_eq!(rdsaad::resource_uri("pc01.contoso.com").unwrap(), RESOURCE_URI);
    assert_eq!(rdsaad::resource_uri("pc01").unwrap(), RESOURCE_URI);
    assert_eq!(
        rdsaad::access_token_scope(RESOURCE_URI),
        "ms-device-service://termsrv.wvd.microsoft.com/name/pc01/user_impersonation"
    );

    for hostname in ["", ".contoso.com", "10.0.0.5", "::1", "[::1]"] {
        assert!(rdsaad::resource_uri(hostname).is_err(), "{hostname:?}");
    }
}

#[test]
fn aad_nonce_response() {
    assert_eq!(
        rdsaad::decode_aad_nonce_response(br#"{"Nonce":"AwABAAAAAAACAOz_"}"#).unwrap(),
        "AwABAAAAAAACAOz_"
    );

    for body in [&b""[..], b"{}", br#"{"Nonce":""}"#, br#"{"Nonce":1}"#, b"<html>"] {
        assert!(
            rdsaad::decode_aad_nonce_response(body).is_err(),
            "{}",
            String::from_utf8_lossy(body)
        );
    }
}

#[test]
fn sequence_builds_the_rdp_assertion() {
    let mut sequence = RdsAadSequence::init(&credentials(), "aad-nonce".to_owned()).unwrap();
    let assertion = send_assertion(&mut sequence);

    let parts: Vec<&str> = assertion.split('.').collect();
    let [header, payload, signature] = parts.as_slice() else {
        panic!("JWS Compact Serialization has three parts: {assertion}");
    };

    // JOSE header, [MS-RDPBCGR] 2.2.18.2.1.
    assert_eq!(
        json_part(header),
        serde_json::json!({ "alg": "RS256", "kid": POP_KEY_REQ_CNF })
    );

    // JWS payload, [MS-RDPBCGR] 2.2.18.2.1.
    let payload_json = json_part(payload);
    assert_eq!(
        payload_json,
        serde_json::json!({
            "ts": "1700000000",
            "at": "access-token",
            "u": RESOURCE_URI,
            "nonce": "server-nonce",
            "cnf": { "jwk": { "kty": "RSA", "e": "AQAB", "n": POP_KEY_N } },
            "client_claims": r#"{"aad_nonce":"aad-nonce"}"#,
        })
    );

    // RS256 over the JWS Signing Input.
    let signing_input = format!("{header}.{payload}");
    assert_eq!(
        URL_SAFE_NO_PAD.decode(signature).unwrap(),
        pop_key().sign_rs256(signing_input.as_bytes()).unwrap()
    );
}

#[test]
fn sequence_escapes_untrusted_values() {
    let mut sequence = RdsAadSequence::init(&credentials(), "aad\"nonce\\".to_owned()).unwrap();
    sequence
        .process_server_nonce(b"{\"ts_nonce\":\"a\\\"b\"}\0", 0)
        .unwrap();

    let signing_input = String::from_utf8(sequence.signing_input().unwrap().to_vec()).unwrap();
    let payload = json_part(signing_input.split('.').nth(1).unwrap());

    assert_eq!(payload["nonce"], "a\"b");

    let client_claims: serde_json::Value = serde_json::from_str(payload["client_claims"].as_str().unwrap()).unwrap();
    assert_eq!(client_claims["aad_nonce"], "aad\"nonce\\");
}

#[test]
fn sequence_accepts_an_external_signature() {
    let mut sequence = RdsAadSequence::init(&credentials(), "aad-nonce".to_owned()).unwrap();
    sequence.process_server_nonce(SERVER_NONCE, 0).unwrap();

    let signing_input = sequence.signing_input().unwrap().to_vec();

    let mut output = WriteBuf::new();
    assert!(sequence.submit_signature(&[], &mut output).is_err());
    sequence.submit_signature(&[1, 2, 3], &mut output).unwrap();

    let assertion = decode::<AuthenticationRequestPdu>(output.filled())
        .unwrap()
        .rdp_assertion;
    assert_eq!(assertion, format!("{}.AQID", String::from_utf8(signing_input).unwrap()));
}

#[test]
fn sequence_finishes_on_s_ok() {
    let mut sequence = RdsAadSequence::init(&credentials(), "aad-nonce".to_owned()).unwrap();
    send_assertion(&mut sequence);

    sequence
        .process_authentication_result(b"{\"authentication_result\":0}\0")
        .unwrap();

    assert!(sequence.is_done());
    assert!(sequence.next_pdu_hint().is_none());
}

#[test]
fn sequence_fails_on_error_result() {
    let mut sequence = RdsAadSequence::init(&credentials(), "aad-nonce".to_owned()).unwrap();
    send_assertion(&mut sequence);

    let error = sequence
        .process_authentication_result(b"{\"authentication_result\":2147942405}\0")
        .unwrap_err();

    assert!(error.to_string().contains("E_ACCESSDENIED"), "{error}");
    assert!(!sequence.is_done());
}

#[test]
fn sequence_rejects_out_of_order_pdus() {
    let mut sequence = RdsAadSequence::init(&credentials(), "aad-nonce".to_owned()).unwrap();

    let mut output = WriteBuf::new();
    assert!(sequence.signing_input().is_none());
    assert!(sequence.sign(&mut output).is_err());
    assert!(
        sequence
            .process_authentication_result(b"{\"authentication_result\":0}\0")
            .is_err()
    );

    sequence.process_server_nonce(SERVER_NONCE, 0).unwrap();
    assert!(sequence.process_server_nonce(SERVER_NONCE, 0).is_err());
    assert!(
        sequence
            .process_authentication_result(b"{\"authentication_result\":0}\0")
            .is_err()
    );
    assert!(!sequence.is_done());
}

#[test]
fn sequence_requires_rdsaad_credentials() {
    let credentials = Credentials::UsernamePassword {
        username: "user".to_owned(),
        password: "password".to_owned(),
    };
    assert!(RdsAadSequence::init(&credentials, "aad-nonce".to_owned()).is_err());
}

/// Entra ID credentials must not let the server fall back to a logon without Entra ID.
#[test]
fn connector_requests_only_rdsaad() {
    let mut config = super::test_config();
    config.enable_credssp = true;
    config.credentials = credentials();

    let mut connector = ClientConnector::new(config, "127.0.0.1:3389".parse().unwrap());

    let mut output = WriteBuf::new();
    connector.step_no_input(&mut output).unwrap();

    let request = decode::<X224<ConnectionRequest>>(output.filled()).unwrap().0;
    assert_eq!(request.protocol, SecurityProtocol::RDSAAD);
    assert!(request.nego_data.is_none(), "no mstshash cookie without a username");
}

#[test]
fn connector_hands_over_to_the_rdsaad_sequence() {
    let mut config = super::test_config();
    config.credentials = credentials();

    let mut connector = ClientConnector::new(config, "127.0.0.1:3389".parse().unwrap());

    let mut output = WriteBuf::new();
    connector.step_no_input(&mut output).unwrap();

    let confirm = encode_vec(&X224(ConnectionConfirm::Response {
        flags: ResponseFlags::empty(),
        protocol: SecurityProtocol::RDSAAD,
    }))
    .unwrap();
    connector.step(&confirm, None, &mut output).unwrap();

    assert!(connector.should_perform_security_upgrade());
    connector.mark_security_upgrade_as_done();
    assert!(connector.should_perform_rdsaad());

    // The connector cannot be stepped past RDS AAD Auth.
    let mut sequence = RdsAadSequence::init(&connector.config.credentials, "aad-nonce".to_owned()).unwrap();
    assert!(connector.mark_rdsaad_as_done(&sequence).is_err());
    assert!(connector.should_perform_rdsaad());

    send_assertion(&mut sequence);
    sequence
        .process_authentication_result(b"{\"authentication_result\":0}\0")
        .unwrap();

    connector.mark_rdsaad_as_done(&sequence).unwrap();
    assert!(matches!(
        connector.state,
        ClientConnectorState::BasicSettingsExchangeSendInitial { selected_protocol }
            if selected_protocol == SecurityProtocol::RDSAAD
    ));
}

#[test]
fn connector_refuses_to_skip_rdsaad() {
    let mut config = super::test_config();
    config.credentials = credentials();

    let mut connector = ClientConnector::new(config, "127.0.0.1:3389".parse().unwrap());
    connector.state = ClientConnectorState::RdsAad {
        selected_protocol: SecurityProtocol::RDSAAD,
    };

    assert!(connector.step_no_input(&mut WriteBuf::new()).is_err());
}
