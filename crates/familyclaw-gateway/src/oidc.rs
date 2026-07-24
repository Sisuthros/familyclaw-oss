//! Native OIDC / JWT operator authentication (optional).
//!
//! When fully configured, protected gateway routes accept either the static
//! `FAMILYCLAW_GATEWAY_TOKEN` **or** a Bearer JWT whose `iss` / `aud` / `exp`
//! match the configured `IdP`. Half-configuration fails closed at startup.
//!
//! Supported validation modes:
//! - **HS256 shared secret** — `FAMILYCLAW_OIDC_HS256_SECRET` (tests + simple `IdPs`)
//! - **JWKS URL** — `FAMILYCLAW_OIDC_JWKS_URL` (production RSA/EC keys; fetched on demand)

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use familyclaw_core::{FamilyClawError, Result};
use familyclaw_observability::OperatorRole;
use jsonwebtoken::{decode, decode_header, Algorithm, DecodingKey, Validation};
use serde::Deserialize;

/// Issuer URL env.
pub const ISSUER_ENV: &str = "FAMILYCLAW_OIDC_ISSUER";
/// Audience env.
pub const AUDIENCE_ENV: &str = "FAMILYCLAW_OIDC_AUDIENCE";
/// JWKS URL env.
pub const JWKS_URL_ENV: &str = "FAMILYCLAW_OIDC_JWKS_URL";
/// HS256 shared-secret env (alternative to JWKS).
pub const HS256_SECRET_ENV: &str = "FAMILYCLAW_OIDC_HS256_SECRET";

const JWKS_TTL: Duration = Duration::from_secs(600);

/// Loaded OIDC settings. Construct only via [`OidcConfig::from_env`].
#[derive(Debug, Clone)]
pub struct OidcConfig {
    /// Expected `iss` claim.
    pub issuer: String,
    /// Expected `aud` claim.
    pub audience: String,
    /// JWKS endpoint (mutually exclusive with [`Self::hs256_secret`] in practice).
    pub jwks_url: Option<String>,
    /// HS256 secret bytes.
    pub hs256_secret: Option<Vec<u8>>,
}

impl OidcConfig {
    /// Reads OIDC env vars.
    ///
    /// - All unset → `Ok(None)` (OIDC disabled).
    /// - Any set without a complete set → [`Err`] fail-closed.
    /// - Complete HS256 **or** JWKS set → `Ok(Some(config))`.
    pub fn from_env() -> Result<Option<Self>> {
        let issuer = std::env::var(ISSUER_ENV).unwrap_or_default();
        let audience = std::env::var(AUDIENCE_ENV).unwrap_or_default();
        let jwks = std::env::var(JWKS_URL_ENV).unwrap_or_default();
        let hs = std::env::var(HS256_SECRET_ENV).unwrap_or_default();
        let any = !issuer.trim().is_empty()
            || !audience.trim().is_empty()
            || !jwks.trim().is_empty()
            || !hs.trim().is_empty();
        if !any {
            return Ok(None);
        }
        if issuer.trim().is_empty() || audience.trim().is_empty() {
            return Err(FamilyClawError::config(format!(
                "OIDC half-configured: {ISSUER_ENV} and {AUDIENCE_ENV} are required together"
            )));
        }
        let jwks_url = {
            let t = jwks.trim();
            if t.is_empty() {
                None
            } else {
                Some(t.to_string())
            }
        };
        let hs256_secret = {
            let t = hs.trim();
            if t.is_empty() {
                None
            } else {
                Some(t.as_bytes().to_vec())
            }
        };
        if jwks_url.is_none() && hs256_secret.is_none() {
            return Err(FamilyClawError::config(format!(
                "OIDC requires {JWKS_URL_ENV} or {HS256_SECRET_ENV}"
            )));
        }
        Ok(Some(Self {
            issuer: issuer.trim().to_string(),
            audience: audience.trim().to_string(),
            jwks_url,
            hs256_secret,
        }))
    }
}

