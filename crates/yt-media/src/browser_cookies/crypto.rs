use super::{invalid, Result};
use std::path::Path;
use zeroize::Zeroizing;

#[cfg(target_os = "macos")]
pub(super) fn key(browser: &str, _path: &Path) -> Result<Zeroizing<Vec<u8>>> {
    let account = match browser {
        "chrome" => "Chrome",
        "chromium" => "Chromium",
        "edge" => "Microsoft Edge",
        "brave" => "Brave",
        "opera" => "Opera",
        "vivaldi" => "Vivaldi",
        "whale" => "Whale",
        _ => {
            return Err(invalid(
                "El navegador no define un almacén seguro compatible.",
            ))
        }
    };
    let password=Zeroizing::new(security_framework::passwords::get_generic_password(&format!("{account} Safe Storage"),account).map_err(|_|invalid("macOS no autorizó el acceso al llavero del navegador. Autorízalo en el diálogo del sistema o usa un archivo Netscape."))?);
    let mut key = Zeroizing::new(vec![0; 16]);
    pbkdf2::pbkdf2_hmac::<sha1::Sha1>(&password, b"saltysalt", 1003, &mut key);
    Ok(key)
}

#[cfg(target_os = "windows")]
pub(super) fn key(_browser: &str, path: &Path) -> Result<Zeroizing<Vec<u8>>> {
    use base64::Engine;
    let local_state = path
        .ancestors()
        .skip(1)
        .take(4)
        .map(|p| p.join("Local State"))
        .find(|p| p.is_file())
        .ok_or_else(|| invalid("No se encontró Local State junto al perfil de Chromium."))?;
    let bytes = Zeroizing::new(super::bounded_read(&local_state, 4 * 1024 * 1024)?);
    let state: serde_json::Value = serde_json::from_slice(&bytes)
        .map_err(|_| invalid("El estado del navegador no es válido."))?;
    let encoded = state
        .pointer("/os_crypt/encrypted_key")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| {
            invalid("El navegador no ofrece una clave DPAPI compatible; usa un archivo Netscape.")
        })?;
    let encrypted = Zeroizing::new(
        base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .map_err(|_| invalid("La clave del navegador no es válida."))?,
    );
    let bytes = encrypted.strip_prefix(b"DPAPI").ok_or_else(|| {
        invalid("La clave del navegador usa una protección no compatible. Usa un archivo Netscape.")
    })?;
    unprotect(bytes)
}

