use std::{
    fs::File,
    io::{self, BufReader},
    net::IpAddr,
    path::Path,
    sync::Arc,
};

use rustls::{
    ClientConfig, DigitallySignedStruct, RootCertStore, ServerConfig, SignatureScheme,
    client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier},
    client::verify_server_name,
    crypto::CryptoProvider,
    pki_types::{CertificateDer, PrivateKeyDer, ServerName, UnixTime},
    server::ParsedCertificate,
};
use sha2::{Digest, Sha256};
use thiserror::Error;
use tokio::net::TcpStream;
use tokio_rustls::{
    TlsConnector, client::TlsStream as ClientTlsStream, rustls,
    server::TlsStream as ServerTlsStream,
};

/// TLS 配置和证书读取错误。
#[derive(Debug, Error)]
pub enum TlsError {
    /// 文件或 PEM 读取失败。
    #[error("TLS 证书文件读取失败：{0}")]
    Io(#[from] io::Error),
    /// PEM 内容无效。
    #[error("TLS PEM 内容无效：{0}")]
    Pem(String),
    /// Rustls 配置无效。
    #[error("TLS 配置无效：{0}")]
    Config(String),
    /// 服务端名称无效。
    #[error("TLS 服务端名称无效：{0}")]
    ServerName(String),
    /// TLS 握手失败。
    #[error("TLS 握手失败：{0}")]
    Handshake(String),
    /// 服务端没有返回证书。
    #[error("Relay TLS 握手没有返回服务端证书")]
    MissingPeerCertificate,
    /// 证书指纹格式无效。
    #[error("TLS 证书指纹无效：{0}")]
    InvalidFingerprint(String),
    /// 服务端证书与固定指纹不一致。
    #[error("Relay 证书指纹已变化；期望 {expected}，实际 {actual}")]
    FingerprintMismatch {
        /// 配置中固定的指纹。
        expected: String,
        /// 当前服务端返回的指纹。
        actual: String,
    },
}

/// 由服务端证书和私钥组成的配置。
#[derive(Clone)]
pub struct TlsServerConfig {
    /// Rustls 服务端配置。
    pub inner: Arc<ServerConfig>,
}

/// 在不发送 `RemoteOps` 应用数据的 TLS 探测中获取的服务端证书信息。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TlsCertificateProbe {
    /// 服务端叶证书的 SHA-256 指纹。
    pub sha256_fingerprint: String,
}

/// 仅用于获取服务端证书的验证器；探测连接不会发送任何 `RemoteOps` 凭据或消息。
#[derive(Debug)]
struct CertificateProbeVerifier(Arc<CryptoProvider>);

impl CertificateProbeVerifier {
    fn new() -> Arc<Self> {
        let provider = CryptoProvider::get_default()
            .cloned()
            .unwrap_or_else(|| Arc::new(rustls::crypto::ring::default_provider()));
        Arc::new(Self(provider))
    }
}

/// 只信任本地用户确认的叶证书，同时保留名称、有效期和握手签名校验。
#[derive(Debug)]
struct PinnedCertificateVerifier {
    /// 规范化后的预期 SHA-256 指纹。
    expected_fingerprint: String,
    /// TLS 握手签名验证算法。
    provider: Arc<CryptoProvider>,
}

impl PinnedCertificateVerifier {
    fn new(expected_fingerprint: String) -> Arc<Self> {
        let provider = CryptoProvider::get_default()
            .cloned()
            .unwrap_or_else(|| Arc::new(rustls::crypto::ring::default_provider()));
        Arc::new(Self {
            expected_fingerprint,
            provider,
        })
    }
}

impl ServerCertVerifier for PinnedCertificateVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        let actual_fingerprint = certificate_fingerprint(end_entity);
        if actual_fingerprint != self.expected_fingerprint {
            return Err(rustls::Error::InvalidCertificate(
                rustls::CertificateError::ApplicationVerificationFailure,
            ));
        }

        let certificate = ParsedCertificate::try_from(end_entity)?;
        verify_server_name(&certificate, server_name)?;
        let (_, parsed) =
            x509_parser::parse_x509_certificate(end_entity.as_ref()).map_err(|_| {
                rustls::Error::InvalidCertificate(rustls::CertificateError::BadEncoding)
            })?;
        let now = i64::try_from(now.as_secs()).unwrap_or(i64::MAX);
        if now < parsed.validity().not_before.timestamp() {
            return Err(rustls::Error::InvalidCertificate(
                rustls::CertificateError::NotValidYet,
            ));
        }
        if now > parsed.validity().not_after.timestamp() {
            return Err(rustls::Error::InvalidCertificate(
                rustls::CertificateError::Expired,
            ));
        }

        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        certificate: &CertificateDer<'_>,
        signature: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            certificate,
            signature,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        certificate: &CertificateDer<'_>,
        signature: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            certificate,
            signature,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

