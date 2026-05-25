//! Authentication and cryptographic utilities for Polymarket API
//!
//! This module provides EIP-712 signing, HMAC authentication, and header generation
//! for secure communication with the Polymarket CLOB API.

use crate::errors::{PolyfillError, Result};
use crate::types::ApiCredentials;
use alloy_primitives::{hex::encode_prefixed, keccak256, Address, B256, U256};
use alloy_signer::SignerSync;
use alloy_signer_local::PrivateKeySigner;
use alloy_sol_types::{eip712_domain, sol, SolValue};
use base64::engine::Engine;
use hmac::{Hmac, KeyInit, Mac};
use serde::Serialize;
use sha2::Sha256;
use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

// Header constants
const POLY_ADDR_HEADER: &str = "poly_address";
const POLY_SIG_HEADER: &str = "poly_signature";
const POLY_TS_HEADER: &str = "poly_timestamp";
const POLY_NONCE_HEADER: &str = "poly_nonce";
const POLY_API_KEY_HEADER: &str = "poly_api_key";
const POLY_PASS_HEADER: &str = "poly_passphrase";

type Headers = HashMap<&'static str, String>;

// EIP-712 struct for CLOB authentication
sol! {
    struct ClobAuth {
        address address;
        string timestamp;
        uint256 nonce;
        string message;
    }
}

// EIP-712 struct for order signing (V2)
sol! {
    struct Order {
        uint256 salt;
        address maker;
        address signer;
        uint256 tokenId;
        uint256 makerAmount;
        uint256 takerAmount;
        uint8 side;
        uint8 signatureType;
        uint256 timestamp;
        bytes32 metadata;
        bytes32 builder;
    }
}

// EIP-712 struct for V1 order signing (used only by RFQ accept/approve).
// Kept alongside V2 Order because CLOB V2's RFQ protocol still requires V1 signatures.
//
// The V1 on-chain Exchange contract registers this EIP-712 type under the name
// "Order". To preserve that type-name while V2's `Order` sol! struct sits in the
// parent module, this is declared in an inner module and re-exported as OrderV1.
mod v1_order {
    alloy_sol_types::sol! {
        struct Order {
            uint256 salt;
            address maker;
            address signer;
            address taker;
            uint256 tokenId;
            uint256 makerAmount;
            uint256 takerAmount;
            uint256 expiration;
            uint256 nonce;
            uint256 feeRateBps;
            uint8 side;
            uint8 signatureType;
        }
    }
}

pub use v1_order::Order as OrderV1;

/// Get current Unix timestamp in seconds
pub fn get_current_unix_time_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("Time went backwards")
        .as_secs()
}

/// Sign CLOB authentication message using EIP-712
pub fn sign_clob_auth_message(
    signer: &PrivateKeySigner,
    timestamp: String,
    nonce: U256,
) -> Result<String> {
    let message = "This message attests that I control the given wallet".to_string();
    let polygon = 137;

    let auth_struct = ClobAuth {
        address: signer.address(),
        timestamp,
        nonce,
        message,
    };

    let domain = eip712_domain!(
        name: "ClobAuthDomain",
        version: "1",
        chain_id: polygon,
    );

    let signature = signer
        .sign_typed_data_sync(&auth_struct, &domain)
        .map_err(|e| PolyfillError::crypto(format!("EIP-712 signature failed: {}", e)))?;

    Ok(encode_prefixed(signature.as_bytes()))
}

/// Sign order message using EIP-712
pub fn sign_order_message(
    signer: &PrivateKeySigner,
    order: Order,
    chain_id: u64,
    verifying_contract: Address,
) -> Result<String> {
    let domain = eip712_domain!(
        name: "Polymarket CTF Exchange",
        version: "2",
        chain_id: chain_id,
        verifying_contract: verifying_contract,
    );

    let signature = signer
        .sign_typed_data_sync(&order, &domain)
        .map_err(|e| PolyfillError::crypto(format!("Order signature failed: {}", e)))?;

    Ok(encode_prefixed(signature.as_bytes()))
}

