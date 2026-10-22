mod ado_net;
mod jdbc;

use std::path::PathBuf;
use std::time::Duration;

use super::AuthMethod;
use crate::EncryptionLevel;
use ado_net::*;
use jdbc::*;

/// SqlClient's `Connect Timeout` default.
const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Clone, Debug)]
/// The `Config` struct contains all configuration information
/// required for connecting to the database with a [`Client`]. It also provides
/// the server address when connecting to a `TcpStream` via the
/// [`get_addr`] method.
///
/// When using an [ADO.NET connection string], it can be
/// constructed using the [`from_ado_string`] function.
///
/// Alternatively, a [`ConfigBuilder`] can be used for an ergonomic,
/// chainable construction. Create one via [`builder`], call its
/// setter methods and finalize it with [`build`].
///
/// [`Client`]: struct.Client.html
/// [ADO.NET connection string]: https://docs.microsoft.com/en-us/dotnet/framework/data/adonet/connection-strings
/// [`from_ado_string`]: struct.Config.html#method.from_ado_string
/// [`get_addr`]: struct.Config.html#method.get_addr
/// [`ConfigBuilder`]: struct.ConfigBuilder.html
/// [`builder`]: struct.Config.html#method.builder
/// [`build`]: struct.ConfigBuilder.html#method.build
pub struct Config {
    pub(crate) host: Option<String>,
    pub(crate) port: Option<u16>,
    pub(crate) database: Option<String>,
    pub(crate) instance_name: Option<String>,
    pub(crate) application_name: Option<String>,
    pub(crate) encryption: EncryptionLevel,
    pub(crate) trust: TrustConfig,
    pub(crate) auth: AuthMethod,
    pub(crate) readonly: bool,
    pub(crate) packet_size: Option<u32>,
    pub(crate) hostname_in_certificate: Option<String>,
    pub(crate) client_name: Option<String>,
    pub(crate) multi_subnet_failover: bool,
    pub(crate) connect_timeout: Option<Duration>,
    pub(crate) server_spn: Option<String>,
    #[cfg(any(
        feature = "rustls",
        feature = "native-tls",
        feature = "vendored-openssl"
    ))]
    pub(crate) client_cert: Option<ClientCertificate>,
}

#[derive(Clone, Debug)]
pub(crate) enum TrustConfig {
    #[allow(dead_code)]
    CaCertificateLocation(PathBuf),
    TrustAll,
    Default,
}

/// A client certificate and its private key, presented to the server during the
/// TLS handshake to authenticate the *client* (mutual TLS / TDS 8.0
/// `ENCRYPT_CLIENT_CERT`).
///
/// Construct one indirectly via [`Config::client_certificate`] (PEM/DER
/// certificate + private-key files) or [`Config::client_certificate_pkcs12`]
/// (a PKCS#12 / PFX bundle, `native-tls` and `vendored-openssl` only).
#[cfg(any(
    feature = "rustls",
    feature = "native-tls",
    feature = "vendored-openssl"
))]
#[derive(Clone, Debug)]
pub(crate) struct ClientCertificate {
    pub(crate) source: ClientCertSource,
}

#[cfg(any(
    feature = "rustls",
    feature = "native-tls",
    feature = "vendored-openssl"
))]
#[derive(Clone)]
pub(crate) enum ClientCertSource {
    /// A certificate file and a separate private-key file. Both may be PEM
    /// (`.pem`/`.crt` for the certificate, `.pem`/`.key` for the key) or DER
    /// (`.der`); the concrete format is detected from the file extension by the
    /// active TLS backend.
    CertAndKey { cert: PathBuf, key: PathBuf },
    /// A PKCS#12 / PFX bundle path together with its decryption password. Only
    /// supported by the `native-tls` and `vendored-openssl` backends.
    #[cfg(any(feature = "native-tls", feature = "vendored-openssl"))]
    Pkcs12 {
        path: PathBuf,
        password: zeroize::Zeroizing<String>,
    },
}

// Manual `Debug` so the PKCS#12 password is never printed.
#[cfg(any(
    feature = "rustls",
    feature = "native-tls",
    feature = "vendored-openssl"
))]
impl std::fmt::Debug for ClientCertSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ClientCertSource::CertAndKey { cert, key } => f
                .debug_struct("CertAndKey")
                .field("cert", cert)
                .field("key", key)
                .finish(),
            #[cfg(any(feature = "native-tls", feature = "vendored-openssl"))]
            ClientCertSource::Pkcs12 { path, .. } => f
                .debug_struct("Pkcs12")
                .field("path", path)
                .field("password", &"<redacted>")
                .finish(),
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            host: None,
            port: None,
            database: None,
            instance_name: None,
            application_name: None,
            #[cfg(any(
                feature = "rustls",
                feature = "native-tls",
                feature = "vendored-openssl"
            ))]
            encryption: EncryptionLevel::Required,
            #[cfg(not(any(
                feature = "rustls",
                feature = "native-tls",
                feature = "vendored-openssl"
            )))]
            encryption: EncryptionLevel::NotSupported,
            trust: TrustConfig::Default,
            auth: AuthMethod::None,
            readonly: false,
            packet_size: None,
            hostname_in_certificate: None,
            client_name: None,
            multi_subnet_failover: false,
            connect_timeout: Some(DEFAULT_CONNECT_TIMEOUT),
            server_spn: None,
            #[cfg(any(
                feature = "rustls",
                feature = "native-tls",
                feature = "vendored-openssl"
            ))]
            client_cert: None,
        }
    }
}

// `AuthMethod::AADToken` holds a plain `String` (public API); zeroize it
// whenever a `Config` goes, including a connect future dropped by a timeout.
impl Drop for Config {
    fn drop(&mut self) {
        if let AuthMethod::AADToken(token) = &mut self.auth {
            zeroize::Zeroize::zeroize(token);
        }
    }
}

impl Config {
    /// Create a new `Config` with the default settings.
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a new [`ConfigBuilder`] initialized with the default settings.
    ///
    /// This provides an ergonomic, chainable alternative to constructing a
    /// [`Config`] via its individual setter methods.
    ///
    /// # Example
    ///
    /// ```
    /// # use tiberius::{Config, AuthMethod};
    /// let config = Config::builder()
    ///     .host("localhost")
    ///     .port(1433)
    ///     .database("master")
    ///     .authentication(AuthMethod::sql_server("SA", "<password>"))
    ///     .build();
    ///
    /// assert_eq!("localhost:1433", config.get_addr());
    /// ```
    ///
    /// [`ConfigBuilder`]: struct.ConfigBuilder.html
    /// [`Config`]: struct.Config.html
    pub fn builder() -> ConfigBuilder {
        ConfigBuilder {
            inner: Self::default(),
        }
    }

