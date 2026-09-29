//! RDS AAD Auth (Microsoft Entra ID) client sequence.
//!
//! With PROTOCOL_RDSAAD ([\[MS-RDPBCGR\] 5.4.5.4]), the client proves the user's identity with a
//! Microsoft Entra ID access token bound to a proof-of-possession (PoP) key, instead of CredSSP.
//! Entra enforces MFA and Conditional Access when the token is issued; the server validates the
//! resulting RDP Assertion with Entra.
//!
//! The steps before the TLS handshake happen outside of this crate ([\[MS-RDPBCGR\] 5.4.5.4.1]):
//!
//! 1. Generate a PoP key ([`RsaPopKey`], or any other [`PopSigner`]).
//! 2. Acquire an RDP access token for [`access_token_scope`], sending [`RsaPublicJwk::req_cnf`] as
//!    the `req_cnf` request parameter.
//! 3. Acquire an AAD nonce: POST [`AAD_NONCE_REQUEST_BODY`] to the Entra token endpoint and decode
//!    the response with [`decode_aad_nonce_response`].
//!
//! After the TLS handshake, [`RdsAadSequence`] exchanges the RDS AAD Auth PDUs with the server.
//!
//! # Security
//!
//! Unlike CredSSP, the RDP Assertion is not bound to the TLS channel. Whoever terminates TLS
//! receives an assertion that the genuine server named in the access token accepts, so a relaying
//! man-in-the-middle can log on as the user for as long as the nonces are fresh. Validating the
//! server certificate is therefore the only protection against such a relay: do not accept
//! untrusted certificates when using RDS AAD Auth.
//!
//! [\[MS-RDPBCGR\] 5.4.5.4]: https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-rdpbcgr/dc43f040-d75d-49a9-90c6-0c9999281136
//! [\[MS-RDPBCGR\] 5.4.5.4.1]: https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-rdpbcgr/8f62058b-c7e5-4244-8f14-ed7d76618cb5

use core::fmt;
use core::net::IpAddr;
use std::sync::Arc;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use ironrdp_core::{WriteBuf, decode, encode_buf};
use ironrdp_pdu::PduHint;
use ironrdp_pdu::rdp::rdsaad::{AuthenticationRequestPdu, AuthenticationResultPdu, RDSAAD_HINT, ServerNoncePdu};
use picky::hash::HashAlgorithm;
use picky::jose::jwk::Jwk;
use picky::key::PrivateKey;
use picky::signature::SignatureAlgorithm;
use tracing::{debug, info};

use crate::{
    ConnectorError, ConnectorErrorExt as _, ConnectorResult, Credentials, Written, custom_err, general_err, reason_err,
};

/// Body of the POST request that acquires an AAD nonce from the Entra token endpoint.
///
/// See [\[MS-RDPBCGR\] 4.11.3].
///
/// [\[MS-RDPBCGR\] 4.11.3]: https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-rdpbcgr/46f2d0a6-ff1d-460e-b4b2-c18d13390004
pub const AAD_NONCE_REQUEST_BODY: &str = "grant_type=srv_challenge";

/// Extracts the nonce from the token endpoint's response to [`AAD_NONCE_REQUEST_BODY`].
///
/// The response is a JSON object with a `Nonce` string member ([\[MS-RDPBCGR\] 4.11.3]).
///
/// [\[MS-RDPBCGR\] 4.11.3]: https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-rdpbcgr/46f2d0a6-ff1d-460e-b4b2-c18d13390004
pub fn decode_aad_nonce_response(body: &[u8]) -> ConnectorResult<String> {
    let response: serde_json::Value =
        serde_json::from_slice(body).map_err(|e| custom_err!("invalid AAD nonce response", e))?;

    match response.get("Nonce") {
        Some(serde_json::Value::String(nonce)) if !nonce.is_empty() => Ok(nonce.clone()),
        _ => Err(general_err!("AAD nonce response has no Nonce string")),
    }
}