impl ServerCertVerifier for CertificateProbeVerifier {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        certificate: &CertificateDer<'_>,
        signature: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            certificate,
            signature,
            &self.0.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        certificate: &CertificateDer<'_>,
        signature: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            certificate,
            signature,
            &self.0.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}

/// 读取 PEM 证书链。
///
/// # Errors
///
/// 当证书文件无法读取、PEM 无效或证书链为空时返回错误。
pub fn load_certificates(path: impl AsRef<Path>) -> Result<Vec<CertificateDer<'static>>, TlsError> {
    let file = File::open(path)?;
    let mut reader = BufReader::new(file);
    let certs: Vec<_> = rustls_pemfile::certs(&mut reader)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| TlsError::Pem(error.to_string()))?;
    if certs.is_empty() {
        return Err(TlsError::Pem("证书链为空".to_owned()));
    }
    Ok(certs)
}

fn load_private_key(path: impl AsRef<Path>) -> Result<PrivateKeyDer<'static>, TlsError> {
    let file = File::open(path)?;
    let mut reader = BufReader::new(file);
    rustls_pemfile::private_key(&mut reader)
        .map_err(|error| TlsError::Pem(error.to_string()))?
        .ok_or_else(|| TlsError::Pem("未找到私钥".to_owned()))
}

/// 从 PEM 文件加载服务端 TLS 配置。
///
/// # Errors
///
/// 当证书、私钥无法读取或 Rustls 无法创建服务端配置时返回错误。
pub fn load_server_config(
    certificate_path: impl AsRef<Path>,
    private_key_path: impl AsRef<Path>,
) -> Result<TlsServerConfig, TlsError> {
    let certificates = load_certificates(certificate_path)?;
    let private_key = load_private_key(private_key_path)?;
    let config = ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certificates, private_key)
        .map_err(|error| TlsError::Config(error.to_string()))?;
    Ok(TlsServerConfig {
        inner: Arc::new(config),
    })
}

/// 从自签名服务端证书加载客户端信任配置。
///
/// # Errors
///
/// 当证书无法读取或不能加入信任根存储时返回错误。
pub fn load_client_config(
    certificate_path: impl AsRef<Path>,
) -> Result<Arc<ClientConfig>, TlsError> {
    let mut roots = RootCertStore::empty();
    for certificate in load_certificates(certificate_path)? {
        roots
            .add(certificate)
            .map_err(|error| TlsError::Config(error.to_string()))?;
    }
    let config = ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    Ok(Arc::new(config))
}

/// 从操作系统证书存储加载客户端信任配置。
///
/// # Errors
///
/// 当系统证书存储为空或没有可解析的可信根证书时返回错误。
pub fn load_native_client_config() -> Result<Arc<ClientConfig>, TlsError> {
    let native_certificates = rustls_native_certs::load_native_certs();
    let mut roots = RootCertStore::empty();
    let (added, _) = roots.add_parsable_certificates(native_certificates.certs);
    if added == 0 {
        let details = native_certificates
            .errors
            .first()
            .map_or_else(|| "系统证书存储为空".to_owned(), ToString::to_string);
        return Err(TlsError::Config(format!(
            "无法加载系统可信根证书：{details}"
        )));
    }
    let config = ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    Ok(Arc::new(config))
}