// ERC-7739 / Solady `TypedDataSign` wrapping constants for POLY_1271 orders.
//
// Single-line literals: the EIP-712 type-string must contain zero stray
// whitespace, otherwise the resulting type-hash will not match the on-chain
// contract.
const POLY_1271_ORDER_TYPE_STRING: &str = "Order(uint256 salt,address maker,address signer,uint256 tokenId,uint256 makerAmount,uint256 takerAmount,uint8 side,uint8 signatureType,uint256 timestamp,bytes32 metadata,bytes32 builder)";

const POLY_1271_SOLADY_TYPE_STRING: &str = "TypedDataSign(Order contents,string name,string version,uint256 chainId,address verifyingContract,bytes32 salt)Order(uint256 salt,address maker,address signer,uint256 tokenId,uint256 makerAmount,uint256 takerAmount,uint8 side,uint8 signatureType,uint256 timestamp,bytes32 metadata,bytes32 builder)";

const POLY_1271_DOMAIN_TYPE_STRING: &str =
    "EIP712Domain(string name,string version,uint256 chainId,address verifyingContract)";

/// Sign an order using the ERC-7739 / Solady `TypedDataSign` envelope required
/// for POLY_1271 (EIP-1271 smart-wallet) orders.
///
/// The CTF Exchange V2 contract dispatches signature validation to the smart
/// wallet's `isValidSignature(...)` via ERC-1271 when `signatureType == 3`.
/// The smart wallet expects an ERC-7739 nested payload so that the controlling
/// EOA's wallet UI can show the underlying order, not an opaque hash.
///
/// Wire layout:
///
///   inner_sig (65 B) ‖ app_domain_separator (32 B) ‖ contents_hash (32 B)
///                    ‖ contents_type_string (UTF-8) ‖ contents_type_len (uint16 BE)
///
/// Both `Order.maker` and `Order.signer` on the wire must be the smart-wallet
/// (deposit-wallet) address; the inner ECDSA signature is still produced by
/// the EOA private key that owns the wallet.
pub fn sign_poly_1271_order_message(
    signer: &PrivateKeySigner,
    order: Order,
    chain_id: u64,
    verifying_contract: Address,
) -> Result<String> {
    // App-side EIP-712 domain separator (Polymarket CTF Exchange V2).
    let domain_typehash = keccak256(POLY_1271_DOMAIN_TYPE_STRING.as_bytes());
    let app_name_hash = keccak256(b"Polymarket CTF Exchange");
    let app_version_hash = keccak256(b"2");
    let app_domain_separator = keccak256(
        (
            domain_typehash,
            app_name_hash,
            app_version_hash,
            U256::from(chain_id),
            verifying_contract,
        )
            .abi_encode_sequence(),
    );

    // Order struct hash — the "contents" inside the TypedDataSign envelope.
    // `uint8` and `uint256` share an identical ABI encoding (32-byte
    // left-padded big-endian), so the `u8` order fields are promoted here to
    // keep the tuple inside `SolValue`'s blanket impl.
    let order_typehash = keccak256(POLY_1271_ORDER_TYPE_STRING.as_bytes());
    let contents_hash = keccak256(
        (
            order_typehash,
            order.salt,
            order.maker,
            order.signer,
            order.tokenId,
            order.makerAmount,
            order.takerAmount,
            U256::from(order.side),
            U256::from(order.signatureType),
            order.timestamp,
            order.metadata,
            order.builder,
        )
            .abi_encode_sequence(),
    );

    // Outer TypedDataSign struct hash, anchored to the DepositWallet domain.
    // `order.signer` here is the smart-wallet address (the verifyingContract
    // of the nested DepositWallet domain).
    let solady_typehash = keccak256(POLY_1271_SOLADY_TYPE_STRING.as_bytes());
    let dw_name_hash = keccak256(b"DepositWallet");
    let dw_version_hash = keccak256(b"1");
    let typed_data_sign_struct_hash = keccak256(
        (
            solady_typehash,
            contents_hash,
            dw_name_hash,
            dw_version_hash,
            U256::from(chain_id),
            order.signer,
            B256::ZERO,
        )
            .abi_encode_sequence(),
    );

    // EIP-712 digest is computed against the CTF Exchange V2 app domain —
    // not the DepositWallet domain, which only appears nested inside the
    // TypedDataSign struct hash above.
    let mut digest_preimage = [0u8; 66];
    digest_preimage[0] = 0x19;
    digest_preimage[1] = 0x01;
    digest_preimage[2..34].copy_from_slice(app_domain_separator.as_slice());
    digest_preimage[34..66].copy_from_slice(typed_data_sign_struct_hash.as_slice());
    let digest = keccak256(digest_preimage);

    // Inner ECDSA signature produced by the controlling EOA over the raw
    // digest (no EIP-191 prefix). The smart wallet recovers the EOA and
    // checks ownership via isValidSignature.
    let inner_sig = signer
        .sign_hash_sync(&digest)
        .map_err(|e| PolyfillError::crypto(format!("POLY_1271 inner signature failed: {}", e)))?;
    let inner_sig_bytes = inner_sig.as_bytes();

    let contents_type_bytes = POLY_1271_ORDER_TYPE_STRING.as_bytes();
    let contents_type_len =
        u16::try_from(contents_type_bytes.len()).expect("Order type string fits in u16");

    let mut out =
        Vec::with_capacity(inner_sig_bytes.len() + 32 + 32 + contents_type_bytes.len() + 2);
    out.extend_from_slice(&inner_sig_bytes);
    out.extend_from_slice(app_domain_separator.as_slice());
    out.extend_from_slice(contents_hash.as_slice());
    out.extend_from_slice(contents_type_bytes);
    out.extend_from_slice(&contents_type_len.to_be_bytes());

    Ok(encode_prefixed(&out))
}