#[cfg(target_os = "windows")]
fn unprotect(bytes: &[u8]) -> Result<Zeroizing<Vec<u8>>> {
    use windows_sys::Win32::{
        Foundation::LocalFree,
        Security::Cryptography::{
            CryptUnprotectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB,
        },
    };
    if bytes.len() > u32::MAX as usize {
        return Err(invalid("La clave del navegador excede el límite admitido."));
    }
    let input = CRYPT_INTEGER_BLOB {
        cbData: bytes.len() as u32,
        pbData: bytes.as_ptr() as *mut u8,
    };
    let mut output = CRYPT_INTEGER_BLOB {
        cbData: 0,
        pbData: std::ptr::null_mut(),
    };
    // DPAPI is invoked as the signed-in user only; no impersonation or elevation.
    let success = unsafe {
        CryptUnprotectData(
            &input,
            std::ptr::null_mut(),
            std::ptr::null(),
            std::ptr::null(),
            std::ptr::null(),
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output,
        )
    };
    if success == 0 {
        return Err(invalid("Windows no autorizó la clave del navegador para este usuario. Usa un archivo Netscape."));
    }
    let result = if output.pbData.is_null() {
        Vec::new()
    } else {
        unsafe { std::slice::from_raw_parts(output.pbData, output.cbData as usize) }.to_vec()
    };
    if !output.pbData.is_null() {
        unsafe {
            std::ptr::write_bytes(output.pbData, 0, output.cbData as usize);
            LocalFree(output.pbData.cast());
        }
    }
    Ok(Zeroizing::new(result))
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub(super) fn key(_browser: &str, _path: &Path) -> Result<Zeroizing<Vec<u8>>> {
    Err(invalid("Este sistema no admite conexión con Chromium."))
}

pub(super) fn decrypt(
    key: &[u8],
    domain: &str,
    encrypted: &[u8],
    domain_bound: bool,
) -> Result<String> {
    #[cfg(target_os = "macos")]
    let plaintext = {
        use cbc::cipher::{block_padding::Pkcs7, BlockDecryptMut, KeyIvInit};
        if !encrypted.starts_with(b"v10") && !encrypted.starts_with(b"v11") {
            return Err(invalid(
                "La versión de cifrado del navegador no es compatible.",
            ));
        }
        Zeroizing::new(
            cbc::Decryptor::<aes::Aes128>::new_from_slices(key, &[0x20; 16])
                .map_err(|_| invalid("La clave del navegador no es válida."))?
                .decrypt_padded_vec_mut::<Pkcs7>(&encrypted[3..])
                .map_err(|_| {
                    invalid("No se pudo descifrar la cookie con el llavero autorizado.")
                })?,
        )
    };
    #[cfg(target_os = "windows")]
    let plaintext = {
        use aes_gcm::{aead::Aead, Aes256Gcm, KeyInit};
        if encrypted.starts_with(b"v10") || encrypted.starts_with(b"v11") {
            if encrypted.len() < 31 {
                return Err(invalid("La cookie cifrada está incompleta."));
            }
            Zeroizing::new(
                Aes256Gcm::new_from_slice(key)
                    .map_err(|_| invalid("La clave del navegador no es válida."))?
                    .decrypt(
                        aes_gcm::Nonce::from_slice(&encrypted[3..15]),
                        &encrypted[15..],
                    )
                    .map_err(|_| {
                        invalid("La cookie no pudo descifrarse con la clave de este usuario.")
                    })?,
            )
        } else {
            unprotect(encrypted)?
        }
    };
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let plaintext = {
        let _ = (key, encrypted);
        return Err(invalid("Este sistema no admite descifrado de Chromium."));
        #[allow(unreachable_code)]
        Zeroizing::new(Vec::<u8>::new())
    };
    decode_plaintext(domain, &plaintext, domain_bound)
}

fn decode_plaintext(domain: &str, plaintext: &[u8], domain_bound: bool) -> Result<String> {
    use sha2::{Digest, Sha256};
    let hash = Sha256::digest(domain.as_bytes());
    let prefixed = plaintext.starts_with(hash.as_slice());
    if domain_bound && !prefixed {
        return Err(invalid("La cookie no coincide con el dominio de YouTube."));
    }
    let value = if prefixed {
        &plaintext[32..]
    } else {
        plaintext
    };
    String::from_utf8(value.to_vec())
        .map_err(|_| invalid("El navegador entregó una cookie con texto no válido."))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn validates_chromium_domain_binding_without_real_keychain() {
        use sha2::{Digest, Sha256};
        let mut data = Sha256::digest(b".youtube.com").to_vec();
        data.extend_from_slice(b"fixture-value");
        assert_eq!(
            decode_plaintext(".youtube.com", &data, true).unwrap(),
            "fixture-value"
        );
        assert!(decode_plaintext(".other.test", &data, true).is_err());
        assert!(decode_plaintext(".youtube.com", b"legacy-value", true).is_err());
        assert_eq!(
            decode_plaintext(".youtube.com", b"legacy-value", false).unwrap(),
            "legacy-value"
        );
    }
    #[cfg(target_os = "macos")]
    #[test]
    fn decrypts_synthetic_mac_cookie_with_supplied_key() {
        use cbc::cipher::{block_padding::Pkcs7, BlockEncryptMut, KeyIvInit};
        let key = [7; 16];
        let mut encrypted = b"v10".to_vec();
        encrypted.extend(
            cbc::Encryptor::<aes::Aes128>::new(&key.into(), &[0x20; 16].into())
                .encrypt_padded_vec_mut::<Pkcs7>(b"fixture-value"),
        );
        assert_eq!(
            decrypt(&key, ".youtube.com", &encrypted, false).unwrap(),
            "fixture-value"
        );
        assert!(decrypt(&[8; 16], ".youtube.com", &encrypted, false).is_err());
    }
}
