use crate::{rncryptor_decrypt, DecryptStatus, SebValue};
use base64::Engine;
use p12_keystore::{Pkcs12Archive, Pkcs12ImportPolicy, KeyStore};
use pkcs8::DecodePrivateKey;
use rsa::traits::PublicKeyParts;
use rsa::{Pkcs1v15Encrypt, RsaPrivateKey};
use secret_service::blocking::SecretService;
use secret_service::EncryptionType;
use sha1::{Digest, Sha1};
use std::collections::HashMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use x509_cert::der::{pem::LineEnding, Decode, EncodePem};

/// SEB's fixed PKCS#12 identity certificate password (matches C++ `certificate_store.cpp`).
const SEB_IDENTITY_P12_PASSWORD: &str = "Di\u{1D22D}l\u{1D216}Ch\u{1D212}ah\u{1D247}t\u{1D201}a\u{1D248}Hai1972";
const IDENTITY_CERT_TYPE: i64 = 1;
const PUBKEY_HASH_LEN: usize = 20;
const SEB_IDENTITY_SCHEMA: &str = "org.safeexambrowser.seb.identity";

#[derive(Debug, Clone)]
pub struct StoredIdentity {
    pub pubkey_sha1: [u8; PUBKEY_HASH_LEN],
    pub rsa_key: RsaPrivateKey,
    pub modulus_size: usize,
}

#[derive(Debug, Default)]
pub struct CertificateStore {
    identities: HashMap<[u8; PUBKEY_HASH_LEN], StoredIdentity>,
}

fn connect_secret_service() -> Option<SecretService<'static>> {
    SecretService::connect(EncryptionType::Dh)
        .or_else(|_| SecretService::connect(EncryptionType::Plain))
        .ok()
}

impl CertificateStore {
    pub fn load() -> Self {
        let mut store = Self::default();
        store.load_persisted_identities();
        store
    }

    fn identity_dir() -> PathBuf {
        let base = std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
            .unwrap_or_else(|| PathBuf::from("/tmp"));
        base.join("Safe Exam Browser").join("identities")
    }

    fn load_persisted_identities(&mut self) {
        // 1. Load via pure-Rust Secret Service D-Bus API (GNOME Keyring / KWallet / KeePassXC)
        if let Some(ss) = connect_secret_service() {
            let attrs = HashMap::from([("xdg:schema", SEB_IDENTITY_SCHEMA)]);
            if let Ok(search) = ss.search_items(attrs) {
                for item in search.unlocked.into_iter().chain(search.locked) {
                    let _ = item.unlock();
                    if let Ok(secret) = item.get_secret() {
                        let raw_b64 = String::from_utf8_lossy(&secret);
                        if let Ok(p12) = base64::engine::general_purpose::STANDARD
                            .decode(raw_b64.trim())
                        {
                            let _ = self.import_pkcs12(&p12, false);
                        }
                    }
                }
            }
        }

        // 2. Also load from restricted 0700/0600 local fallback directory
        let dir = Self::identity_dir();
        if let Ok(rd) = fs::read_dir(&dir) {
            for entry in rd.flatten() {
                if entry.path().extension().and_then(|e| e.to_str()) == Some("p12") {
                    if let Ok(p12) = fs::read(entry.path()) {
                        let _ = self.import_pkcs12(&p12, false);
                    }
                }
            }
        }
    }

    fn persist_identity(&self, hash: &[u8; PUBKEY_HASH_LEN], p12: &[u8]) {
        let hex_hash = hex::encode(hash);
        let b64 = base64::engine::general_purpose::STANDARD.encode(p12);

        // Try persisting via pure-Rust Secret Service D-Bus API
        if let Some(ss) = connect_secret_service() {
            if let Ok(collection) = ss.get_default_collection() {
                let _ = collection.unlock();
                let label = format!("Safe Exam Browser identity {hex_hash}");
                let attrs = HashMap::from([
                    ("xdg:schema", SEB_IDENTITY_SCHEMA),
                    ("public-key-sha1", hex_hash.as_str()),
                ]);
                if collection
                    .create_item(&label, attrs, b64.as_bytes(), true, "text/plain")
                    .is_ok()
                {
                    return;
                }
            }
        }

        // Fallback: restricted 0700/0600 identity directory
        let dir = Self::identity_dir();
        if fs::create_dir_all(&dir).is_ok() {
            let _ = fs::set_permissions(&dir, fs::Permissions::from_mode(0o700));
            let path = dir.join(format!("{hex_hash}.p12"));
            if fs::write(&path, p12).is_ok() {
                let _ = fs::set_permissions(&path, fs::Permissions::from_mode(0o600));
            }
        }
    }