    /// A host or ip address to connect to.
    ///
    /// - Defaults to `localhost`.
    pub fn host(&mut self, host: impl ToString) {
        self.host = Some(host.to_string());
    }

    /// The server port.
    ///
    /// - Defaults to `1433`.
    pub fn port(&mut self, port: u16) {
        self.port = Some(port);
    }

    /// The database to connect to.
    ///
    /// - Defaults to `master`.
    pub fn database(&mut self, database: impl ToString) {
        self.database = Some(database.to_string())
    }

    /// The instance name as defined in the SQL Browser. Only available on
    /// Windows platforms.
    ///
    /// If specified, the port is replaced with the value returned from the
    /// browser.
    ///
    /// - Defaults to no name specified.
    pub fn instance_name(&mut self, name: impl ToString) {
        self.instance_name = Some(name.to_string());
    }

    /// Sets the application name to the connection, queryable with the
    /// `APP_NAME()` command.
    ///
    /// - Defaults to no name specified.
    pub fn application_name(&mut self, name: impl ToString) {
        self.application_name = Some(name.to_string());
    }

    /// Sets the TDS packet size for the connection.
    ///
    /// Larger packet sizes can improve bulk insert performance by reducing
    /// the number of network round-trips. Valid values are 512 to 32767.
    /// The server may negotiate a different size.
    ///
    /// - Defaults to 4096 bytes.
    pub fn packet_size(&mut self, size: u32) {
        self.packet_size = Some(size);
    }

    /// Gets the configured packet size, if set.
    pub fn get_packet_size(&self) -> Option<u32> {
        self.packet_size
    }

    /// Set the preferred encryption level.
    ///
    /// - With `tls` feature, defaults to `Required`.
    /// - Without `tls` feature, defaults to `NotSupported`.
    pub fn encryption(&mut self, encryption: EncryptionLevel) {
        self.encryption = encryption;
    }

    /// If set, the server certificate will not be validated and it is accepted
    /// as-is.
    ///
    /// On production setting, the certificate should be added to the local key
    /// storage (or use `trust_cert_ca` instead), using this setting is potentially dangerous.
    ///
    /// # Panics
    /// Will panic in case `trust_cert_ca` was called before.
    ///
    /// - Defaults to `default`, meaning server certificate is validated against system-truststore.
    pub fn trust_cert(&mut self) {
        if let TrustConfig::CaCertificateLocation(_) = &self.trust {
            panic!("'trust_cert' and 'trust_cert_ca' are mutual exclusive! Only use one.")
        }
        self.trust = TrustConfig::TrustAll;
    }

    /// If set, the server certificate will be validated against the given CA certificate in
    /// in addition to the system-truststore.
    /// Useful when using self-signed certificates on the server without having to disable the
    /// trust-chain.
    ///
    /// # Panics
    /// Will panic in case `trust_cert` was called before.
    ///
    /// - Defaults to validating the server certificate is validated against system's certificate storage.
    pub fn trust_cert_ca(&mut self, path: impl ToString) {
        if let TrustConfig::TrustAll = &self.trust {
            panic!("'trust_cert' and 'trust_cert_ca' are mutual exclusive! Only use one.")
        } else {
            self.trust = TrustConfig::CaCertificateLocation(PathBuf::from(path.to_string()))
        }
    }

    /// Sets the hostname that the server certificate is validated against,
    /// instead of the value given to [`host`].
    ///
    /// This is useful when connecting through an IP address, a tunnel, or a
    /// load balancer whose certificate carries a different subject/SAN than the
    /// address used to reach it (see issue #340).
    ///
    /// - Defaults to the value of [`host`].
    ///
    /// [`host`]: Config::host
    pub fn hostname_in_certificate(&mut self, hostname: impl ToString) {
        self.hostname_in_certificate = Some(hostname.to_string());
    }

    /// Sets the client / workstation name reported to the server in the login
    /// record (queryable with `HOST_NAME()`).
    ///
    /// - Defaults to the local workstation id (the machine hostname).
    pub fn client_name(&mut self, name: impl ToString) {
        self.client_name = Some(name.to_string());
    }

    /// Sets the authentication method.
    ///
    /// - Defaults to `None`.
    pub fn authentication(&mut self, auth: AuthMethod) {
        self.auth = auth;
    }

    /// Sets ApplicationIntent readonly.
    ///
    /// - Defaults to `false`.
    pub fn readonly(&mut self, readnoly: bool) {
        self.readonly = readnoly;
    }

    /// Enable multi-subnet failover.
    ///
    /// When enabled and the server host name resolves to more than one IP
    /// address (for example, an Always On availability group listener spread
    /// across subnets), connections are attempted to all resolved addresses in
    /// parallel and the first one to succeed is used. This mirrors the ADO.NET
    /// `MultiSubnetFailover` connection-string keyword.
    ///
    /// - Defaults to `false`.
    pub fn multi_subnet_failover(&mut self, multi_subnet_failover: bool) {
        self.multi_subnet_failover = multi_subnet_failover;
    }

    /// Returns whether multi-subnet failover is enabled.
    pub fn get_multi_subnet_failover(&self) -> bool {
        self.multi_subnet_failover
    }

    /// How long establishing a connection may take: TCP connect, the SQL
    /// Browser lookup, TLS and login. `None` means no limit.
    ///
    /// The crate is runtime-agnostic and owns no timer, so it doesn't enforce
    /// this itself: wrap the TCP connect and [`Client::connect`] in your
    /// runtime's timeout.
    ///
    /// ```no_run
    /// # use tiberius::{Client, Config};
    /// # use tokio_util::compat::TokioAsyncWriteCompatExt;
    /// # #[tokio::main]
    /// # async fn main() -> anyhow::Result<()> {
    /// let config = Config::from_ado_string("server=tcp:db,1433;Connect Timeout=10")?;
    /// let connect = async {
    ///     let tcp = tokio::net::TcpStream::connect(config.get_addr()).await?;
    ///     tcp.set_nodelay(true)?;
    ///     Ok::<_, anyhow::Error>(Client::connect(config.clone(), tcp.compat_write()).await?)
    /// };
    /// let client = match config.connect_timeout() {
    ///     Some(limit) => tokio::time::timeout(limit, connect).await??,
    ///     None => connect.await?,
    /// };
    /// # Ok(()) }
    /// ```
    ///
    /// - Defaults to 15 seconds, as in SqlClient. Set from a connection
    ///   string's `Connect Timeout` (ADO.NET, 0 = no limit) or `loginTimeout`
    ///   (JDBC, 0 = the 30-second JDBC default).
    ///
    /// [`Client::connect`]: crate::Client::connect
    pub fn connect_timeout(&self) -> Option<Duration> {
        self.connect_timeout
    }