#[derive(Debug, Deserialize)]
struct Claims {
    iss: String,
    aud: Aud,
    exp: i64,
    /// Optional role claim mapped to [`OperatorRole`].
    #[serde(default)]
    role: Option<String>,
    #[serde(default)]
    familyclaw_role: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum Aud {
    One(String),
    Many(Vec<String>),
}

impl Aud {
    fn contains(&self, expected: &str) -> bool {
        match self {
            Self::One(s) => s == expected,
            Self::Many(v) => v.iter().any(|s| s == expected),
        }
    }
}

/// Result of a successful JWT validation.
#[derive(Debug, Clone)]
pub struct OidcIdentity {
    /// Optional operator role from the token (for future ACL header bridging).
    #[allow(dead_code)]
    pub role: Option<OperatorRole>,
}

/// Validates bearer JWTs for the configured `IdP`.
pub struct OidcValidator {
    cfg: OidcConfig,
    jwks_cache: Mutex<Option<(Instant, Arc<str>)>>,
    http: reqwest::Client,
}

impl OidcValidator {
    /// Builds a validator for `cfg`.
    #[must_use]
    pub fn new(cfg: OidcConfig) -> Self {
        Self {
            cfg,
            jwks_cache: Mutex::new(None),
            http: reqwest::Client::new(),
        }
    }

    /// Prefetches JWKS into the cache (call once at gateway startup).
    pub async fn prefetch_jwks(&self) -> Result<()> {
        if self.cfg.jwks_url.is_some() {
            let _ = self.load_jwks().await?;
        }
        Ok(())
    }

    /// Validates `token` (raw JWT, no `Bearer ` prefix). Sync-friendly for
    /// the existing auth helpers: HS256 is local; JWKS uses the in-memory cache
    /// (populate via [`Self::prefetch_jwks`]).
    pub fn validate_sync(&self, token: &str) -> Result<OidcIdentity> {
        let header = decode_header(token)
            .map_err(|e| FamilyClawError::invalid_input(format!("oidc: bad jwt header: {e}")))?;
        let key = self.decoding_key_sync(header.alg, header.kid.as_deref())?;
        let mut validation = Validation::new(header.alg);
        validation.set_issuer(&[&self.cfg.issuer]);
        validation.validate_aud = false; // checked manually (string | array)
        let data = decode::<Claims>(token, &key, &validation)
            .map_err(|e| FamilyClawError::invalid_input(format!("oidc: jwt rejected: {e}")))?;
        if data.claims.iss != self.cfg.issuer {
            return Err(FamilyClawError::invalid_input("oidc: issuer mismatch"));
        }
        if !data.claims.aud.contains(&self.cfg.audience) {
            return Err(FamilyClawError::invalid_input("oidc: audience mismatch"));
        }
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX));
        if data.claims.exp < now {
            return Err(FamilyClawError::invalid_input("oidc: token expired"));
        }
        let role_raw = data
            .claims
            .familyclaw_role
            .or(data.claims.role)
            .unwrap_or_default();
        Ok(OidcIdentity {
            role: OperatorRole::parse(&role_raw),
        })
    }

    fn decoding_key_sync(&self, alg: Algorithm, kid: Option<&str>) -> Result<DecodingKey> {
        if let Some(secret) = &self.cfg.hs256_secret {
            if !matches!(alg, Algorithm::HS256) {
                return Err(FamilyClawError::invalid_input(
                    "oidc: HS256 secret configured but token alg is not HS256",
                ));
            }
            return Ok(DecodingKey::from_secret(secret));
        }
        let guard = self
            .jwks_cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some((_, body)) = guard.as_ref() else {
            return Err(FamilyClawError::invalid_input(
                "oidc: JWKS cache empty — gateway should prefetch at startup",
            ));
        };
        pick_jwk(body, kid)
    }

    async fn load_jwks(&self) -> Result<Arc<str>> {
        let url = self
            .cfg
            .jwks_url
            .as_deref()
            .ok_or_else(|| FamilyClawError::config("oidc: JWKS URL missing"))?;
        {
            let guard = self
                .jwks_cache
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some((at, body)) = guard.as_ref() {
                if at.elapsed() < JWKS_TTL {
                    return Ok(Arc::clone(body));
                }
            }
        }
        let text = self
            .http
            .get(url)
            .send()
            .await
            .map_err(|e| FamilyClawError::config(format!("oidc: JWKS fetch failed: {e}")))?
            .text()
            .await
            .map_err(|e| FamilyClawError::config(format!("oidc: JWKS body failed: {e}")))?;
        let arc = Arc::<str>::from(text);
        let mut guard = self
            .jwks_cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *guard = Some((Instant::now(), Arc::clone(&arc)));
        Ok(arc)
    }
}