/// Resource URI identifying the target device by its host name.
///
/// Returns `ms-device-service://termsrv.wvd.microsoft.com/name/<host>`, where `<host>` is the first
/// label of `hostname` (the device name in Entra ID). This is the `u` claim of the RDP Assertion;
/// [`access_token_scope`] derives the token scope from it.
///
/// IP addresses are rejected: Entra identifies the device by name.
///
/// See [\[MS-RDPBCGR\] 5.4.5.4.1].
///
/// [\[MS-RDPBCGR\] 5.4.5.4.1]: https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-rdpbcgr/8f62058b-c7e5-4244-8f14-ed7d76618cb5
pub fn resource_uri(hostname: &str) -> ConnectorResult<String> {
    // A colon only appears in IPv6 literals, bracketed or not.
    if hostname.parse::<IpAddr>().is_ok() || hostname.contains(':') {
        return Err(general_err!("RDS AAD Auth requires a host name, not an IP address"));
    }

    let device_name = hostname.split('.').next().unwrap_or_default();

    if device_name.is_empty() {
        return Err(general_err!("empty host name"));
    }

    Ok(format!(
        "ms-device-service://termsrv.wvd.microsoft.com/name/{device_name}"
    ))
}

/// OAuth 2.0 scope for the RDP access token of `resource_uri`.
pub fn access_token_scope(resource_uri: &str) -> String {
    format!("{resource_uri}/user_impersonation")
}

/// Public part of an RSA PoP key, as JSON Web Key members ([RFC 7517], [RFC 7518] section 6.3.1).
///
/// [RFC 7517]: https://www.rfc-editor.org/rfc/rfc7517
/// [RFC 7518]: https://www.rfc-editor.org/rfc/rfc7518#section-6.3.1
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RsaPublicJwk {
    /// INVARIANT: non-empty base64url (no padding).
    n: String,
    /// INVARIANT: non-empty base64url (no padding).
    e: String,
}

impl RsaPublicJwk {
    /// Builds the key from the unsigned big-endian modulus and public exponent.
    pub fn from_unsigned_be(modulus: &[u8], public_exponent: &[u8]) -> ConnectorResult<Self> {
        // JWK integers are encoded without leading zero octets (RFC 7518 section 2).
        let strip = |bytes: &[u8]| -> Vec<u8> {
            let first = bytes.iter().position(|&b| b != 0).unwrap_or(bytes.len());
            bytes[first..].to_vec()
        };

        let modulus = strip(modulus);
        let public_exponent = strip(public_exponent);

        if modulus.is_empty() || public_exponent.is_empty() {
            return Err(general_err!("RSA modulus and public exponent must be non-zero"));
        }

        Ok(Self {
            n: URL_SAFE_NO_PAD.encode(modulus),
            e: URL_SAFE_NO_PAD.encode(public_exponent),
        })
    }

    /// Builds the key from the `n` and `e` members of a JWK.
    pub fn from_base64url(n: String, e: String) -> ConnectorResult<Self> {
        for value in [&n, &e] {
            let decoded = URL_SAFE_NO_PAD
                .decode(value)
                .map_err(|e| custom_err!("invalid base64url in RSA JWK", e))?;

            if decoded.is_empty() || decoded[0] == 0 {
                return Err(general_err!("RSA JWK members must be non-empty and minimal"));
            }
        }

        Ok(Self { n, e })
    }

    /// The `n` member (modulus), base64url encoded.
    pub fn n(&self) -> &str {
        &self.n
    }

    /// The `e` member (public exponent), base64url encoded.
    pub fn e(&self) -> &str {
        &self.e
    }