    /// Extracts and imports type-1 (`IdentityCertificateType`) PKCS#12 certificates from plist
    /// `embeddedCertificates`, returning the plist with imported identity entries removed.
    pub fn extract_and_import_identities(&mut self, root: &mut SebValue) {
        let SebValue::Map(entries) = root else { return };
        let Some((_, SebValue::List(certs))) = entries
            .iter_mut()
            .find(|(k, _)| k == "embeddedCertificates")
        else {
            return;
        };

        certs.retain(|item| {
            let SebValue::Map(m) = item else { return true };
            let ctype = m.iter().find(|(k, _)| k == "type").and_then(|(_, v)| match v {
                SebValue::Int(i) => Some(*i),
                _ => None,
            });
            if ctype != Some(IDENTITY_CERT_TYPE) {
                return true;
            }
            let raw_b64 = m
                .iter()
                .find(|(k, _)| k == "certificateData" || k == "certificateDataBase64")
                .and_then(|(_, v)| match v {
                    SebValue::Bytes(b) | SebValue::String(b) => Some(b.as_str()),
                    _ => None,
                });
            let Some(b64) = raw_b64 else { return true };
            let Ok(p12) = base64::engine::general_purpose::STANDARD.decode(b64.trim()) else {
                return true;
            };
            // Remove imported identity entry from plist on success
            self.import_pkcs12(&p12, true).is_err()
        });
    }

    /// Pure-Rust in-memory PKCS#12 parsing, X.509 SPKI SHA-1 hashing, and RSA key extraction.
    pub fn import_pkcs12(&mut self, p12: &[u8], persist: bool) -> Result<[u8; PUBKEY_HASH_LEN], String> {
        let (key_der, cert_der) = extract_key_and_cert_der(p12)?;
        let rsa_key = RsaPrivateKey::from_pkcs8_der(&key_der)
            .map_err(|e| format!("Failed parsing RSA PKCS#8 private key: {e}"))?;
        let modulus_size = rsa_key.size();
        if modulus_size == 0 {
            return Err("Invalid RSA private key modulus size.".to_string());
        }

        // Compute SHA-1 of SubjectPublicKeyInfo bitstring (matches OpenSSL X509_get0_pubkey_bitstr)
        let cert = x509_cert::Certificate::from_der(&cert_der)
            .map_err(|e| format!("Failed parsing X.509 certificate: {e}"))?;
        let spki_bitstring = cert
            .tbs_certificate
            .subject_public_key_info
            .subject_public_key
            .raw_bytes();
        let digest = Sha1::digest(spki_bitstring);
        let mut hash = [0u8; PUBKEY_HASH_LEN];
        hash.copy_from_slice(&digest[..PUBKEY_HASH_LEN]);

        // Keep PEM conversion helper validated for debugging/diagnostics if needed
        let _ = cert.to_pem(LineEnding::LF);

        let id = StoredIdentity {
            pubkey_sha1: hash,
            rsa_key,
            modulus_size,
        };
        if persist {
            self.persist_identity(&hash, p12);
        }
        self.identities.insert(hash, id);
        Ok(hash)
    }

    pub fn decrypt_pkhs(&self, payload: &[u8]) -> Result<Vec<u8>, DecryptStatus> {
        if payload.len() <= PUBKEY_HASH_LEN {
            return Err(DecryptStatus::InvalidData);
        }
        let mut hash = [0u8; PUBKEY_HASH_LEN];
        hash.copy_from_slice(&payload[..PUBKEY_HASH_LEN]);
        let Some(identity) = self.identities.get(&hash) else {
            return Err(DecryptStatus::PasswordNeeded);
        };
        rsa_decrypt_blocks(identity, &payload[PUBKEY_HASH_LEN..])
            .map_err(|_| DecryptStatus::InvalidData)
    }