/// 规范化 SHA-256 证书指纹。
///
/// 接受带或不带 `SHA256:` 前缀、冒号或空格分隔的十六进制文本。
///
/// # Errors
///
/// 当输入不是 32 字节 SHA-256 十六进制值时返回错误。
pub fn normalize_certificate_fingerprint(value: &str) -> Result<String, TlsError> {
    let value = value.trim();
    let value = value
        .strip_prefix("SHA256:")
        .or_else(|| value.strip_prefix("sha256:"))
        .unwrap_or(value);
    let compact: String = value
        .chars()
        .filter(|character| !matches!(character, ':' | '-' | ' ' | '\t'))
        .collect();
    if compact.len() != 64
        || !compact
            .chars()
            .all(|character| character.is_ascii_hexdigit())
    {
        return Err(TlsError::InvalidFingerprint(
            "必须是 64 位 SHA-256 十六进制值".to_owned(),
        ));
    }
    let uppercase = compact.to_ascii_uppercase();
    let mut normalized = String::with_capacity(95);
    for (index, chunk) in uppercase.as_bytes().as_chunks::<2>().0.iter().enumerate() {
        if index > 0 {
            normalized.push(':');
        }
        normalized.push(char::from(chunk[0]));
        normalized.push(char::from(chunk[1]));
    }
    Ok(normalized)
}

fn certificate_fingerprint(certificate: &CertificateDer<'_>) -> String {
    let digest = Sha256::digest(certificate.as_ref());
    digest
        .iter()
        .map(|byte| format!("{byte:02X}"))
        .collect::<Vec<_>>()
        .join(":")
}

/// 建立不发送 `RemoteOps` 应用数据的 TLS 探测连接并读取服务端叶证书。
///
/// 此函数只用于向本地用户展示证书指纹；返回结果本身不代表证书可信。
///
/// # Errors
///
/// 当 TCP/TLS 连接失败或服务端没有返回证书时返回错误。
pub async fn probe_server_certificate(
    address: &str,
    server_name: &str,
) -> Result<TlsCertificateProbe, TlsError> {
    let config = Arc::new(
        ClientConfig::builder()
            .dangerous()
            .with_custom_certificate_verifier(CertificateProbeVerifier::new())
            .with_no_client_auth(),
    );
    let stream = connect_tls(address, server_name, config).await?;
    let certificate = stream
        .get_ref()
        .1
        .peer_certificates()
        .and_then(|certificates| certificates.first())
        .ok_or(TlsError::MissingPeerCertificate)?;
    Ok(TlsCertificateProbe {
        sha256_fingerprint: certificate_fingerprint(certificate),
    })
}

/// 探测服务端证书并核对固定指纹，随后创建只信任该叶证书的客户端配置。
///
/// 指纹核对发生在发送任何 `RemoteOps` 应用数据之前。实际业务连接仍会验证证书
/// 有效期和 `server_name`。
///
/// # Errors
///
/// 当探测失败、指纹无效、指纹变化或证书无法作为信任根时返回错误。
pub async fn load_pinned_client_config(
    address: &str,
    server_name: &str,
    expected_fingerprint: &str,
) -> Result<Arc<ClientConfig>, TlsError> {
    let expected = normalize_certificate_fingerprint(expected_fingerprint)?;
    let probe = probe_server_certificate(address, server_name).await?;
    if probe.sha256_fingerprint != expected {
        return Err(TlsError::FingerprintMismatch {
            expected,
            actual: probe.sha256_fingerprint,
        });
    }
    let verifier = PinnedCertificateVerifier::new(expected);
    Ok(Arc::new(
        ClientConfig::builder()
            .dangerous()
            .with_custom_certificate_verifier(verifier)
            .with_no_client_auth(),
    ))
}

/// 连接 Relay 并完成 TLS 握手。
///
/// # Errors
///
/// 当 TCP 连接、服务端名称解析或 TLS 握手失败时返回错误。
pub async fn connect_tls(
    address: &str,
    server_name: &str,
    client_config: Arc<ClientConfig>,
) -> Result<ClientTlsStream<TcpStream>, TlsError> {
    let stream = TcpStream::connect(address).await?;
    let name = if let Ok(ip) = server_name.parse::<IpAddr>() {
        ServerName::IpAddress(ip.into())
    } else {
        ServerName::try_from(server_name.to_owned())
            .map_err(|error| TlsError::ServerName(error.to_string()))?
    };
    TlsConnector::from(client_config)
        .connect(name, stream)
        .await
        .map_err(|error| TlsError::Handshake(error.to_string()))
}

/// 使用服务端配置完成一次 TLS 握手。
///
/// # Errors
///
/// 当 TLS 握手失败时返回错误。
pub async fn accept_tls(
    stream: TcpStream,
    config: &TlsServerConfig,
) -> Result<ServerTlsStream<TcpStream>, TlsError> {
    tokio_rustls::TlsAcceptor::from(config.inner.clone())
        .accept(stream)
        .await
        .map_err(|error| TlsError::Handshake(error.to_string()))
}