    /// JWK Thumbprint ([RFC 7638]): base64url of the SHA-256 digest of the canonical JWK.
    ///
    /// [RFC 7638]: https://www.rfc-editor.org/rfc/rfc7638
    pub fn thumbprint(&self) -> String {
        // Required members in lexicographic order, no whitespace (RFC 7638 section 3.2). Both
        // values are base64url, so they need no JSON escaping.
        let canonical = format!(r#"{{"e":"{}","kty":"RSA","n":"{}"}}"#, self.e, self.n);
        URL_SAFE_NO_PAD.encode(HashAlgorithm::SHA2_256.digest(canonical.as_bytes()))
    }

    /// Key identifier: base64url of `{"kid":"<thumbprint>"}`.
    ///
    /// Sent as the `req_cnf` parameter of the access token request, which binds the token to this
    /// key, and as the `kid` of the RDP Assertion's JOSE header.
    pub fn req_cnf(&self) -> String {
        URL_SAFE_NO_PAD.encode(format!(r#"{{"kid":"{}"}}"#, self.thumbprint()))
    }
}

/// Holder of the proof-of-possession key that the RDP access token is bound to.
///
/// The key signs the RDP Assertion. Implement this trait to keep the key in a hardware-backed
/// store; [`RsaPopKey`] is an in-memory implementation.
pub trait PopSigner: fmt::Debug + Send + Sync {
    /// Public part of the key.
    fn public_jwk(&self) -> &RsaPublicJwk;

    /// Signs `signing_input` with RSASSA-PKCS1-v1_5 using SHA-256 (`RS256`, [RFC 7518] section 3.3).
    ///
    /// [RFC 7518]: https://www.rfc-editor.org/rfc/rfc7518#section-3.3
    fn sign_rs256(&self, signing_input: &[u8]) -> ConnectorResult<Vec<u8>>;
}

/// In-memory RSA PoP key.
///
/// The access token stays bound to this key, so persist the key (see [`Self::to_pem`]) for as long
/// as the token is cached.
pub struct RsaPopKey {
    key: PrivateKey,
    jwk: RsaPublicJwk,
}

impl RsaPopKey {
    /// Key size used by [`Self::generate`], as in [\[MS-RDPBCGR\] 5.4.5.4.1].
    ///
    /// [\[MS-RDPBCGR\] 5.4.5.4.1]: https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-rdpbcgr/8f62058b-c7e5-4244-8f14-ed7d76618cb5
    pub const BITS: usize = 2048;

    /// Generates a new RSA key of [`Self::BITS`] bits.
    pub fn generate() -> ConnectorResult<Self> {
        let key = PrivateKey::generate_rsa(Self::BITS).map_err(|e| custom_err!("RSA key generation", e))?;
        Self::from_private_key(key)
    }

    /// Loads a PEM encoded RSA private key (`PRIVATE KEY` or `RSA PRIVATE KEY`).
    pub fn from_pem(pem: &str) -> ConnectorResult<Self> {
        let key = PrivateKey::from_pem_str(pem).map_err(|e| custom_err!("invalid PoP key", e))?;
        Self::from_private_key(key)
    }

    /// Encodes the private key as PKCS#8 PEM.
    pub fn to_pem(&self) -> ConnectorResult<String> {
        self.key.to_pem_str().map_err(|e| custom_err!("PoP key encoding", e))
    }

    fn from_private_key(key: PrivateKey) -> ConnectorResult<Self> {
        let public_key = key.to_public_key().map_err(|e| custom_err!("invalid PoP key", e))?;
        let jwk = Jwk::from_public_key(&public_key).map_err(|e| custom_err!("invalid PoP key", e))?;
        let rsa = jwk
            .key
            .as_rsa()
            .ok_or_else(|| general_err!("PoP key is not an RSA key"))?;

        let modulus = rsa
            .modulus_unsigned_bytes_be()
            .map_err(|e| custom_err!("invalid PoP key", e))?;
        let public_exponent = rsa
            .public_exponent_unsigned_bytes_be()
            .map_err(|e| custom_err!("invalid PoP key", e))?;

        let jwk = RsaPublicJwk::from_unsigned_be(&modulus, &public_exponent)?;

        Ok(Self { key, jwk })
    }
}

impl fmt::Debug for RsaPopKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RsaPopKey")
            .field("jwk", &self.jwk)
            .finish_non_exhaustive()
    }
}

impl PopSigner for RsaPopKey {
    fn public_jwk(&self) -> &RsaPublicJwk {
        &self.jwk
    }