    pub fn decrypt_phsk(&self, payload: &[u8]) -> Result<Vec<u8>, DecryptStatus> {
        if payload.len() <= PUBKEY_HASH_LEN + 4 {
            return Err(DecryptStatus::InvalidData);
        }
        let mut hash = [0u8; PUBKEY_HASH_LEN];
        hash.copy_from_slice(&payload[..PUBKEY_HASH_LEN]);
        let key_len = u32::from_le_bytes([
            payload[PUBKEY_HASH_LEN],
            payload[PUBKEY_HASH_LEN + 1],
            payload[PUBKEY_HASH_LEN + 2],
            payload[PUBKEY_HASH_LEN + 3],
        ]) as usize;
        let key_start = PUBKEY_HASH_LEN + 4;
        if key_len == 0 || key_start + key_len >= payload.len() {
            return Err(DecryptStatus::InvalidData);
        }
        let Some(identity) = self.identities.get(&hash) else {
            return Err(DecryptStatus::PasswordNeeded);
        };
        let enc_key = &payload[key_start..key_start + key_len];
        let rncryptor_payload = &payload[key_start + key_len..];
        let decrypted_key =
            rsa_decrypt_blocks(identity, enc_key).map_err(|_| DecryptStatus::InvalidData)?;
        let symmetric_pw = base64::engine::general_purpose::STANDARD.encode(&decrypted_key);
        rncryptor_decrypt(rncryptor_payload, &symmetric_pw)
    }
}

fn extract_key_and_cert_der(p12: &[u8]) -> Result<(Vec<u8>, Vec<u8>), String> {
    if let Ok(keystore) = KeyStore::from_pkcs12(
        p12,
        SEB_IDENTITY_P12_PASSWORD,
        Pkcs12ImportPolicy::Relaxed,
    ) {
        if let Some((_, chain)) = keystore.private_key_chain() {
            if let Some(leaf) = chain.certs().first() {
                return Ok((chain.key().as_der().to_vec(), leaf.as_der().to_vec()));
            }
        }
    }

    // Fallback to raw bag scan in case localKeyId is missing between key and cert bags
    let archive = Pkcs12Archive::from_pkcs12(p12, SEB_IDENTITY_P12_PASSWORD)
        .map_err(|e| format!("Failed parsing PKCS#12 archive: {e}"))?;
    let key_bag = archive
        .keys
        .first()
        .ok_or_else(|| "PKCS#12 archive contains no private key.".to_string())?;
    let cert_bag = key_bag
        .local_key_id
        .as_ref()
        .and_then(|kid| {
            archive.certs.iter().find(|c| {
                c.local_key_id
                    .as_ref()
                    .is_some_and(|cid| cid.as_slice() == kid.as_ref())
            })
        })
        .or_else(|| archive.certs.first())
        .ok_or_else(|| "PKCS#12 archive contains no certificate.".to_string())?;
    Ok((
        key_bag.key.as_der().to_vec(),
        cert_bag.cert.as_der().to_vec(),
    ))
}

fn rsa_decrypt_blocks(identity: &StoredIdentity, ciphertext: &[u8]) -> Result<Vec<u8>, String> {
    if ciphertext.is_empty() || identity.modulus_size == 0 {
        return Err("Invalid RSA ciphertext".to_string());
    }
    let mut plain = Vec::new();
    for chunk in ciphertext.chunks(identity.modulus_size) {
        let block = identity
            .rsa_key
            .decrypt(Pkcs1v15Encrypt, chunk)
            .map_err(|e| format!("RSA PKCS1 block decryption failed: {e}"))?;
        plain.extend_from_slice(&block);
    }
    Ok(plain)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsa::RsaPublicKey;

    #[test]
    fn pure_rust_rsa_pkhs_decrypt_roundtrip() {
        let mut rng = rand::thread_rng();
        let priv_key = RsaPrivateKey::new(&mut rng, 1024).expect("rsa keygen");
        let pub_key = RsaPublicKey::from(&priv_key);
        let modulus_size = priv_key.size();

        let pubkey_sha1 = [0x42u8; PUBKEY_HASH_LEN];
        let mut store = CertificateStore::default();
        store.identities.insert(
            pubkey_sha1,
            StoredIdentity {
                pubkey_sha1,
                rsa_key: priv_key,
                modulus_size,
            },
        );

        let plaintext = b"seb-pure-rust-secret-payload";
        let enc = pub_key
            .encrypt(&mut rng, Pkcs1v15Encrypt, plaintext)
            .expect("rsa encrypt");

        let mut payload = Vec::with_capacity(PUBKEY_HASH_LEN + enc.len());
        payload.extend_from_slice(&pubkey_sha1);
        payload.extend_from_slice(&enc);

        let decrypted = store.decrypt_pkhs(&payload).expect("pkhs decrypt");
        assert_eq!(decrypted, plaintext);

        let mut wrong = payload.clone();
        wrong[0] ^= 0xff;
        assert_eq!(store.decrypt_pkhs(&wrong), Err(DecryptStatus::PasswordNeeded));
    }
}