    /// Sets the value returned by [`connect_timeout`](Config::connect_timeout).
    pub fn set_connect_timeout(&mut self, timeout: Option<Duration>) {
        self.connect_timeout = timeout;
    }

    /// The service principal name integrated authentication asks a ticket
    /// for, as given, like SqlClient's `Server SPN`. A blank one is ignored.
    ///
    /// - Defaults to `MSSQLSvc/<host>:<port>`, or `MSSQLSvc/<host>:<instance>`
    ///   for a named instance without a port.
    pub fn server_spn(&mut self, spn: impl ToString) {
        self.server_spn = Some(spn.to_string());
    }

    /// The SPN set with [`server_spn`](Config::server_spn), unless blank:
    /// SqlClient derives one for a null or white-space `ServerSPN`.
    pub(crate) fn get_server_spn(&self) -> Option<&str> {
        self.server_spn
            .as_deref()
            .filter(|spn| !spn.trim().is_empty())
    }

    /// Supplies a client certificate and private key used to authenticate the
    /// client to the server during the TLS handshake (mutual TLS). This is
    /// required for TDS 8.0 "strict" connections that use client-certificate
    /// authentication (`ENCRYPT_CLIENT_CERT`), and may also be used with the
    /// classic (pre-8.0) TLS handshake when the server requests a client
    /// certificate.
    ///
    /// Both arguments are paths to files:
    ///
    /// - `cert`: the client certificate, PEM (`.pem`/`.crt`) or DER (`.der`).
    /// - `key`: the matching private key, PEM (`.pem`/`.key`) or DER (`.der`,
    ///   PKCS#8).
    ///
    /// Backend support:
    ///
    /// - `rustls`: PEM and DER certificate/key files.
    /// - `native-tls`: PEM certificate + PEM PKCS#8 key only (DER files are
    ///   rejected at connect time; use [`client_certificate_pkcs12`] for a
    ///   bundled DER identity).
    /// - `vendored-openssl` (opentls): does not support separate certificate/key
    ///   files; use [`client_certificate_pkcs12`] instead.
    ///
    /// - Defaults to no client certificate.
    ///
    /// [`client_certificate_pkcs12`]: Config::client_certificate_pkcs12
    #[cfg(any(
        feature = "rustls",
        feature = "native-tls",
        feature = "vendored-openssl"
    ))]
    #[cfg_attr(
        docsrs,
        doc(cfg(any(
            feature = "rustls",
            feature = "native-tls",
            feature = "vendored-openssl"
        )))
    )]
    pub fn client_certificate(&mut self, cert: impl Into<PathBuf>, key: impl Into<PathBuf>) {
        self.client_cert = Some(ClientCertificate {
            source: ClientCertSource::CertAndKey {
                cert: cert.into(),
                key: key.into(),
            },
        });
    }

    /// Supplies a client identity from a PKCS#12 / PFX bundle (certificate,
    /// private key and any chain, encrypted with `password`) used to
    /// authenticate the client to the server during the TLS handshake (mutual
    /// TLS).
    ///
    /// Only supported by the `native-tls` and `vendored-openssl` backends; the
    /// `rustls` backend rejects PKCS#12 identities at connect time (supply
    /// separate PEM/DER files via [`client_certificate`] instead).
    ///
    /// - Defaults to no client certificate.
    ///
    /// [`client_certificate`]: Config::client_certificate
    #[cfg(any(feature = "native-tls", feature = "vendored-openssl"))]
    #[cfg_attr(
        docsrs,
        doc(cfg(any(feature = "native-tls", feature = "vendored-openssl")))
    )]
    pub fn client_certificate_pkcs12(
        &mut self,
        path: impl Into<PathBuf>,
        password: impl Into<String>,
    ) {
        self.client_cert = Some(ClientCertificate {
            source: ClientCertSource::Pkcs12 {
                path: path.into(),
                password: zeroize::Zeroizing::new(password.into()),
            },
        });
    }

    #[cfg(any(
        feature = "rustls",
        feature = "native-tls",
        feature = "vendored-openssl"
    ))]
    pub(crate) fn get_client_certificate(&self) -> Option<&ClientCertificate> {
        self.client_cert.as_ref()
    }

    pub(crate) fn get_host(&self) -> &str {
        self.host
            .as_deref()
            .filter(|v| v != &".")
            .unwrap_or("localhost")
    }

    #[cfg(any(
        feature = "rustls",
        feature = "native-tls",
        feature = "vendored-openssl"
    ))]
    pub(crate) fn get_hostname_in_certificate(&self) -> &str {
        self.hostname_in_certificate
            .as_deref()
            .unwrap_or_else(|| self.get_host())
    }

    /// The certificate validation to apply. Strict encryption always
    /// validates: like SqlClient, it ignores `trust_cert`.
    #[cfg(any(
        feature = "rustls",
        feature = "native-tls",
        feature = "vendored-openssl"
    ))]
    pub(crate) fn tls_trust(&self) -> &TrustConfig {
        static VALIDATE: TrustConfig = TrustConfig::Default;

        match (&self.trust, self.encryption) {
            (TrustConfig::TrustAll, EncryptionLevel::Strict) => {
                tracing::warn!(
                    "TrustServerCertificate is ignored with strict encryption; validating the server certificate."
                );
                &VALIDATE
            }
            (trust, _) => trust,
        }
    }

    pub(crate) fn get_port(&self) -> u16 {
        match (self.port, self.instance_name.as_ref()) {
            // A user-defined port, we must use that.
            (Some(port), _) => port,
            // If using a named instance, we'll give the default port of SQL
            // Browser.
            (None, Some(_)) => 1434,
            // Otherwise the defaulting to the default SQL Server port.
            (None, None) => 1433,
        }
    }

    /// Get the host address including port
    pub fn get_addr(&self) -> String {
        format!("{}:{}", self.get_host(), self.get_port())
    }

    /// Creates a new `Config` from an [ADO.NET connection string].
    ///
    /// # Supported parameters
    ///
    /// All parameter keys are handled case-insensitive. The last of a key and
    /// its synonyms wins, and a key SqlClient doesn't know is an error.
    ///
    /// |Parameter|Allowed values|Description|
    /// |--------|--------|--------|
    /// |`Data Source`, `server`, `addr`, `address`, `network address`|`<string>`|The name or network address of the instance of SQL Server to which to connect. The port number can be specified after the server name. The correct form of this parameter is either `tcp:host,port` or `tcp:host\\instance`; other protocols (`np:`, `lpc:`, `admin:`) are errors.|
    /// |`Integrated Security`, `IntegratedSecurity`, `Trusted_Connection`|`sspi`,`true`,`false`,`yes`,`no`|Toggle between Windows/Kerberos authentication and SQL authentication.|
    /// |`User ID`, `uid`, `user`, `username`|`<string>`|The SQL Server login account.|
    /// |`password`,`pwd`|`<string>`|The password for the SQL Server account logging on.|
    /// |`Initial Catalog`, `database`|`<string>`|The name of the database.|
    /// |`Trust Server Certificate`, `TrustServerCertificate`|`true`,`false`,`yes`,`no`|Specifies whether the driver trusts the server certificate when connecting using TLS. Cannot be used toghether with `TrustServerCertificateCA`|
    /// |`TrustServerCertificateCA`|`<path>`|Path to a `pem`, `crt` or `der` certificate file. Cannot be used together with `TrustServerCertificate`|
    /// |`encrypt`|`strict`,`true`,`false`,`yes`,`no`,`DANGER_PLAINTEXT`|Specifies whether the driver uses TLS to encrypt communication. `strict` (TDS 8.0) requires the `tds80` feature.|
    /// |`Application Name`, `app`, `ApplicationName`|`<string>`|Sets the application name for the connection.|
    /// |`Application Intent`, `ApplicationIntent`|`ReadOnly`,`ReadWrite`|Declares a read-only workload.|
    /// |`Host Name In Certificate`, `HostNameInCertificate`|`<string>`|The hostname the server certificate is validated against. Defaults to `server`.|
    /// |`Workstation ID`, `WorkstationID`, `wsid`|`<string>`|The client / workstation name reported to the server.|
    /// |`Multi Subnet Failover`, `MultiSubnetFailover`|`true`,`false`,`yes`,`no`|When enabled, connections are attempted in parallel to all IP addresses the server resolves to, and the first to succeed is used.|
    /// |`Connect Timeout`, `Connection Timeout`, `Timeout`|`<seconds>`|See [`Config::connect_timeout`].|
    /// |`Packet Size`|`512` to `32768`|See [`Config::packet_size`].|
    /// |`Server SPN`, `ServerSPN`|`<string>`|See [`Config::server_spn`].|
    ///
    /// SqlClient's pooling, retry, `Command Timeout`, `Enlist` and similar
    /// keywords are accepted and ignored. Those this crate can't honour without
    /// changing the connection are errors: `MultipleActiveResultSets=true`,
    /// `Failover Partner`, `Column Encryption Setting=Enabled`,
    /// `AttachDbFilename`, `Current Language`, `Server Certificate`,
    /// `User Instance=true`, `Replication=true`, Entra ID `Authentication`
    /// values and `Network Library` other than `dbmssocn`.
    ///
    /// [ADO.NET connection string]: https://docs.microsoft.com/en-us/dotnet/framework/data/adonet/connection-strings
    pub fn from_ado_string(s: &str) -> crate::Result<Self> {
        let ado: AdoNetConfig = s.parse()?;
        Self::from_config_string(ado)
    }

    /// Creates a new `Config` from a [JDBC connection string].
    ///
    /// See [`from_ado_string`] method for supported parameters. The URL is
    /// split as mssql-jdbc splits it: a value containing `;` goes in braces,
    /// `{like;this}`, with `}}` for a `}`; other values and all names are
    /// trimmed; the last of a property and its synonyms wins.
    ///
    /// [JDBC connection string]: https://docs.microsoft.com/en-us/sql/connect/jdbc/building-the-connection-url?view=sql-server-ver15
    /// [`from_ado_string`]: #method.from_ado_string
    pub fn from_jdbc_string(s: &str) -> crate::Result<Self> {
        let jdbc: JdbcConfig = s.parse()?;
        Self::from_config_string(jdbc)
    }

    fn from_config_string(s: impl ConfigString) -> crate::Result<Self> {
        let mut builder = Self::new();

        let server = s.server()?;

        if let Some(host) = server.host {
            builder.host(host);
        }

        if let Some(port) = server.port {
            builder.port(port);
        }

        if let Some(instance) = server.instance {
            builder.instance_name(instance);
        }

        builder.authentication(s.authentication()?);

        if let Some(database) = s.database() {
            builder.database(database);
        }

        if let Some(name) = s.application_name() {
            builder.application_name(name);
        }

        let encryption = s.encrypt()?;
        builder.encryption(encryption);

        match (s.trust_cert()?, s.trust_cert_ca()) {
            // SqlClient ignores TrustServerCertificate under Encrypt=Strict.
            (true, ca) if encryption == EncryptionLevel::Strict => {
                tracing::warn!(
                    "TrustServerCertificate is ignored with Encrypt=Strict; validating the server certificate."
                );
                if let Some(ca) = ca {
                    builder.trust_cert_ca(ca);
                }
            }
            (true, Some(_)) => {
                return Err(crate::Error::Conversion(
                    "Connection string: TrustServerCertificate=true and TrustServerCertificateCA are mutually exclusive"
                        .into(),
                ))
            }
            (true, None) => builder.trust_cert(),
            (false, Some(ca)) => builder.trust_cert_ca(ca),
            (false, None) => (),
        }

        if let Some(hostname_in_cert) = s.hostname_in_certificate() {
            builder.hostname_in_certificate(hostname_in_cert);
        }

        builder.readonly(s.readonly());

        if let Some(client_name) = s.client_name() {
            builder.client_name(client_name);
        }
        builder.multi_subnet_failover(s.multi_subnet_failover()?);
        if let Some(size) = s.packet_size()? {
            builder.packet_size(size);
        }
        builder.set_connect_timeout(s.connect_timeout()?);
        if let Some(spn) = s.server_spn() {
            builder.server_spn(spn);
        }

        Ok(builder)
    }
}