/// Sign V1 order message using EIP-712 (RFQ accept/approve path only).
pub fn sign_v1_order_message(
    signer: &PrivateKeySigner,
    order: OrderV1,
    chain_id: u64,
    verifying_contract: Address,
) -> Result<String> {
    let domain = eip712_domain!(
        name: "Polymarket CTF Exchange",
        version: "1",
        chain_id: chain_id,
        verifying_contract: verifying_contract,
    );

    let signature = signer
        .sign_typed_data_sync(&order, &domain)
        .map_err(|e| PolyfillError::crypto(format!("V1 order signature failed: {}", e)))?;

    Ok(encode_prefixed(signature.as_bytes()))
}

/// Build HMAC signature for L2 authentication
///
/// Performs cryptographic message authentication using SHA-256 with
/// specialized key derivation and encoding schemes for API compliance.
pub fn build_hmac_signature<T>(
    secret: &str,
    timestamp: u64,
    method: &str,
    request_path: &str,
    body: Option<&T>,
) -> Result<String>
where
    T: ?Sized + Serialize,
{
    // Apply inverse transformation to key material for digest initialization
    // This ensures compatibility with the expected cryptographic envelope format
    let decoded_secret = base64::engine::general_purpose::URL_SAFE
        .decode(secret)
        .map_err(|e| PolyfillError::crypto(format!("Failed to decode base64 secret: {}", e)))?;

    // Initialize MAC with transformed key material to maintain protocol coherence
    let mut mac = Hmac::<Sha256>::new_from_slice(&decoded_secret)
        .map_err(|e| PolyfillError::crypto(format!("Invalid HMAC key: {}", e)))?;

    // Construct canonical message representation for signature verification
    // Message components are concatenated in strict order to preserve cryptographic binding
    let message = format!(
        "{}{}{}{}",
        timestamp,
        method.to_uppercase(),
        request_path,
        match body {
            Some(b) => serde_json::to_string(b).map_err(|e| PolyfillError::parse(
                format!("Failed to serialize body: {}", e),
                None
            ))?,
            None => String::new(),
        }
    );

    // Compute authentication tag over canonical message form
    mac.update(message.as_bytes());
    let result = mac.finalize();

    // Apply URL-safe encoding transformation for transport layer compatibility
    // This encoding scheme ensures proper signature validation across network boundaries
    Ok(base64::engine::general_purpose::URL_SAFE.encode(result.into_bytes()))
}