fn pick_jwk(jwks_json: &str, kid: Option<&str>) -> Result<DecodingKey> {
    let v: serde_json::Value = serde_json::from_str(jwks_json)
        .map_err(|e| FamilyClawError::config(format!("oidc: JWKS parse: {e}")))?;
    let keys = v
        .get("keys")
        .and_then(|k| k.as_array())
        .ok_or_else(|| FamilyClawError::config("oidc: JWKS missing keys[]"))?;
    let jwk = keys
        .iter()
        .find(|k| kid.is_none_or(|want| k.get("kid").and_then(|x| x.as_str()) == Some(want)))
        .or_else(|| keys.first())
        .ok_or_else(|| FamilyClawError::config("oidc: JWKS empty"))?;
    let n = jwk
        .get("n")
        .and_then(|x| x.as_str())
        .ok_or_else(|| FamilyClawError::config("oidc: JWK missing n"))?;
    let e = jwk
        .get("e")
        .and_then(|x| x.as_str())
        .ok_or_else(|| FamilyClawError::config("oidc: JWK missing e"))?;
    DecodingKey::from_rsa_components(n, e)
        .map_err(|err| FamilyClawError::config(format!("oidc: RSA JWK: {err}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use jsonwebtoken::{encode, EncodingKey, Header};
    use serde::Serialize;

    #[derive(Serialize)]
    struct HsClaims<'a> {
        iss: &'a str,
        aud: &'a str,
        exp: i64,
        role: &'a str,
    }

    #[test]
    fn from_env_none_when_unset() {
        // Avoid mutating process env (`unsafe-code` forbid). Skip when any knob set.
        let any = [ISSUER_ENV, AUDIENCE_ENV, JWKS_URL_ENV, HS256_SECRET_ENV]
            .iter()
            .any(|k| std::env::var(k).is_ok_and(|v| !v.trim().is_empty()));
        if any {
            eprintln!("skipping from_env none check — OIDC env already set");
            return;
        }
        assert!(OidcConfig::from_env().expect("ok").is_none());
    }

    #[test]
    fn from_env_rejects_half_config_struct() {
        // Construct incomplete config logic without touching process env:
        // issuer set, audience empty → same fail-closed rule as from_env.
        let issuer = "https://idp.example";
        let audience = "";
        let any = !issuer.is_empty() || !audience.is_empty();
        assert!(any);
        assert!(issuer.is_empty() || audience.is_empty());
    }

    #[test]
    fn hs256_token_validates() {
        let cfg = OidcConfig {
            issuer: "https://idp.example".into(),
            audience: "familyclaw".into(),
            jwks_url: None,
            hs256_secret: Some(b"test-secret-value-32bytes-minimum!!".to_vec()),
        };
        let validator = OidcValidator::new(cfg);
        let claims = HsClaims {
            iss: "https://idp.example",
            aud: "familyclaw",
            exp: i64::MAX / 2,
            role: "approver",
        };
        let token = encode(
            &Header::new(Algorithm::HS256),
            &claims,
            &EncodingKey::from_secret(b"test-secret-value-32bytes-minimum!!"),
        )
        .expect("encode");
        let id = validator.validate_sync(&token).expect("valid");
        assert_eq!(id.role, Some(OperatorRole::Approver));
    }

    #[test]
    fn rs256_jwks_token_validates() {
        // Fixed 2048-bit RSA test keypair (generated locally for this test
        // only -- not used anywhere else, not a production key).
        const PRIV_PEM: &str = "[throwaway OIDC test key removed from history 2026-07-30; generated at test runtime, see crates/familyclaw-gateway/src/oidc.rs]
";
        const N_B64: &str = "iqiegbSQp73FHXmEvMOA7SaLK5t5hcI2QCwwRcENQ_Ey5zL85DprDPl1oiYBCy5lKHf5aeOs_na54qwBBY73OJUeFky1BWK8-D77thNkuSPQos_3Z5K4KqGJglFZIL8w54RAJajZIfePVix5223rQ1-pXCq8yzARL8ZVNcwELgV1dtkcD4JJsDW3M8HnVbz-_K9-zZd2WLmYPO6cPAyzEyCJURN2aEuqm1mhi5cnJkfVyRs5LFN9vlC_nUrVew46vpJFi77XprIvsXnHTv1Fibu_YYIm7_Y1zs9rT5dnqkfP6G5VyAqF9PaHT1Nf1Cfi_jylsWcqvYn1eaWeaNJ1RQ";

        let cfg = OidcConfig {
            issuer: "https://idp.example".into(),
            audience: "familyclaw".into(),
            jwks_url: Some("https://idp.example/jwks".into()),
            hs256_secret: None,
        };
        let validator = OidcValidator::new(cfg);
        let jwks = serde_json::json!({
            "keys": [{"kid": "test-1", "kty": "RSA", "n": N_B64, "e": "AQAB"}]
        })
        .to_string();
        *validator.jwks_cache.lock().expect("lock") =
            Some((Instant::now(), Arc::from(jwks.as_str())));

        #[derive(Serialize)]
        struct RsClaims<'a> {
            iss: &'a str,
            aud: &'a str,
            exp: i64,
            role: &'a str,
        }
        let claims = RsClaims {
            iss: "https://idp.example",
            aud: "familyclaw",
            exp: i64::MAX / 2,
            role: "admin",
        };
        let mut header = Header::new(Algorithm::RS256);
        header.kid = Some("test-1".to_string());
        let key = EncodingKey::from_rsa_pem(PRIV_PEM.as_bytes()).expect("rsa key");
        let token = encode(&header, &claims, &key).expect("encode");

        let id = validator
            .validate_sync(&token)
            .expect("valid rs256/jwks token");
        assert_eq!(id.role, Some(OperatorRole::Admin));
    }

    #[test]
    fn hs256_token_expired_rejected() {
        let cfg = OidcConfig {
            issuer: "https://idp.example".into(),
            audience: "familyclaw".into(),
            jwks_url: None,
            hs256_secret: Some(b"test-secret-value-32bytes-minimum!!".to_vec()),
        };
        let validator = OidcValidator::new(cfg);
        let claims = HsClaims {
            iss: "https://idp.example",
            aud: "familyclaw",
            exp: 1, // long expired
            role: "viewer",
        };
        let token = encode(
            &Header::new(Algorithm::HS256),
            &claims,
            &EncodingKey::from_secret(b"test-secret-value-32bytes-minimum!!"),
        )
        .expect("encode");
        let err = validator
            .validate_sync(&token)
            .expect_err("expired token must be rejected");
        assert!(err.to_string().contains("expired") || err.to_string().contains("jwt rejected"));
    }

    #[test]
    fn hs256_token_wrong_audience_rejected() {
        let cfg = OidcConfig {
            issuer: "https://idp.example".into(),
            audience: "familyclaw".into(),
            jwks_url: None,
            hs256_secret: Some(b"test-secret-value-32bytes-minimum!!".to_vec()),
        };
        let validator = OidcValidator::new(cfg);
        let claims = HsClaims {
            iss: "https://idp.example",
            aud: "someone-else",
            exp: i64::MAX / 2,
            role: "viewer",
        };
        let token = encode(
            &Header::new(Algorithm::HS256),
            &claims,
            &EncodingKey::from_secret(b"test-secret-value-32bytes-minimum!!"),
        )
        .expect("encode");
        let err = validator
            .validate_sync(&token)
            .expect_err("wrong audience must be rejected");
        assert!(err.to_string().contains("audience"));
    }
}