    fn sign_rs256(&self, signing_input: &[u8]) -> ConnectorResult<Vec<u8>> {
        SignatureAlgorithm::RsaPkcs1v15(HashAlgorithm::SHA2_256)
            .sign(signing_input, &self.key)
            .map_err(|e| custom_err!("RS256 signature", e))
    }
}

/// Credentials for RDS AAD Auth, see [`Credentials::RdsAad`].
#[derive(Clone)]
pub struct RdsAadCredentials {
    /// RDP access token bound to `pop_signer` (the `at` claim).
    pub access_token: String,
    /// Resource URI the token was acquired for (the `u` claim), see [`resource_uri`].
    pub resource_uri: String,
    /// Holder of the PoP key the token is bound to.
    pub pop_signer: Arc<dyn PopSigner>,
}

impl fmt::Debug for RdsAadCredentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RdsAadCredentials")
            .field("access_token", &"<redacted>")
            .field("resource_uri", &self.resource_uri)
            .field("pop_signer", &self.pop_signer)
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum RdsAadState {
    /// Waiting for the Server Nonce PDU.
    WaitServerNonce,
    /// The RDP Assertion must be signed, see [`RdsAadSequence::signing_input`].
    SignatureRequired { signing_input: String },
    /// Waiting for the Authentication Result PDU.
    WaitAuthenticationResult,
    /// The server accepted the RDP Assertion.
    Finished,
}

/// Client side of the RDS AAD Auth PDU exchange ([\[MS-RDPBCGR\] 5.4.5.4.1]).
///
/// Drive it right after the TLS handshake, while [`crate::ClientConnector::should_perform_rdsaad`]
/// is `true`:
///
/// 1. read a PDU framed by [`Self::next_pdu_hint`] and pass it to [`Self::process_server_nonce`],
/// 2. sign [`Self::signing_input`] and pass the signature to [`Self::submit_signature`], which
///    writes the Authentication Request PDU (or use [`Self::sign`] to call the [`PopSigner`] of the
///    credentials),
/// 3. read the next PDU and pass it to [`Self::process_authentication_result`],
/// 4. call [`crate::ClientConnector::mark_rdsaad_as_done`].
///
/// The signing step is separate so that a signer that needs I/O (such as an OS broker) does not
/// block the sequence.
///
/// [\[MS-RDPBCGR\] 5.4.5.4.1]: https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-rdpbcgr/8f62058b-c7e5-4244-8f14-ed7d76618cb5
#[derive(Debug)]
pub struct RdsAadSequence {
    credentials: RdsAadCredentials,
    aad_nonce: String,
    state: RdsAadState,
}

impl RdsAadSequence {
    /// Starts the sequence with the credentials from [`Credentials::RdsAad`] and a fresh AAD nonce.
    pub fn init(credentials: &Credentials, aad_nonce: String) -> ConnectorResult<Self> {
        let Credentials::RdsAad(credentials) = credentials else {
            return Err(general_err!("RDS AAD Auth requires RDS AAD credentials"));
        };

        Ok(Self {
            credentials: credentials.clone(),
            aad_nonce,
            state: RdsAadState::WaitServerNonce,
        })
    }

    pub fn state(&self) -> &RdsAadState {
        &self.state
    }

    pub fn is_done(&self) -> bool {
        self.state == RdsAadState::Finished
    }

    pub fn next_pdu_hint(&self) -> Option<&dyn PduHint> {
        match self.state {
            RdsAadState::WaitServerNonce | RdsAadState::WaitAuthenticationResult => Some(&RDSAAD_HINT),
            RdsAadState::SignatureRequired { .. } | RdsAadState::Finished => None,
        }
    }