/// A builder for [`Config`], providing an ergonomic, chainable way to
/// construct a connection configuration.
///
/// Create a builder with [`Config::builder`], set the desired options by
/// calling its methods (each returns the builder to allow chaining) and
/// finalize it with [`build`].
///
/// # Example
///
/// ```
/// # use tiberius::{Config, AuthMethod, EncryptionLevel};
/// let config = Config::builder()
///     .host("localhost")
///     .port(1433)
///     .database("master")
///     .encryption(EncryptionLevel::NotSupported)
///     .authentication(AuthMethod::sql_server("SA", "<password>"))
///     .build();
/// ```
///
/// [`Config`]: struct.Config.html
/// [`Config::builder`]: struct.Config.html#method.builder
/// [`build`]: struct.ConfigBuilder.html#method.build
#[derive(Clone, Debug)]
pub struct ConfigBuilder {
    inner: Config,
}

impl ConfigBuilder {
    /// A host or ip address to connect to.
    ///
    /// - Defaults to `localhost`.
    pub fn host(mut self, host: impl ToString) -> Self {
        self.inner.host = Some(host.to_string());
        self
    }

    /// The server port.
    ///
    /// - Defaults to `1433`.
    pub fn port(mut self, port: u16) -> Self {
        self.inner.port = Some(port);
        self
    }