/// Create L1 headers for authentication (using private key signature)
///
/// Generates initial authentication envelope using elliptic curve cryptography
/// for establishing trusted communication channels with the distributed ledger API.
pub fn create_l1_headers(signer: &PrivateKeySigner, nonce: Option<U256>) -> Result<Headers> {
    // Capture temporal context for replay prevention at protocol boundary
    let timestamp = get_current_unix_time_secs().to_string();
    let nonce = nonce.unwrap_or(U256::ZERO);

    // Generate EIP-712 compliant signature for cryptographic proof of authority
    let signature = sign_clob_auth_message(signer, timestamp.clone(), nonce)?;
    let address = encode_prefixed(signer.address().as_slice());

    // Assemble primary authentication header set with identity binding
    Ok(HashMap::from([
        (POLY_ADDR_HEADER, address),
        (POLY_SIG_HEADER, signature),
        (POLY_TS_HEADER, timestamp),
        (POLY_NONCE_HEADER, nonce.to_string()),
    ]))
}

/// Create L2 headers for API calls (using API key and HMAC)
///
/// Assembles authentication header set with computed signature digest
/// to satisfy bilateral verification requirements at the protocol layer.
pub fn create_l2_headers<T>(
    signer: &PrivateKeySigner,
    api_creds: &ApiCredentials,
    method: &str,
    req_path: &str,
    body: Option<&T>,
) -> Result<Headers>
where
    T: ?Sized + Serialize,
{
    // Extract identity from signing authority for header binding
    let address = encode_prefixed(signer.address().as_slice());
    let timestamp = get_current_unix_time_secs();

    // Generate cryptographic authenticator using temporal and message context
    let hmac_signature =
        build_hmac_signature(&api_creds.secret, timestamp, method, req_path, body)?;

    // Construct header map with authentication primitives in canonical order
    Ok(HashMap::from([
        (POLY_ADDR_HEADER, address),
        (POLY_SIG_HEADER, hmac_signature),
        (POLY_TS_HEADER, timestamp.to_string()),
        (POLY_API_KEY_HEADER, api_creds.api_key.clone()),
        (POLY_PASS_HEADER, api_creds.passphrase.clone()),
    ]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_unix_timestamp() {
        let timestamp = get_current_unix_time_secs();
        assert!(timestamp > 1_600_000_000); // Should be after 2020
    }

    #[test]
    fn test_hmac_signature() {
        let result = build_hmac_signature::<String>(
            "dGVzdF9zZWNyZXRfa2V5XzEyMzQ1",
            1234567890,
            "GET",
            "/test",
            None,
        );
        assert!(result.is_ok());
    }

    #[test]
    fn test_hmac_signature_with_body() {
        let body = r#"{"test": "data"}"#;
        let result = build_hmac_signature(
            "dGVzdF9zZWNyZXRfa2V5XzEyMzQ1",
            1234567890,
            "POST",
            "/orders",
            Some(body),
        );
        assert!(result.is_ok());
        let signature = result.unwrap();
        assert!(!signature.is_empty());
    }

    #[test]
    fn test_hmac_signature_consistency() {
        let secret = "dGVzdF9zZWNyZXRfa2V5XzEyMzQ1";
        let timestamp = 1234567890;
        let method = "GET";
        let path = "/test";

        let sig1 = build_hmac_signature::<String>(secret, timestamp, method, path, None).unwrap();
        let sig2 = build_hmac_signature::<String>(secret, timestamp, method, path, None).unwrap();

        // Same inputs should produce same signature
        assert_eq!(sig1, sig2);
    }

    #[test]
    fn test_hmac_signature_different_inputs() {
        let secret = "dGVzdF9zZWNyZXRfa2V5XzEyMzQ1";
        let timestamp = 1234567890;

        let sig1 = build_hmac_signature::<String>(secret, timestamp, "GET", "/test", None).unwrap();
        let sig2 =
            build_hmac_signature::<String>(secret, timestamp, "POST", "/test", None).unwrap();
        let sig3 =
            build_hmac_signature::<String>(secret, timestamp, "GET", "/other", None).unwrap();

        // Different inputs should produce different signatures
        assert_ne!(sig1, sig2);
        assert_ne!(sig1, sig3);
        assert_ne!(sig2, sig3);
    }

    #[test]
    fn test_create_l1_headers() {
        use alloy_primitives::U256;
        use alloy_signer_local::PrivateKeySigner;

        let private_key = "0x1234567890123456789012345678901234567890123456789012345678901234";
        let signer: PrivateKeySigner = private_key.parse().expect("Valid private key");

        let result = create_l1_headers(&signer, Some(U256::from(12345)));
        assert!(result.is_ok());

        let headers = result.unwrap();
        assert!(headers.contains_key("poly_address"));
        assert!(headers.contains_key("poly_signature"));
        assert!(headers.contains_key("poly_timestamp"));
        assert!(headers.contains_key("poly_nonce"));
    }

    #[test]
    fn test_create_l1_headers_different_nonces() {
        use alloy_primitives::U256;
        use alloy_signer_local::PrivateKeySigner;

        let private_key = "0x1234567890123456789012345678901234567890123456789012345678901234";
        let signer: PrivateKeySigner = private_key.parse().expect("Valid private key");

        let headers_1 = create_l1_headers(&signer, Some(U256::from(12345))).unwrap();
        let headers_2 = create_l1_headers(&signer, Some(U256::from(54321))).unwrap();

        // Different nonces should produce different signatures
        assert_ne!(
            headers_1.get("poly_signature"),
            headers_2.get("poly_signature")
        );

        // But same address
        assert_eq!(headers_1.get("poly_address"), headers_2.get("poly_address"));
    }

    #[test]
    fn test_create_l2_headers() {
        use alloy_signer_local::PrivateKeySigner;

        let private_key = "0x1234567890123456789012345678901234567890123456789012345678901234";
        let signer: PrivateKeySigner = private_key.parse().expect("Valid private key");

        let api_creds = ApiCredentials {
            api_key: "test_key".to_string(),
            secret: "dGVzdF9zZWNyZXRfa2V5XzEyMzQ1".to_string(),
            passphrase: "test_passphrase".to_string(),
        };

        let result = create_l2_headers::<String>(&signer, &api_creds, "/test", "GET", None);
        assert!(result.is_ok());

        let headers = result.unwrap();
        assert!(headers.contains_key("poly_api_key"));
        assert!(headers.contains_key("poly_signature"));
        assert!(headers.contains_key("poly_timestamp"));
        assert!(headers.contains_key("poly_passphrase"));

        assert_eq!(headers.get("poly_api_key").unwrap(), "test_key");
        assert_eq!(headers.get("poly_passphrase").unwrap(), "test_passphrase");
    }

    #[test]
    fn test_eip712_signature_format() {
        use alloy_primitives::U256;
        use alloy_signer_local::PrivateKeySigner;

        let private_key = "0x1234567890123456789012345678901234567890123456789012345678901234";
        let signer: PrivateKeySigner = private_key.parse().expect("Valid private key");

        // Test that we can create and sign EIP-712 messages
        let result = create_l1_headers(&signer, Some(U256::from(12345)));
        assert!(result.is_ok());

        let headers = result.unwrap();
        let signature = headers.get("poly_signature").unwrap();

        // EIP-712 signatures should be hex strings of specific length
        assert!(signature.starts_with("0x"));
        assert_eq!(signature.len(), 132); // 0x + 130 hex chars = 132 total
    }

    #[test]
    fn test_timestamp_generation() {
        let ts1 = get_current_unix_time_secs();
        std::thread::sleep(std::time::Duration::from_millis(1));
        let ts2 = get_current_unix_time_secs();

        // Timestamps should be increasing
        assert!(ts2 >= ts1);

        // Should be reasonable current time (after 2020, before 2030)
        assert!(ts1 > 1_600_000_000);
        assert!(ts1 < 1_900_000_000);
    }

    #[test]
    fn v2_order_typehash_matches_ts_reference() {
        use alloy_primitives::{keccak256, B256};
        use alloy_sol_types::SolStruct;

        // Single-line literal: `keccak256` input must contain zero whitespace.
        let expected = keccak256(
            "Order(uint256 salt,address maker,address signer,uint256 tokenId,uint256 makerAmount,uint256 takerAmount,uint8 side,uint8 signatureType,uint256 timestamp,bytes32 metadata,bytes32 builder)",
        );

        let dummy = Order {
            salt: U256::ZERO,
            maker: Address::ZERO,
            signer: Address::ZERO,
            tokenId: U256::ZERO,
            makerAmount: U256::ZERO,
            takerAmount: U256::ZERO,
            side: 0,
            signatureType: 0,
            timestamp: U256::ZERO,
            metadata: B256::ZERO,
            builder: B256::ZERO,
        };

        assert_eq!(dummy.eip712_type_hash(), expected);
    }

    /// Byte-for-byte parity with a deterministic POLY_1271 wire signature.
    /// Any deviation in the ERC-7739 / Solady TypedDataSign wrapping (type
    /// strings, domain separator, contents hash, digest, or the wire-format
    /// concatenation) will surface here.
    #[test]
    fn sign_poly_1271_order_matches_reference_fixture() {
        use alloy_primitives::{address, B256};
        use std::str::FromStr;

        // Anvil/Hardhat dev account #0. Public test key; do not reuse on mainnet.
        let private_key = "0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80";
        let signer: PrivateKeySigner = private_key.parse().expect("valid private key");

        let chain_id: u64 = 80002; // Amoy
        let verifying_contract = address!("E111180000d2663C0091e4f400237545B87B996B");
        let deposit_wallet = address!("1111111111111111111111111111111111111111");

        let order = Order {
            salt: U256::from_str("479249096354").unwrap(),
            maker: deposit_wallet,
            signer: deposit_wallet,
            tokenId: U256::from(1234u64),
            makerAmount: U256::from(100_000_000u64),
            takerAmount: U256::from(50_000_000u64),
            side: 0, // BUY
            signatureType: 3,
            timestamp: U256::from(1_710_000_000_000u64),
            metadata: B256::ZERO,
            builder: B256::ZERO,
        };

        let actual =
            sign_poly_1271_order_message(&signer, order, chain_id, verifying_contract).unwrap();

        let expected = concat!(
            "0xa3a093c83b6c20c83355c16ce94c92e6e9fcbdeb840618cc74f6c57a42ad145b",
            "2b98db73d2c73cbf1f2b6af288566ae81960ddbc3a13921027358a8bff3be6ff1c",
            "a440cbd865bc0c6243d7a8df9a8bf48a8827b0a4abbb61c30e96d305423af148",
            "d23d42d3ad94e65d78258cecaf8dcbaddac0f73dc085040f2c12bb595dd83804",
            "4f726465722875696e743235362073616c742c61646472657373206d616b65722c",
            "61646472657373207369676e65722c75696e7432353620746f6b656e49642c75",
            "696e74323536206d616b6572416d6f756e742c75696e743235362074616b6572",
            "416d6f756e742c75696e743820736964652c75696e7438207369676e61747572",
            "65547970652c75696e743235362074696d657374616d702c6279746573333220",
            "6d657461646174612c62797465733332206275696c6465722900ba",
        );
        assert_eq!(actual, expected);
    }

    #[test]
    fn v1_order_typehash_matches_reference() {
        use alloy_primitives::{keccak256, B256};
        use alloy_sol_types::SolStruct;

        let expected = keccak256(
            "Order(uint256 salt,address maker,address signer,address taker,uint256 tokenId,uint256 makerAmount,uint256 takerAmount,uint256 expiration,uint256 nonce,uint256 feeRateBps,uint8 side,uint8 signatureType)",
        );

        let dummy = OrderV1 {
            salt: U256::ZERO,
            maker: Address::ZERO,
            signer: Address::ZERO,
            taker: Address::ZERO,
            tokenId: U256::ZERO,
            makerAmount: U256::ZERO,
            takerAmount: U256::ZERO,
            expiration: U256::ZERO,
            nonce: U256::ZERO,
            feeRateBps: U256::ZERO,
            side: 0,
            signatureType: 0,
        };

        assert_eq!(dummy.eip712_type_hash(), expected);
        let _ = B256::ZERO;
    }
}
