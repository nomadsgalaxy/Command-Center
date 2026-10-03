//! cc-host cert | check, which cc-share used to do with Python. `cert` makes krdp's TLS certificate
//! (cert.pem, key.pem). It's a fixed one so the Frame can pin it, and it's kept once made. `check`
//! makes sure the agent answers on 3399 with its TLS pinned to this host's own key (cc-share check's
//! "agent answering").
use crate::agent::write_private;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{DigitallySignedStruct, SignatureScheme};
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

/// Turns days since 1970-01-01 into (year, month, day), using Howard Hinnant's civil_from_days.
fn civil(days: i64) -> (i32, u8, u8) {
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u8;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u8;
    ((yoe + era * 400 + (m <= 2) as i64) as i32, m, d)
}

/// Makes cert.pem and key.pem for krdp once: ECDSA P-256, self-signed, with the host's name as the CN,
/// valid from yesterday for ten years (like cc-share's Python made its RSA one). If they're there, they're kept.
pub fn cert(conf: &Path, host: &str) -> Result<&'static str, String> {
    if std::fs::metadata(conf.join("cert.pem")).is_ok_and(|m| m.len() > 0) {
        return Ok("kept");
    }
    let kp = rcgen::KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256).map_err(|e| e.to_string())?;
    let mut p = rcgen::CertificateParams::new(Vec::<String>::new()).map_err(|e| e.to_string())?;
    p.distinguished_name.push(rcgen::DnType::CommonName, host);
    let today = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).unwrap_or_default().as_secs() as i64 / 86400;
    let (y0, m0, d0) = civil(today - 1);
    let (y1, m1, d1) = civil(today + 3650);
    p.not_before = rcgen::date_time_ymd(y0, m0, d0);
    p.not_after = rcgen::date_time_ymd(y1, m1, d1);
    let c = p.self_signed(&kp).map_err(|e| e.to_string())?;
    write_private(&conf.join("key.pem"), kp.serialize_pem().as_bytes()).map_err(|e| format!("key.pem: {e}"))?;
    write_private(&conf.join("cert.pem"), c.pem().as_bytes()).map_err(|e| format!("cert.pem: {e}"))?;
    Ok("made")
}

#[derive(Debug)]
struct Pin([u8; 32], Arc<rustls::crypto::CryptoProvider>);

impl ServerCertVerifier for Pin {
    fn verify_server_cert(&self, cert: &CertificateDer, _: &[CertificateDer], _: &ServerName, _: &[u8], _: UnixTime) -> Result<ServerCertVerified, rustls::Error> {
        match cc_proto::agent::cert_ed25519_key(cert.as_ref()) {
            Some(k) if k == self.0 => Ok(ServerCertVerified::assertion()),
            _ => Err(rustls::Error::General("not this host's key".into())),
        }
    }
    fn verify_tls12_signature(&self, _: &[u8], _: &CertificateDer, _: &DigitallySignedStruct) -> Result<HandshakeSignatureValid, rustls::Error> {
        Err(rustls::Error::General("TLS 1.3 only".into()))
    }
    fn verify_tls13_signature(&self, m: &[u8], c: &CertificateDer, d: &DigitallySignedStruct) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(m, c, d, &self.1.signature_verification_algorithms)
    }
    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.1.signature_verification_algorithms.supported_schemes()
    }
}

/// Checks that the agent on `port` answers TLS with this host's key and sends its challenge.
pub fn check(conf: &Path, port: u16) -> Result<(), String> {
    use ed25519_dalek::pkcs8::DecodePrivateKey;
    // Read this host's key as it is, since a check shouldn't make anything.
    let pem = std::fs::read_to_string(conf.join("host-key")).map_err(|_| "no host key yet (the agent makes it)".to_string())?;
    let pk = ed25519_dalek::SigningKey::from_pkcs8_pem(&pem).map_err(|e| format!("host-key: {e}"))?.verifying_key().to_bytes();
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let cfg = rustls::ClientConfig::builder_with_provider(provider.clone())
        .with_protocol_versions(&[&rustls::version::TLS13]).map_err(|e| e.to_string())?
        .dangerous().with_custom_certificate_verifier(Arc::new(Pin(pk, provider)))
        .with_no_client_auth();
    let conn = rustls::ClientConnection::new(Arc::new(cfg), ServerName::try_from("cc-host").unwrap()).map_err(|e| e.to_string())?;
    let sock = std::net::TcpStream::connect_timeout(&([127, 0, 0, 1], port).into(), Duration::from_secs(3)).map_err(|e| format!("port {port}: {e}"))?;
    sock.set_read_timeout(Some(Duration::from_secs(3))).ok();
    let mut tls = rustls::StreamOwned::new(conn, sock);
    tls.flush().map_err(|e| e.to_string())?;
    let mut line = String::new();
    BufReader::new(tls).read_line(&mut line).map_err(|e| format!("TLS: {e}"))?;
    if line.contains("challenge") { Ok(()) } else { Err(format!("no challenge: {}", line.trim())) }
}

#[cfg(test)]
mod tests {
    #[test]
    fn dates() {
        assert_eq!(super::civil(0), (1970, 1, 1));
        assert_eq!(super::civil(20729), (2026, 10, 3));
        assert_eq!(super::civil(11016), (2000, 2, 29));
    }
}
