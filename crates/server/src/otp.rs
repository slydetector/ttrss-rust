//! OTP (TOTP) helpers for the `Pref_Prefs` "Authenticator (OTP)" pane.
//!
//! Faithful ports of `UserHelper::is_otp_enabled` / `UserHelper::get_otp_secret`
//! and `Pref_Prefs::_get_otp_qrcode_img` / `Pref_Prefs::format_otp_secret`.

use base32::Alphabet;
use base64::Engine as _;
use sha1::Digest;
use sqlx::PgPool;

/// `ParagonIE\ConstantTime\Base32::encodeUpperUnpadded` — RFC 4648, uppercase,
/// no padding. (Delegates to the maintained `base32` crate.)
pub fn base32_upper_unpadded(data: &[u8]) -> String {
    base32::encode(Alphabet::Rfc4648 { padding: false }, data)
}

/// `Pref_Prefs::format_otp_secret` — `implode(" ", str_split($secret, 4))`.
pub fn format_otp_secret(secret: &str) -> String {
    secret
        .as_bytes()
        .chunks(4)
        .map(|c| std::str::from_utf8(c).unwrap_or(""))
        .collect::<Vec<_>>()
        .join(" ")
}

/// `Pref_Prefs::_get_otp_qrcode_img` — render an `otpauth://` URL to a base64
/// PNG data URI (the chillerlan `render()` shape: `data:image/png;base64,…`).
pub fn otp_qrcode_data_uri(otpurl: &str) -> Option<String> {
    let qr = qrcode::QrCode::new(otpurl.as_bytes()).ok()?;
    let img = qr.render::<image::Luma<u8>>().build();
    let mut buf = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut buf, img.width(), img.height());
        encoder.set_color(png::ColorType::Grayscale);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().ok()?;
        writer.write_image_data(img.as_raw()).ok()?;
    }
    Some(format!(
        "data:image/png;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(&buf)
    ))
}

/// `UserHelper::is_otp_enabled`.
pub async fn is_otp_enabled(db: &PgPool, uid: i64) -> Result<bool, sqlx::Error> {
    let row: Option<(bool,)> =
        sqlx::query_as("SELECT otp_enabled FROM ttrss_users WHERE id = $1")
            .bind(uid)
            .fetch_optional(db)
            .await?;
    Ok(row.map(|r| r.0).unwrap_or(false))
}

/// `UserHelper::get_otp_secret` (the `schema_version >= 143` path).
///
/// Returns the base32 secret, generating and persisting a new one when the
/// `otp_secret` column is empty (matching PHP's lazy migration). Returns `None`
/// when OTP is enabled and `show_if_enabled` is false.
pub async fn get_otp_secret(
    db: &PgPool,
    uid: i64,
    show_if_enabled: bool,
) -> Result<Option<String>, sqlx::Error> {
    let row: Option<(bool, Option<String>, String)> =
        sqlx::query_as("SELECT otp_enabled, otp_secret, salt FROM ttrss_users WHERE id = $1")
            .bind(uid)
            .fetch_optional(db)
            .await?;
    let Some((otp_enabled, otp_secret, salt)) = row else {
        return Ok(None);
    };
    let salt_based = format!("{:x}", sha1::Sha1::digest(salt.as_bytes()));
    let secret = if otp_secret.as_deref().map(str::is_empty).unwrap_or(true) {
        let new = if otp_enabled {
            salt_based[..12].to_string()
        } else {
            let mut bytes = [0u8; 10];
            let mut rng = rand::thread_rng();
            rand::RngCore::fill_bytes(&mut rng, &mut bytes);
            bytes.iter().map(|b| format!("{:02x}", b)).collect()
        };
        sqlx::query("UPDATE ttrss_users SET otp_secret = $1 WHERE id = $2")
            .bind(&new)
            .bind(uid)
            .execute(db)
            .await?;
        new
    } else {
        otp_secret.unwrap()
    };

    if !otp_enabled || show_if_enabled {
        Ok(Some(base32_upper_unpadded(secret.as_bytes())))
    } else {
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base32_matches_rfc4648() {
        // RFC 4648 §10 standard-alphabet test vectors (unpadded form).
        assert_eq!(base32_upper_unpadded(b""), "");
        assert_eq!(base32_upper_unpadded(b"f"), "MY");
        assert_eq!(base32_upper_unpadded(b"fo"), "MZXQ");
        assert_eq!(base32_upper_unpadded(b"foo"), "MZXW6");
        assert_eq!(base32_upper_unpadded(b"foob"), "MZXW6YQ");
        assert_eq!(base32_upper_unpadded(b"fooba"), "MZXW6YTB");
        assert_eq!(base32_upper_unpadded(b"foobar"), "MZXW6YTBOI");
    }

    #[test]
    fn format_groups_by_four() {
        assert_eq!(
            format_otp_secret("MZXW6YTBOI"),
            "MZXW 6YTB OI"
        );
        assert_eq!(
            format_otp_secret("ABCDEFGHIJKLMNOP"),
            "ABCD EFGH IJKL MNOP"
        );
    }

    #[test]
    fn qrcode_is_png_data_uri() {
        let uri = otp_qrcode_data_uri(
            "otpauth://totp/admin?secret=MZXW6YTBOI&issuer=Tiny+Tiny+RSS",
        )
        .expect("qr");
        assert!(uri.starts_with("data:image/png;base64,"), "uri: {uri}");
        let b64 = uri.strip_prefix("data:image/png;base64,").unwrap();
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(b64)
            .expect("valid base64");
        // PNG signature.
        assert_eq!(&bytes[0..8], &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]);
    }
}