#[cfg(test)]
mod tests {
    use rcgen::generate_simple_self_signed;
    use rustls::pki_types::PrivatePkcs8KeyDer;
    use tokio::net::TcpListener;

    use super::*;

    async fn serve_one_tls_connection(
        certificate: CertificateDer<'static>,
        private_key: PrivateKeyDer<'static>,
    ) -> (String, tokio::task::JoinHandle<Result<(), TlsError>>) {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("应能绑定测试端口");
        let address = listener.local_addr().expect("应能读取测试地址");
        let config = ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(vec![certificate], private_key)
            .expect("测试证书应有效");
        let server = TlsServerConfig {
            inner: Arc::new(config),
        };
        let task = tokio::spawn(async move {
            let (stream, _) = listener.accept().await?;
            accept_tls(stream, &server).await?;
            Ok(())
        });
        (address.to_string(), task)
    }

    fn self_signed_test_certificate() -> (CertificateDer<'static>, PrivateKeyDer<'static>, String) {
        let generated = generate_simple_self_signed(vec!["relay.example".to_owned()])
            .expect("应能生成测试证书");
        let certificate = generated.cert.der().clone();
        let fingerprint = certificate_fingerprint(&certificate);
        let private_key = PrivatePkcs8KeyDer::from(generated.signing_key.serialize_der()).into();
        (certificate, private_key, fingerprint)
    }

    #[test]
    fn normalizes_certificate_fingerprint() {
        let compact = "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff";
        let expected = "00:11:22:33:44:55:66:77:88:99:AA:BB:CC:DD:EE:FF:00:11:22:33:44:55:66:77:88:99:AA:BB:CC:DD:EE:FF";
        assert_eq!(
            normalize_certificate_fingerprint(compact).expect("应规范化指纹"),
            expected
        );
        assert_eq!(
            normalize_certificate_fingerprint(&format!("SHA256:{expected}")).expect("应接受前缀"),
            expected
        );
    }

    #[test]
    fn rejects_invalid_certificate_fingerprint() {
        assert!(normalize_certificate_fingerprint("not-a-fingerprint").is_err());
        assert!(normalize_certificate_fingerprint("AA:BB").is_err());
    }

    #[test]
    fn fingerprints_certificate_bytes() {
        let certificate = CertificateDer::from(vec![1, 2, 3]);
        assert_eq!(
            certificate_fingerprint(&certificate),
            "03:90:58:C6:F2:C0:CB:49:2C:53:3B:0A:4D:14:EF:77:CC:0F:78:AB:CC:CE:D5:28:7D:84:A1:A2:01:1C:FB:81"
        );
    }

    #[tokio::test]
    async fn pinned_self_signed_certificate_completes_tls_handshake_without_ca_root() {
        let (certificate, private_key, fingerprint) = self_signed_test_certificate();
        let (address, server) = serve_one_tls_connection(certificate, private_key).await;
        let config = Arc::new(
            ClientConfig::builder()
                .dangerous()
                .with_custom_certificate_verifier(PinnedCertificateVerifier::new(fingerprint))
                .with_no_client_auth(),
        );

        connect_tls(&address, "relay.example", config)
            .await
            .expect("正确的指纹和域名应完成握手");
        server
            .await
            .expect("服务端任务不应崩溃")
            .expect("服务端握手应成功");
    }

    #[tokio::test]
    async fn pinned_certificate_rejects_fingerprint_mismatch() {
        let (certificate, private_key, _) = self_signed_test_certificate();
        let (address, server) = serve_one_tls_connection(certificate, private_key).await;
        let wrong_fingerprint = normalize_certificate_fingerprint(
            "0000000000000000000000000000000000000000000000000000000000000000",
        )
        .expect("测试指纹格式应有效");
        let config = Arc::new(
            ClientConfig::builder()
                .dangerous()
                .with_custom_certificate_verifier(PinnedCertificateVerifier::new(wrong_fingerprint))
                .with_no_client_auth(),
        );

        let error = connect_tls(&address, "relay.example", config)
            .await
            .expect_err("错误指纹必须拒绝握手");
        assert!(error.to_string().contains("certificate"));
        assert!(server.await.expect("服务端任务不应崩溃").is_err());
    }
}