    /// Decodes the Server Nonce PDU and prepares the RDP Assertion.
    ///
    /// `timestamp` is the current time in seconds since the Unix epoch (the `ts` claim).
    pub fn process_server_nonce(&mut self, input: &[u8], timestamp: u64) -> ConnectorResult<()> {
        if self.state != RdsAadState::WaitServerNonce {
            return Err(general_err!("unexpected Server Nonce PDU"));
        }

        let server_nonce = decode::<ServerNoncePdu>(input).map_err(ConnectorError::decode)?;

        debug!(message = ?server_nonce, "Received");

        let jwk = self.credentials.pop_signer.public_jwk();

        // RDP Assertion, [MS-RDPBCGR] 2.2.18.2.1.
        let header = serde_json::json!({
            "alg": "RS256",
            "kid": jwk.req_cnf(),
        });

        let payload = serde_json::json!({
            "ts": timestamp.to_string(),
            "at": self.credentials.access_token,
            "u": self.credentials.resource_uri,
            "nonce": server_nonce.ts_nonce,
            "cnf": {
                "jwk": {
                    "kty": "RSA",
                    "e": jwk.e(),
                    "n": jwk.n(),
                },
            },
            // The client claims are a JSON document embedded as a string.
            "client_claims": serde_json::json!({ "aad_nonce": self.aad_nonce }).to_string(),
        });

        let signing_input = format!(
            "{}.{}",
            URL_SAFE_NO_PAD.encode(header.to_string()),
            URL_SAFE_NO_PAD.encode(payload.to_string())
        );

        self.state = RdsAadState::SignatureRequired { signing_input };

        Ok(())
    }

    /// JWS Signing Input (`<header>.<payload>`) to sign with the PoP key, while a signature is
    /// required.
    pub fn signing_input(&self) -> Option<&[u8]> {
        match &self.state {
            RdsAadState::SignatureRequired { signing_input } => Some(signing_input.as_bytes()),
            _ => None,
        }
    }

    /// Signs the RDP Assertion with the [`PopSigner`] of the credentials and writes the
    /// Authentication Request PDU.
    pub fn sign(&mut self, output: &mut WriteBuf) -> ConnectorResult<Written> {
        let signing_input = self
            .signing_input()
            .ok_or_else(|| general_err!("no RDP Assertion to sign"))?;

        let signature = self.credentials.pop_signer.sign_rs256(signing_input)?;

        self.submit_signature(&signature, output)
    }

    /// Completes the RDP Assertion with its `RS256` signature and writes the Authentication Request
    /// PDU.
    pub fn submit_signature(&mut self, signature: &[u8], output: &mut WriteBuf) -> ConnectorResult<Written> {
        let RdsAadState::SignatureRequired { signing_input } = &self.state else {
            return Err(general_err!("no RDP Assertion to sign"));
        };

        if signature.is_empty() {
            return Err(general_err!("empty RDP Assertion signature"));
        }

        let request = AuthenticationRequestPdu {
            rdp_assertion: format!("{signing_input}.{}", URL_SAFE_NO_PAD.encode(signature)),
        };

        debug!(message = ?request, "Send");

        let written = encode_buf(&request, output).map_err(ConnectorError::encode)?;

        self.state = RdsAadState::WaitAuthenticationResult;

        Written::from_size(written)
    }

    /// Decodes the Authentication Result PDU.
    ///
    /// Fails unless the server reports `S_OK`.
    pub fn process_authentication_result(&mut self, input: &[u8]) -> ConnectorResult<()> {
        if self.state != RdsAadState::WaitAuthenticationResult {
            return Err(general_err!("unexpected Authentication Result PDU"));
        }

        let result = decode::<AuthenticationResultPdu>(input).map_err(ConnectorError::decode)?;

        debug!(message = ?result, "Received");

        if !result.authentication_result.is_success() {
            return Err(reason_err!(
                "RDS AAD Auth",
                "server rejected the RDP Assertion: {}",
                result.authentication_result
            ));
        }

        info!("RDS AAD Auth succeeded");

        self.state = RdsAadState::Finished;

        Ok(())
    }
}
