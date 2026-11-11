use std::{fs, path::PathBuf};

use anyhow::{bail, Context};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use jsonwebtoken::EncodingKey;
use ring::{
    rand::SystemRandom,
    signature::{Ed25519KeyPair, KeyPair},
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::config::KeysConfig;

/// Signing material loaded once at startup.
///
/// `encoding` is the private key used by jsonwebtoken; only the public JWK is
/// exposed over HTTP.
pub struct KeyMaterial {
    pub kid: String,
    pub encoding: EncodingKey,
    #[allow(dead_code)]
    pub public_jwk: Value,
    pub jwks_json: String,
    pub jwks_etag: String,
}

impl std::fmt::Debug for KeyMaterial {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("KeyMaterial")
            .field("kid", &self.kid)
            .finish_non_exhaustive()
    }
}

pub fn load_or_generate(config: &KeysConfig) -> anyhow::Result<KeyMaterial> {
    let (pem_text, keypair) = if config.private_key_path.exists() {
        let pem_text = fs::read_to_string(&config.private_key_path).with_context(|| {
            format!(
                "cannot read private key at {}",
                config.private_key_path.display()
            )
        })?;
        let keypair = parse_pkcs8(&pem_text)?;
        (pem_text, keypair)
    } else {
        if !config.generate_if_missing {
            bail!(
                "private key {} does not exist and keys.generate_if_missing is false",
                config.private_key_path.display()
            );
        }
        generate(&config.private_key_path)?
    };

    let encoding = EncodingKey::from_ed_pem(pem_text.as_bytes())
        .context("jsonwebtoken rejected the Ed25519 PEM key")?;

    let raw_public = keypair.public_key().as_ref();
    let x = URL_SAFE_NO_PAD.encode(raw_public);

    // RFC 7638 JWK thumbprint for an Ed25519 key.
    let canonical = format!(r#"{{"crv":"Ed25519","kty":"OKP","x":"{x}"}}"#);
    let kid = URL_SAFE_NO_PAD.encode(Sha256::digest(canonical.as_bytes()));

    let public_jwk = json!({
        "kty": "OKP",
        "crv": "Ed25519",
        "x": x,
        "kid": kid,
        "alg": "EdDSA",
        "use": "sig",
    });
    let jwks = json!({ "keys": [public_jwk.clone()] });
    let jwks_json = serde_json::to_string(&jwks)?;
    let jwks_etag = format!(
        "\"{}\"",
        URL_SAFE_NO_PAD.encode(Sha256::digest(jwks_json.as_bytes()))
    );

    Ok(KeyMaterial {
        kid,
        encoding,
        public_jwk,
        jwks_json,
        jwks_etag,
    })
}

fn parse_pkcs8(pem_text: &str) -> anyhow::Result<Ed25519KeyPair> {
    let pem = pem::parse(pem_text).context("private key is not valid PEM")?;
    if pem.tag() != "PRIVATE KEY" {
        bail!(
            "expected an unencrypted PKCS#8 'PRIVATE KEY' PEM block, found '{}'",
            pem.tag()
        );
    }
    Ed25519KeyPair::from_pkcs8(pem.contents())
        .map_err(|_| anyhow::anyhow!("invalid Ed25519 PKCS#8 private key"))
}

fn generate(path: &PathBuf) -> anyhow::Result<(String, Ed25519KeyPair)> {
    let rng = SystemRandom::new();
    let pkcs8 = Ed25519KeyPair::generate_pkcs8(&rng)
        .map_err(|_| anyhow::anyhow!("failed to generate Ed25519 key"))?;
    let keypair = Ed25519KeyPair::from_pkcs8(pkcs8.as_ref())
        .map_err(|_| anyhow::anyhow!("failed to load freshly generated Ed25519 key"))?;

    let pem_text = pem::encode(&pem::Pem::new("PRIVATE KEY", pkcs8.as_ref().to_vec()));

    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent)
            .with_context(|| format!("cannot create key directory {}", parent.display()))?;
    }
    fs::write(path, &pem_text)
        .with_context(|| format!("cannot write private key to {}", path.display()))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let permissions = fs::Permissions::from_mode(0o600);
        fs::set_permissions(path, permissions)
            .with_context(|| format!("cannot restrict permissions on {}", path.display()))?;
    }

    Ok((pem_text, keypair))
}

#[cfg(test)]
mod tests {
    use jsonwebtoken::{decode, encode, Algorithm, DecodingKey, Validation};
    use serde::{Deserialize, Serialize};

    use super::*;

    #[derive(Debug, Serialize, Deserialize)]
    struct TestClaims {
        sub: String,
        iss: String,
        aud: String,
        exp: u64,
    }

    #[test]
    fn generates_key_and_roundtrips_a_token() {
        let dir = tempfile::tempdir().expect("tempdir");
        let config = KeysConfig {
            private_key_path: dir.path().join("ed25519.pem"),
            generate_if_missing: true,
        };

        let material = load_or_generate(&config).expect("generate key");
        assert!(config.private_key_path.exists());

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_secs();
        let claims = TestClaims {
            sub: "demo-client".into(),
            iss: "https://issuer.test".into(),
            aud: "rusto2".into(),
            exp: now + 60,
        };

        let mut header = jsonwebtoken::Header::new(Algorithm::EdDSA);
        header.kid = Some(material.kid.clone());
        let token = encode(&header, &claims, &material.encoding).expect("sign");

        let jwk: jsonwebtoken::jwk::Jwk =
            serde_json::from_value(material.public_jwk.clone()).expect("jwk");
        let decoding = DecodingKey::from_jwk(&jwk).expect("decoding key");

        let mut validation = Validation::new(Algorithm::EdDSA);
        validation.set_issuer(&["https://issuer.test"]);
        validation.set_audience(&["rusto2"]);
        let decoded = decode::<TestClaims>(&token, &decoding, &validation).expect("verify");
        assert_eq!(decoded.claims.sub, "demo-client");
    }

    #[test]
    fn refuses_missing_key_when_generation_is_disabled() {
        let dir = tempfile::tempdir().expect("tempdir");
        let config = KeysConfig {
            private_key_path: dir.path().join("absent.pem"),
            generate_if_missing: false,
        };
        assert!(load_or_generate(&config).is_err());
    }
}