    /// The database to connect to.
    ///
    /// - Defaults to `master`.
    pub fn database(mut self, database: impl ToString) -> Self {
        self.inner.database = Some(database.to_string());
        self
    }

    /// The instance name as defined in the SQL Browser. Only available on
    /// Windows platforms.
    ///
    /// If specified, the port is replaced with the value returned from the
    /// browser.
    ///
    /// - Defaults to no name specified.
    pub fn instance_name(mut self, name: impl ToString) -> Self {
        self.inner.instance_name = Some(name.to_string());
        self
    }

    /// Sets the application name to the connection, queryable with the
    /// `APP_NAME()` command.
    ///
    /// - Defaults to no name specified.
    pub fn application_name(mut self, name: impl ToString) -> Self {
        self.inner.application_name = Some(name.to_string());
        self
    }

    /// Set the preferred encryption level.
    ///
    /// - With `tls` feature, defaults to `Required`.
    /// - Without `tls` feature, defaults to `NotSupported`.
    pub fn encryption(mut self, encryption: EncryptionLevel) -> Self {
        self.inner.encryption = encryption;
        self
    }

    /// If set, the server certificate will not be validated and it is accepted
    /// as-is.
    ///
    /// On production setting, the certificate should be added to the local key
    /// storage (or use `trust_cert_ca` instead), using this setting is potentially dangerous.
    ///
    /// # Panics
    /// Will panic in case `trust_cert_ca` was called before.
    ///
    /// - Defaults to `default`, meaning server certificate is validated against system-truststore.
    pub fn trust_cert(mut self) -> Self {
        if let TrustConfig::CaCertificateLocation(_) = &self.inner.trust {
            panic!("'trust_cert' and 'trust_cert_ca' are mutual exclusive! Only use one.")
        }
        self.inner.trust = TrustConfig::TrustAll;
        self
    }

    /// If set, the server certificate will be validated against the given CA certificate in
    /// in addition to the system-truststore.
    /// Useful when using self-signed certificates on the server without having to disable the
    /// trust-chain.
    ///
    /// # Panics
    /// Will panic in case `trust_cert` was called before.
    ///
    /// - Defaults to validating the server certificate is validated against system's certificate storage.
    pub fn trust_cert_ca(mut self, path: impl ToString) -> Self {
        if let TrustConfig::TrustAll = &self.inner.trust {
            panic!("'trust_cert' and 'trust_cert_ca' are mutual exclusive! Only use one.")
        } else {
            self.inner.trust = TrustConfig::CaCertificateLocation(PathBuf::from(path.to_string()))
        }
        self
    }

    /// Sets the authentication method.
    ///
    /// - Defaults to `None`.
    pub fn authentication(mut self, auth: AuthMethod) -> Self {
        self.inner.auth = auth;
        self
    }

    /// Sets ApplicationIntent readonly.
    ///
    /// - Defaults to `false`.
    pub fn readonly(mut self, readonly: bool) -> Self {
        self.inner.readonly = readonly;
        self
    }

    /// Sets the connect timeout; see [`Config::connect_timeout`].
    pub fn connect_timeout(mut self, timeout: Option<Duration>) -> Self {
        self.inner.connect_timeout = timeout;
        self
    }

    /// Sets the SPN for integrated authentication; see
    /// [`Config::server_spn`].
    pub fn server_spn(mut self, spn: impl ToString) -> Self {
        self.inner.server_spn(spn);
        self
    }

    /// Supplies a client certificate and private key for mutual TLS.
    ///
    /// See [`Config::client_certificate`] for details and backend support.
    #[cfg(any(
        feature = "rustls",
        feature = "native-tls",
        feature = "vendored-openssl"
    ))]
    #[cfg_attr(
        docsrs,
        doc(cfg(any(
            feature = "rustls",
            feature = "native-tls",
            feature = "vendored-openssl"
        )))
    )]
    pub fn client_certificate(mut self, cert: impl Into<PathBuf>, key: impl Into<PathBuf>) -> Self {
        self.inner.client_certificate(cert, key);
        self
    }

    /// Supplies a client identity from a PKCS#12 / PFX bundle for mutual TLS.
    ///
    /// See [`Config::client_certificate_pkcs12`] for details and backend
    /// support.
    #[cfg(any(feature = "native-tls", feature = "vendored-openssl"))]
    #[cfg_attr(
        docsrs,
        doc(cfg(any(feature = "native-tls", feature = "vendored-openssl")))
    )]
    pub fn client_certificate_pkcs12(
        mut self,
        path: impl Into<PathBuf>,
        password: impl Into<String>,
    ) -> Self {
        self.inner.client_certificate_pkcs12(path, password);
        self
    }

    /// Produces the finalized [`Config`] from this builder.
    ///
    /// [`Config`]: struct.Config.html
    pub fn build(self) -> Config {
        self.inner
    }
}

impl From<Config> for ConfigBuilder {
    fn from(config: Config) -> Self {
        ConfigBuilder { inner: config }
    }
}

impl From<ConfigBuilder> for Config {
    fn from(builder: ConfigBuilder) -> Self {
        builder.inner
    }
}

pub(crate) struct ServerDefinition {
    host: Option<String>,
    port: Option<u16>,
    instance: Option<String>,
}

pub(crate) trait ConfigString {
    fn get(&self, key: &str) -> Option<&str>;

    /// The value of the first of `keys` present.
    fn first(&self, keys: &[&str]) -> Option<&str> {
        keys.iter().find_map(|key| self.get(key))
    }

    fn server(&self) -> crate::Result<ServerDefinition>;

    fn connect_timeout(&self) -> crate::Result<Option<Duration>>;

    fn packet_size(&self) -> crate::Result<Option<u32>> {
        Ok(None)
    }

    /// `sspi`, `true` or `yes`; `false` or `no` (SqlClient's
    /// `ConvertValueToIntegratedSecurityInternal`).
    fn integrated_security(&self) -> crate::Result<bool> {
        match self.first(&["integrated security", "integratedsecurity"]) {
            Some(v) if v.trim().eq_ignore_ascii_case("sspi") => Ok(true),
            Some(v) => Self::parse_bool(v),
            None => Ok(false),
        }
    }

    fn authentication(&self) -> crate::Result<AuthMethod> {
        let user = self.first(&["uid", "username", "user", "user id"]);

        let pw = self.first(&["password", "pwd"]);

        if !self.integrated_security()? {
            return Ok(AuthMethod::sql_server(user.unwrap_or(""), pw.unwrap_or("")));
        }

        // Without a backend this used to log in with SQL authentication.
        #[allow(unused)]
        let missing = |what: &str| -> crate::Result<AuthMethod> {
            Err(crate::Error::Conversion(
                format!("Connection string: Integrated Security needs {what}").into(),
            ))
        };

        #[cfg(all(windows, feature = "winauth"))]
        return match (user, pw) {
            (None, None) => Ok(AuthMethod::Integrated),
            _ => Ok(AuthMethod::windows(user.unwrap_or(""), pw.unwrap_or(""))),
        };
        #[cfg(all(windows, not(feature = "winauth")))]
        return missing("the crate's `winauth` feature");

        // `sspi-rs` is NTLM, so it needs a user name; Kerberos takes the
        // logged-in user's ticket.
        #[cfg(all(unix, feature = "sspi-rs"))]
        return match (user, pw) {
            (Some(user), Some(pw)) => Ok(AuthMethod::windows(user, pw)),
            #[cfg(feature = "integrated-auth-gssapi")]
            (None, None) => Ok(AuthMethod::Integrated),
            #[cfg(not(feature = "integrated-auth-gssapi"))]
            (None, None) => missing("a User ID, or the crate's `integrated-auth-gssapi` feature"),
            _ => Ok(AuthMethod::windows(user.unwrap_or(""), pw.unwrap_or(""))),
        };
        #[cfg(all(unix, feature = "integrated-auth-gssapi", not(feature = "sspi-rs")))]
        return Ok(AuthMethod::Integrated);
        #[cfg(all(
            unix,
            not(any(feature = "integrated-auth-gssapi", feature = "sspi-rs"))
        ))]
        return missing(
            "the crate's `integrated-auth-gssapi` (Kerberos) or `sspi-rs` (NTLM) feature",
        );

        #[cfg(not(any(windows, unix)))]
        return missing("Windows or Unix");
    }

    fn database(&self) -> Option<String> {
        self.first(&["database", "initial catalog", "databasename"])
            .map(|db| db.to_string())
    }

    fn application_name(&self) -> Option<String> {
        self.first(&["application name", "applicationname"])
            .map(|name| name.to_string())
    }

    fn trust_cert(&self) -> crate::Result<bool> {
        self.first(&["trust server certificate", "trustservercertificate"])
            .map(Self::parse_bool)
            .unwrap_or(Ok(false))
    }

    fn trust_cert_ca(&self) -> Option<String> {
        self.first(&["trustservercertificateca"])
            .map(|ca| ca.to_string())
    }

    fn hostname_in_certificate(&self) -> Option<String> {
        self.first(&[
            "host name in certificate",
            "hostnameincertificate",
            "hostname in certificate",
        ])
        .map(|host| host.to_string())
    }

    fn client_name(&self) -> Option<String> {
        self.first(&["workstation id", "workstationid"])
            .map(|name| name.to_string())
    }

    /// ADO.NET `Server SPN`, JDBC `serverSpn`.
    fn server_spn(&self) -> Option<String> {
        self.first(&["server spn", "serverspn"]).map(str::to_owned)
    }

    #[cfg(any(
        feature = "rustls",
        feature = "native-tls",
        feature = "vendored-openssl"
    ))]
    fn encrypt(&self) -> crate::Result<EncryptionLevel> {
        self.first(&["encrypt"])
            .map(|val| {
                let val = val.trim();
                let is = |s: &str| val.eq_ignore_ascii_case(s);
                // SqlClient 5+: true/yes/mandatory, false/no/optional, strict.
                if is("true") || is("yes") || is("mandatory") {
                    Ok(EncryptionLevel::Required)
                } else if is("false") || is("no") || is("optional") {
                    Ok(EncryptionLevel::Off)
                } else if val == "DANGER_PLAINTEXT" {
                    Ok(EncryptionLevel::NotSupported)
                } else if is("strict") && cfg!(feature = "tds80") {
                    Ok(EncryptionLevel::Strict)
                } else if is("strict") {
                    Err(crate::Error::Conversion(
                        "encrypt=strict requires the crate's `tds80` feature to be enabled".into(),
                    ))
                } else {
                    Err(crate::Error::Conversion(
                        "Connection string: Encrypt must be true, false, yes, no, mandatory, optional or strict".into(),
                    ))
                }
            })
            // When the `encrypt` keyword is omitted, default to requiring
            // encryption — matching `Config::default()` and modern ADO.NET
            // (`Encrypt=Mandatory`). Callers who want an unencrypted connection
            // must opt out explicitly with `encrypt=false` (or
            // `encrypt=DANGER_PLAINTEXT`).
            .unwrap_or(Ok(EncryptionLevel::Required))
    }

    #[cfg(not(any(
        feature = "rustls",
        feature = "native-tls",
        feature = "vendored-openssl"
    )))]
    fn encrypt(&self) -> crate::Result<EncryptionLevel> {
        Ok(EncryptionLevel::NotSupported)
    }

    fn parse_bool<T: AsRef<str>>(v: T) -> crate::Result<bool> {
        match v.as_ref().trim().to_lowercase().as_str() {
            "true" | "yes" => Ok(true),
            "false" | "no" => Ok(false),
            _ => Err(crate::Error::Conversion(
                "Connection string: Not a valid boolean".into(),
            )),
        }
    }

    fn readonly(&self) -> bool {
        self.first(&["application intent", "applicationintent"])
            .filter(|val| val.trim().eq_ignore_ascii_case("ReadOnly"))
            .is_some()
    }

    fn multi_subnet_failover(&self) -> crate::Result<bool> {
        self.first(&["multi subnet failover", "multisubnetfailover"])
            .map(Self::parse_bool)
            .unwrap_or(Ok(false))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_builder_constructs_config() {
        let config = Config::builder()
            .host("db.example.com")
            .port(4433)
            .database("northwind")
            .application_name("my-app")
            .authentication(AuthMethod::sql_server("SA", "secret"))
            .readonly(true)
            .build();

        assert_eq!("db.example.com", config.get_host());
        assert_eq!(4433, config.get_port());
        assert_eq!("db.example.com:4433", config.get_addr());
        assert_eq!(Some("northwind"), config.database.as_deref());
        assert_eq!(Some("my-app"), config.application_name.as_deref());
        assert!(config.readonly);
        assert!(matches!(config.auth, AuthMethod::SqlServer(_)));
        assert!(matches!(config.trust, TrustConfig::Default));
    }

    #[test]
    fn config_builder_roundtrips_via_from() {
        let config = Config::builder().host("localhost").port(1433).build();
        let builder: ConfigBuilder = config.into();
        let config = builder.database("master").build();

        assert_eq!("localhost:1433", config.get_addr());
        assert_eq!(Some("master"), config.database.as_deref());
    }

    #[test]
    fn config_from_builder_carries_builder_settings() {
        // `From<ConfigBuilder>` must return the built inner config, not a default.
        let config: Config = Config::builder().host("db.internal").port(2020).into();
        assert_eq!("db.internal", config.get_host());
        assert_eq!(2020, config.get_port());
    }

    #[test]
    fn get_packet_size_reflects_the_set_value() {
        let mut config = Config::new();
        assert_eq!(config.get_packet_size(), None);
        config.packet_size(8192);
        assert_eq!(config.get_packet_size(), Some(8192));
    }

    #[test]
    fn server_spn_setters() {
        let mut config = Config::new();
        assert_eq!(config.get_server_spn(), None);
        config.server_spn("MSSQLSvc/a:1");
        assert_eq!(config.get_server_spn(), Some("MSSQLSvc/a:1"));
        config.server_spn("");
        assert_eq!(config.get_server_spn(), None);

        let config = Config::builder().server_spn("MSSQLSvc/b:2").build();
        assert_eq!(config.get_server_spn(), Some("MSSQLSvc/b:2"));
    }

    #[test]
    fn from_jdbc_string_parses_host_and_port() {
        let config =
            Config::from_jdbc_string("jdbc:sqlserver://db.example.com:2345").expect("valid jdbc");
        assert_eq!("db.example.com", config.get_host());
        assert_eq!(2345, config.get_port());
    }

    #[cfg(any(
        feature = "rustls",
        feature = "native-tls",
        feature = "vendored-openssl"
    ))]
    #[test]
    fn get_hostname_in_certificate_falls_back_to_host() {
        let mut config = Config::new();
        config.host("real.host");
        // Unset: falls back to the connection host.
        assert_eq!(config.get_hostname_in_certificate(), "real.host");
        // Set: returns the explicit certificate hostname.
        config.hostname_in_certificate("cert.host");
        assert_eq!(config.get_hostname_in_certificate(), "cert.host");
    }

    #[cfg(any(
        feature = "rustls",
        feature = "native-tls",
        feature = "vendored-openssl"
    ))]
    #[test]
    fn client_certificate_sets_cert_and_key_source() {
        let mut config = Config::new();
        assert!(config.get_client_certificate().is_none());

        config.client_certificate("/tmp/client.pem", "/tmp/client.key");

        let cert = config
            .get_client_certificate()
            .expect("client certificate should be set");
        match &cert.source {
            ClientCertSource::CertAndKey { cert, key } => {
                assert_eq!(cert, &PathBuf::from("/tmp/client.pem"));
                assert_eq!(key, &PathBuf::from("/tmp/client.key"));
            }
            #[allow(unreachable_patterns)]
            other => panic!("expected CertAndKey source, got {other:?}"),
        }
    }

    #[cfg(any(
        feature = "rustls",
        feature = "native-tls",
        feature = "vendored-openssl"
    ))]
    #[test]
    fn config_builder_sets_client_certificate() {
        let config = Config::builder()
            .host("localhost")
            .client_certificate("cert.der", "key.der")
            .build();

        match &config
            .get_client_certificate()
            .expect("client certificate should be set")
            .source
        {
            ClientCertSource::CertAndKey { cert, key } => {
                assert_eq!(cert, &PathBuf::from("cert.der"));
                assert_eq!(key, &PathBuf::from("key.der"));
            }
            #[allow(unreachable_patterns)]
            other => panic!("expected CertAndKey source, got {other:?}"),
        }
    }

    #[cfg(any(feature = "native-tls", feature = "vendored-openssl"))]
    #[test]
    fn client_certificate_pkcs12_sets_bundle_source() {
        let mut config = Config::new();
        config.client_certificate_pkcs12("/tmp/identity.pfx", "s3cr3t");

        match &config
            .get_client_certificate()
            .expect("client certificate should be set")
            .source
        {
            ClientCertSource::Pkcs12 { path, password } => {
                assert_eq!(path, &PathBuf::from("/tmp/identity.pfx"));
                assert_eq!(password.as_str(), "s3cr3t");
            }
            other => panic!("expected Pkcs12 source, got {other:?}"),
        }
    }

    #[cfg(any(feature = "native-tls", feature = "vendored-openssl"))]
    #[test]
    fn client_certificate_debug_redacts_pkcs12_password() {
        let mut config = Config::new();
        config.client_certificate_pkcs12("/tmp/identity.pfx", "topsecret");

        let dbg = format!("{:?}", config.get_client_certificate().unwrap());
        assert!(dbg.contains("<redacted>"));
        assert!(!dbg.contains("topsecret"));
    }

    #[cfg(all(unix, feature = "sspi-rs"))]
    #[test]
    fn ado_integrated_security_sspi_with_credentials_uses_windows_ntlm() {
        let config = Config::from_ado_string(
            "server=tcp:localhost,1433;IntegratedSecurity=SSPI;uid=DOMAIN\\user;pwd=secret",
        )
        .unwrap();

        match &config.auth {
            AuthMethod::Windows(auth) => {
                assert_eq!("user", auth.user);
                assert_eq!(Some("DOMAIN"), auth.domain.as_deref());
            }
            other => panic!("expected Windows NTLM auth, got {other:?}"),
        }
    }

    #[test]
    fn config_direct_setters_populate_fields() {
        let mut config = Config::new();
        config.database("northwind");
        config.instance_name("SQLEXPRESS");
        config.client_name("workstation-7");

        assert_eq!(Some("northwind"), config.database.as_deref());
        assert_eq!(Some("SQLEXPRESS"), config.instance_name.as_deref());
        assert_eq!(Some("workstation-7"), config.client_name.as_deref());
    }

    #[test]
    fn get_port_defaults_without_port_or_instance() {
        // No explicit port and no instance -> default SQL Server port.
        let config = Config::new();
        assert_eq!(1433, config.get_port());
    }

    #[test]
    fn get_port_uses_sql_browser_port_for_named_instance() {
        // A named instance without an explicit port -> SQL Browser port.
        let mut config = Config::new();
        config.instance_name("SQLEXPRESS");
        assert_eq!(1434, config.get_port());
    }

    #[test]
    #[should_panic(expected = "mutual exclusive")]
    fn trust_cert_after_trust_cert_ca_panics() {
        let mut config = Config::new();
        config.trust_cert_ca("/tmp/ca.crt");
        config.trust_cert();
    }

    #[test]
    #[should_panic(expected = "mutual exclusive")]
    fn trust_cert_ca_after_trust_cert_panics() {
        let mut config = Config::new();
        config.trust_cert();
        config.trust_cert_ca("/tmp/ca.crt");
    }

    #[test]
    fn trust_cert_ca_sets_ca_location() {
        let mut config = Config::new();
        config.trust_cert_ca("/tmp/ca.crt");
        assert!(matches!(
            config.trust,
            TrustConfig::CaCertificateLocation(_)
        ));
    }

    #[test]
    fn config_builder_covers_all_setters() {
        let config = Config::builder()
            .host("localhost")
            .instance_name("SQLEXPRESS")
            .encryption(EncryptionLevel::Off)
            .trust_cert_ca("/tmp/ca.crt")
            .build();

        assert_eq!(Some("SQLEXPRESS"), config.instance_name.as_deref());
        assert!(matches!(config.encryption, EncryptionLevel::Off));
        assert!(matches!(
            config.trust,
            TrustConfig::CaCertificateLocation(_)
        ));
    }

    #[test]
    fn config_builder_trust_cert_sets_trust_all() {
        let config = Config::builder().trust_cert().build();
        assert!(matches!(config.trust, TrustConfig::TrustAll));
    }

    #[test]
    #[should_panic(expected = "mutual exclusive")]
    fn config_builder_trust_cert_after_ca_panics() {
        Config::builder().trust_cert_ca("/tmp/ca.crt").trust_cert();
    }

    #[test]
    #[should_panic(expected = "mutual exclusive")]
    fn config_builder_trust_cert_ca_after_trust_cert_panics() {
        Config::builder().trust_cert().trust_cert_ca("/tmp/ca.crt");
    }

    #[test]
    fn from_ado_string_populates_optional_fields() {
        let config = Config::from_ado_string(
            "server=tcp:my-server.com\\SQLEXPRESS;database=northwind;\
             HostNameInCertificate=cert.host;WorkstationID=ws-1",
        )
        .expect("valid ado string");

        assert_eq!("my-server.com", config.get_host());
        assert_eq!(Some("SQLEXPRESS"), config.instance_name.as_deref());
        assert_eq!(Some("northwind"), config.database.as_deref());
        assert_eq!(Some("cert.host"), config.hostname_in_certificate.as_deref());
        assert_eq!(Some("ws-1"), config.client_name.as_deref());
    }

    #[cfg(any(
        feature = "rustls",
        feature = "native-tls",
        feature = "vendored-openssl"
    ))]
    #[test]
    fn client_cert_source_debug_formats_cert_and_key() {
        let mut config = Config::new();
        config.client_certificate("/tmp/client.pem", "/tmp/client.key");

        let dbg = format!("{:?}", config.get_client_certificate().unwrap().source);
        assert!(dbg.contains("CertAndKey"));
        assert!(dbg.contains("client.pem"));
        assert!(dbg.contains("client.key"));
    }

    #[cfg(any(feature = "native-tls", feature = "vendored-openssl"))]
    #[test]
    fn config_builder_sets_pkcs12_client_certificate() {
        let config = Config::builder()
            .client_certificate_pkcs12("/tmp/identity.pfx", "s3cr3t")
            .build();

        match &config
            .get_client_certificate()
            .expect("client certificate should be set")
            .source
        {
            ClientCertSource::Pkcs12 { path, password } => {
                assert_eq!(path, &PathBuf::from("/tmp/identity.pfx"));
                assert_eq!(password.as_str(), "s3cr3t");
            }
            other => panic!("expected Pkcs12 source, got {other:?}"),
        }
    }

    #[cfg(all(unix, feature = "sspi-rs"))]
    #[test]
    fn ado_integrated_security_sspi_with_partial_credentials_uses_windows() {
        // Only a username (no password) -> falls into the catch-all NTLM arm.
        let config = Config::from_ado_string(
            "server=tcp:localhost,1433;IntegratedSecurity=SSPI;uid=onlyuser",
        )
        .unwrap();

        match &config.auth {
            AuthMethod::Windows(auth) => {
                assert_eq!("onlyuser", auth.user);
            }
            other => panic!("expected Windows auth, got {other:?}"),
        }
    }

    #[test]
    fn trust_cert_with_trust_cert_ca_is_an_error_not_a_panic() {
        let res = Config::from_ado_string(
            "server=tcp:h,1433;TrustServerCertificate=true;TrustServerCertificateCA=ca.pem",
        );
        assert!(matches!(res, Err(crate::Error::Conversion(_))));

        let res = Config::from_jdbc_string(
            "jdbc:sqlserver://h:1433;trustServerCertificate=true;TrustServerCertificateCA=ca.pem",
        );
        assert!(matches!(res, Err(crate::Error::Conversion(_))));
    }

    #[cfg(feature = "tds80")]
    #[test]
    fn strict_ignores_trust_server_certificate() {
        let config =
            Config::from_ado_string("server=tcp:h,1433;Encrypt=Strict;TrustServerCertificate=true")
                .unwrap();
        assert!(matches!(config.trust, TrustConfig::Default));

        // The CA is still honoured.
        let config = Config::from_ado_string(
            "server=tcp:h,1433;encrypt=strict;TrustServerCertificate=yes;TrustServerCertificateCA=ca.pem",
        )
        .unwrap();
        assert!(matches!(
            config.trust,
            TrustConfig::CaCertificateLocation(_)
        ));

        // Set through the API, it is ignored when TLS is set up.
        let mut config = Config::new();
        config.trust_cert();
        assert!(matches!(config.tls_trust(), TrustConfig::TrustAll));
        config.encryption(EncryptionLevel::Strict);
        assert!(matches!(config.tls_trust(), TrustConfig::Default));
    }

    #[test]
    fn connect_timeout_defaults_and_setters() {
        use std::time::Duration;
        assert_eq!(
            Config::new().connect_timeout(),
            Some(Duration::from_secs(15))
        );

        let mut config = Config::new();
        config.set_connect_timeout(None);
        assert_eq!(config.connect_timeout(), None);

        let config = Config::builder()
            .connect_timeout(Some(Duration::from_secs(3)))
            .build();
        assert_eq!(config.connect_timeout(), Some(Duration::from_secs(3)));

        let config = Config::from_ado_string("server=tcp:h,1433;Connect Timeout=0").unwrap();
        assert_eq!(config.connect_timeout(), None);
        let config = Config::from_jdbc_string("jdbc:sqlserver://h;loginTimeout=9").unwrap();
        assert_eq!(config.connect_timeout(), Some(Duration::from_secs(9)));
    }
}
