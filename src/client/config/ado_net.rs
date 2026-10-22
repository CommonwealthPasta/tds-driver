use super::{ConfigString, ServerDefinition};
use std::{collections::HashMap, str::FromStr, time::Duration};
use zeroize::Zeroizing;

/// An ADO.NET connection string, split with SqlClient's rules.
pub(crate) struct AdoNetConfig {
    dict: HashMap<String, Zeroizing<String>>,
}

impl FromStr for AdoNetConfig {
    type Err = crate::error::Error;

    fn from_str(s: &str) -> crate::Result<Self> {
        let dict = parse(s, |key| match keyword(&key.to_lowercase()) {
            Some(keyword) => Ok(keyword.to_owned()),
            None => Ok(key.to_lowercase()),
        })?;
        Ok(Self { dict })
    }
}

/// The keyword `key` (lowercase) is stored under, for keys with synonyms:
/// SqlClient's (SqlConnectionOptions' keyword map, DbConnectionStringKeywords,
/// DbConnectionStringSynonyms) and the crate's older spellings. Keys are
/// stored by keyword, so the last of a set of synonyms wins.
fn keyword(key: &str) -> Option<&'static str> {
    Some(match key {
        "application intent" | "applicationintent" => "application intent",
        "application name" | "app" | "applicationname" => "application name",
        "connect timeout" | "connection timeout" | "connecttimeout" | "timeout" => {
            "connect timeout"
        }
        "data source" | "addr" | "address" | "network address" | "server" => "data source",
        "host name in certificate" | "hostnameincertificate" | "hostname in certificate" => {
            "host name in certificate"
        }
        "initial catalog" | "database" | "databasename" => "initial catalog",
        "integrated security" | "trusted_connection" | "integratedsecurity" => {
            "integrated security"
        }
        "multi subnet failover" | "multisubnetfailover" => "multi subnet failover",
        "packet size" | "packetsize" => "packet size",
        "password" | "pwd" => "password",
        "trust server certificate" | "trustservercertificate" => "trust server certificate",
        "user id" | "uid" | "user" | "username" => "user id",
        "workstation id" | "workstationid" | "wsid" => "workstation id",
        _ => return None,
    })
}

/// Splits an ADO.NET connection string into its pairs, each stored under
/// `keyword(key)`. An error from `keyword` is returned as it is.
///
/// - Pairs are `key=value`, separated by `;`. Whitespace and `;` before a key
///   are skipped.
/// - A key runs to the first single `=`; `==` stands for a literal `=`, while
///   `;` and whitespace inside a key are kept. Keys are trimmed and must not
///   be empty.
/// - A value in `'` or `"` is kept as it is, with the quote doubled for a
///   literal quote. Only whitespace may follow it before the `;` or the end.
/// - Any other value runs to the next `;`, is trimmed and must not end in a
///   quote. An empty one removes the key.
/// - Control characters other than whitespace may appear only inside quotes.
/// - A NUL ends the string; only whitespace and further NULs may follow it.
/// - The last occurrence of a key wins.
///
/// A syntax error gives the byte index where the offending pair, or the
/// offending character, starts. Each value is copied once, into a
/// [`Zeroizing`] buffer that never grows.
fn parse(
    input: &str,
    keyword: impl Fn(&str) -> crate::Result<String>,
) -> crate::Result<HashMap<String, Zeroizing<String>>> {
    // What follows a NUL is padding, not pairs.
    let (text, padding) = input.split_at(input.find('\0').unwrap_or(input.len()));
    let mut pairs = HashMap::new();
    let mut cursor = Cursor { text, pos: 0 };

    while let Some(start) = cursor.next_pair() {
        let malformed = || malformed_at(start);
        let key = cursor.key().ok_or_else(malformed)?;
        cursor.skip_whitespace();
        let quote = cursor.peek().filter(|&c| c == '\'' || c == '"');
        let value = match quote {
            Some(quote) => Some(cursor.quoted(quote).ok_or_else(malformed)?),
            None => cursor.unquoted().ok_or_else(malformed)?,
        };
        let key = keyword(&key)?;
        // An unquoted value has already stopped at a `;`, the end or a stray
        // control character, which the next round rejects at its own index.
        if quote.is_some() && !cursor.at_pair_end() {
            return Err(malformed());
        }
        match value {
            Some(value) => pairs.insert(key, value),
            None => pairs.remove(&key),
        };
    }

    match padding.find(|c: char| c != '\0' && !c.is_whitespace()) {
        Some(offset) => Err(malformed_at(text.len() + offset)),
        None => Ok(pairs),
    }
}

fn malformed_at(index: usize) -> crate::Error {
    crate::Error::Conversion(
        format!("Connection string: invalid syntax in the text starting at index {index}").into(),
    )
}

/// A control character that is not whitespace: allowed only inside quotes.
fn is_stray_control(c: char) -> bool {
    c.is_control() && !c.is_whitespace()
}

/// A byte position in a connection string cut off before any NUL.
struct Cursor<'a> {
    text: &'a str,
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn rest(&self) -> &'a str {
        &self.text[self.pos..]
    }

    fn peek(&self) -> Option<char> {
        self.rest().chars().next()
    }

    fn bump(&mut self) -> Option<char> {
        let c = self.peek()?;
        self.pos += c.len_utf8();
        Some(c)
    }

    fn skip(&mut self, what: impl Fn(char) -> bool) {
        let rest = self.rest();
        self.pos += rest.len() - rest.trim_start_matches(what).len();
    }

    fn skip_whitespace(&mut self) {
        self.skip(char::is_whitespace);
    }

    /// Skips separators and returns where the next pair starts, if one does.
    fn next_pair(&mut self) -> Option<usize> {
        self.skip(|c| c == ';' || c.is_whitespace());
        (self.pos < self.text.len()).then_some(self.pos)
    }

    /// Reads a key and its `=`; the key comes back trimmed, with `==` as `=`.
    fn key(&mut self) -> Option<String> {
        let mut key = String::new();
        loop {
            match self.bump()? {
                '=' if self.peek() == Some('=') => {
                    self.pos += 1;
                    key.push('=');
                }
                '=' => break,
                c if is_stray_control(c) => return None,
                c => key.push(c),
            }
        }
        let key = key.trim();
        (!key.is_empty()).then(|| key.to_owned())
    }

    /// Reads a value from its opening `quote` through its closing one.
    fn quoted(&mut self, quote: char) -> Option<Zeroizing<String>> {
        self.pos += quote.len_utf8();
        let body = self.rest();
        // The quote is ASCII, so a byte search can't land inside a character.
        let q = quote as u8;
        let bytes = body.as_bytes();
        let mut i = 0;
        let end = loop {
            match bytes.get(i) {
                None => return None,
                Some(&b) if b == q && bytes.get(i + 1) == Some(&q) => i += 2,
                Some(&b) if b == q => break i,
                Some(_) => i += 1,
            }
        };
        let mut value = Zeroizing::new(String::with_capacity(end));
        let mut chars = body[..end].chars();
        while let Some(c) = chars.next() {
            value.push(c);
            if c == quote {
                // The second of a doubled quote.
                chars.next();
            }
        }
        self.pos += end + quote.len_utf8();
        Some(value)
    }

    /// Reads an unquoted value up to the next `;` or stray control character.
    /// `Some(None)` is an empty value, which removes the key; `None` is a
    /// malformed one.
    fn unquoted(&mut self) -> Option<Option<Zeroizing<String>>> {
        let rest = self.rest();
        let len = rest
            .find(|c| c == ';' || is_stray_control(c))
            .unwrap_or(rest.len());
        self.pos += len;
        let value = rest[..len].trim();
        if value.is_empty() {
            // Nothing, so the pair must end here.
            return matches!(self.peek(), None | Some(';')).then_some(None);
        }
        if value.ends_with(['\'', '"']) {
            return None;
        }
        Some(Some(Zeroizing::new(value.to_owned())))
    }

    /// Skips whitespace and tells whether the pair ends there.
    fn at_pair_end(&mut self) -> bool {
        self.skip_whitespace();
        matches!(self.peek(), None | Some(';'))
    }
}

impl ConfigString for AdoNetConfig {
    fn get(&self, key: &str) -> Option<&str> {
        self.dict.get(key).map(|v| v.as_str())
    }

    /// `Connect Timeout` and its synonyms: seconds, 0 for none, 15 by default
    /// (SqlClient).
    fn connect_timeout(&self) -> crate::Result<Option<Duration>> {
        match self.get("connect timeout") {
            None => Ok(Some(super::DEFAULT_CONNECT_TIMEOUT)),
            Some(v) => match v.trim().parse::<i32>() {
                Ok(0) => Ok(None),
                Ok(secs) if secs > 0 => Ok(Some(Duration::from_secs(secs as u64))),
                _ => Err(crate::Error::Conversion(
                    "Connection string: Connect Timeout must be an integer from 0 to 2147483647"
                        .into(),
                )),
            },
        }
    }

    /// `Packet Size`: 512 to 32768 bytes (SqlConnectionOptions,
    /// TdsEnums.MIN_PACKET_SIZE/MAX_PACKET_SIZE).
    fn packet_size(&self) -> crate::Result<Option<u32>> {
        self.get("packet size")
            .map(|v| match v.trim().parse::<u32>() {
                Ok(size @ 512..=32768) => Ok(size),
                _ => Err(crate::Error::Conversion(
                    "Connection string: Packet Size must be from 512 to 32768".into(),
                )),
            })
            .transpose()
    }

    /// `Data Source` as SqlClient's managed SNI splits it (`DataSource` in
    /// SniProxy.netcore.cs): an optional protocol prefix, case-insensitive,
    /// then `host[\instance][,port]`, every part trimmed.
    fn server(&self) -> crate::Result<ServerDefinition> {
        let Some(value) = self.get("data source") else {
            return Ok(ServerDefinition {
                host: None,
                port: None,
                instance: None,
            });
        };
        let unsupported = |what: &str| {
            Err(crate::Error::Conversion(
                format!("Connection string: {what} is not supported; use tcp:").into(),
            ))
        };

        let value = match value.split_once(':') {
            Some((protocol, rest)) => match protocol.trim().to_ascii_lowercase().as_str() {
                "tcp" => rest,
                "np" => return unsupported("named pipes (np:)"),
                "lpc" => return unsupported("shared memory (lpc:)"),
                "admin" => return unsupported("the dedicated admin connection (admin:)"),
                // Not a protocol, such as an IPv6 address: all of it is the host.
                _ => value,
            },
            None => value,
        };
        if value.trim_start().starts_with(r"\\") {
            return unsupported("a named pipe path");
        }

        let mut parts = value.split(',');
        let mut host = parts.next().unwrap_or_default().split('\\');
        let port = parts.next().map(|p| p.trim().parse()).transpose()?;
        if parts.next().is_some() {
            return Err(crate::Error::Conversion("Server value faulty.".into()));
        }

        Ok(ServerDefinition {
            host: host
                .next()
                .map(|h| h.trim().replace("(local)", "localhost")),
            port,
            instance: host.next().map(|i| i.trim().to_owned()),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::AuthMethod;

    #[cfg(any(
        feature = "rustls",
        feature = "native-tls",
        feature = "vendored-openssl"
    ))]
    use crate::EncryptionLevel;

    #[test]
    fn server_parsing_no_browser() -> crate::Result<()> {
        let test_str = "server=tcp:my-server.com,4200";
        let ado: AdoNetConfig = test_str.parse()?;
        let server = ado.server()?;

        assert_eq!(Some("my-server.com".to_string()), server.host);
        assert_eq!(Some(4200), server.port);
        assert_eq!(None, server.instance);

        let test_str = "data source=tcp:my-server.com,4200";
        let ado: AdoNetConfig = test_str.parse()?;
        let server = ado.server()?;

        assert_eq!(Some("my-server.com".to_string()), server.host);
        assert_eq!(Some(4200), server.port);
        assert_eq!(None, server.instance);

        Ok(())
    }

    #[test]
    fn server_parsing_no_tcp() -> crate::Result<()> {
        let test_str = "server=my-server.com,4200";
        let ado: AdoNetConfig = test_str.parse()?;
        let server = ado.server()?;

        assert_eq!(Some("my-server.com".to_string()), server.host);
        assert_eq!(Some(4200), server.port);
        assert_eq!(None, server.instance);

        let test_str = "data source=my-server.com,4200";
        let ado: AdoNetConfig = test_str.parse()?;
        let server = ado.server()?;

        assert_eq!(Some("my-server.com".to_string()), server.host);
        assert_eq!(Some(4200), server.port);
        assert_eq!(None, server.instance);

        Ok(())
    }

    #[test]
    fn server_parsing_local() -> crate::Result<()> {
        let test_str = "server=tcp:(local),4200";
        let ado: AdoNetConfig = test_str.parse()?;
        let server = ado.server()?;

        assert_eq!(Some("localhost".to_string()), server.host);
        assert_eq!(Some(4200), server.port);
        assert_eq!(None, server.instance);

        let test_str = "data source=tcp:(local),4200";
        let ado: AdoNetConfig = test_str.parse()?;
        let server = ado.server()?;

        assert_eq!(Some("localhost".to_string()), server.host);
        assert_eq!(Some(4200), server.port);
        assert_eq!(None, server.instance);

        Ok(())
    }

    #[test]
    fn server_parsing_local_no_tcp() -> crate::Result<()> {
        let test_str = "server=(local),4200";
        let ado: AdoNetConfig = test_str.parse()?;
        let server = ado.server()?;

        assert_eq!(Some("localhost".to_string()), server.host);
        assert_eq!(Some(4200), server.port);
        assert_eq!(None, server.instance);

        let test_str = "data source=(local),4200";
        let ado: AdoNetConfig = test_str.parse()?;
        let server = ado.server()?;

        assert_eq!(Some("localhost".to_string()), server.host);
        assert_eq!(Some(4200), server.port);
        assert_eq!(None, server.instance);

        Ok(())
    }

    #[test]
    fn server_parsing_no_port() -> crate::Result<()> {
        let test_str = "server=tcp:my-server.com";
        let ado: AdoNetConfig = test_str.parse()?;
        let server = ado.server()?;

        assert_eq!(Some("my-server.com".to_string()), server.host);
        assert_eq!(None, server.port);
        assert_eq!(None, server.instance);

        let test_str = "server=my-server.com";
        let ado: AdoNetConfig = test_str.parse()?;
        let server = ado.server()?;

        assert_eq!(Some("my-server.com".to_string()), server.host);
        assert_eq!(None, server.port);
        assert_eq!(None, server.instance);

        Ok(())
    }

    #[test]
    fn server_parsing_with_browser() -> crate::Result<()> {
        let test_str = "server=tcp:my-server.com\\TIBERIUS";
        let ado: AdoNetConfig = test_str.parse()?;
        let server = ado.server()?;

        assert_eq!(Some("my-server.com".to_string()), server.host);
        assert_eq!(None, server.port);
        assert_eq!(Some("TIBERIUS".to_string()), server.instance);

        let test_str = "data source=tcp:my-server.com\\TIBERIUS";
        let ado: AdoNetConfig = test_str.parse()?;
        let server = ado.server()?;

        assert_eq!(Some("my-server.com".to_string()), server.host);
        assert_eq!(None, server.port);
        assert_eq!(Some("TIBERIUS".to_string()), server.instance);

        Ok(())
    }

    #[test]
    fn server_parsing_with_browser_and_port() -> crate::Result<()> {
        let test_str = "server=tcp:my-server.com\\TIBERIUS,666";
        let ado: AdoNetConfig = test_str.parse()?;
        let server = ado.server()?;

        assert_eq!(Some("my-server.com".to_string()), server.host);
        assert_eq!(Some(666), server.port);
        assert_eq!(Some("TIBERIUS".to_string()), server.instance);

        let test_str = "data source=tcp:my-server.com\\TIBERIUS,666";
        let ado: AdoNetConfig = test_str.parse()?;
        let server = ado.server()?;

        assert_eq!(Some("my-server.com".to_string()), server.host);
        assert_eq!(Some(666), server.port);
        assert_eq!(Some("TIBERIUS".to_string()), server.instance);

        Ok(())
    }

    #[test]
    fn server_parsing_too_many_parts_is_error() -> crate::Result<()> {
        // The Server value must have at most two comma-separated parts
        // (host[,port]). Three parts is invalid and must error. The guard is
        // `parts.is_empty() || parts.len() >= 3`; a `&&` mutation would never
        // trigger (a slice cannot be both empty and have >= 3 parts), so this
        // three-part value would be wrongly accepted.
        let ado: AdoNetConfig = "server=tcp:my-server.com,1433,extra".parse()?;
        assert!(ado.server().is_err());

        let ado: AdoNetConfig = "server=my-server.com,1433,extra".parse()?;
        assert!(ado.server().is_err());

        Ok(())
    }

    #[test]
    fn server_parsing_missing_key() -> crate::Result<()> {
        // No `server`/`data source` key at all -> an all-`None` definition.
        let ado: AdoNetConfig = "database=Foo".parse()?;
        let server = ado.server()?;

        assert_eq!(None, server.host);
        assert_eq!(None, server.port);
        assert_eq!(None, server.instance);

        // And the same path through the public constructor.
        let config = crate::Config::from_ado_string("database=Foo")?;
        assert_eq!("localhost", config.get_host());

        Ok(())
    }

    #[test]
    fn database_parsing() -> crate::Result<()> {
        let test_str = "database=Foo";
        let ado: AdoNetConfig = test_str.parse()?;

        assert_eq!(Some("Foo".to_string()), ado.database());

        let test_str = "databaseName=Foo";
        let jdbc: AdoNetConfig = test_str.parse()?;

        assert_eq!(Some("Foo".to_string()), jdbc.database());

        let test_str = "Initial Catalog=Foo";
        let jdbc: AdoNetConfig = test_str.parse()?;

        assert_eq!(Some("Foo".to_string()), jdbc.database());

        Ok(())
    }

    #[test]
    fn trust_cert_parsing_true() -> crate::Result<()> {
        let test_str = "TrustServerCertificate=true";
        let ado: AdoNetConfig = test_str.parse()?;

        assert!(ado.trust_cert()?);

        Ok(())
    }

    #[test]
    fn trust_cert_parsing_false() -> crate::Result<()> {
        let test_str = "TrustServerCertificate=false";
        let ado: AdoNetConfig = test_str.parse()?;

        assert!(!ado.trust_cert()?);

        Ok(())
    }

    #[test]
    fn trust_cert_parsing_yes() -> crate::Result<()> {
        let test_str = "TrustServerCertificate=yes";
        let ado: AdoNetConfig = test_str.parse()?;

        assert!(ado.trust_cert()?);

        Ok(())
    }

    #[test]
    fn trust_cert_parsing_no() -> crate::Result<()> {
        let test_str = "TrustServerCertificate=no";
        let ado: AdoNetConfig = test_str.parse()?;

        assert!(!ado.trust_cert()?);

        Ok(())
    }

    #[test]
    fn trust_cert_parsing_missing() -> crate::Result<()> {
        let test_str = "Something=foo;";
        let ado: AdoNetConfig = test_str.parse()?;

        assert!(!ado.trust_cert()?);

        Ok(())
    }

    #[test]
    fn trust_cert_parsing_faulty() -> crate::Result<()> {
        let test_str = "TrustServerCertificate=musti;";
        let ado: AdoNetConfig = test_str.parse()?;

        assert!(ado.trust_cert().is_err());

        Ok(())
    }

    #[test]
    fn trust_cert_ca_parsing_ok() -> crate::Result<()> {
        let test_str = "TrustServerCertificateCA=someca.crt;";
        let ado: AdoNetConfig = test_str.parse()?;

        assert_eq!(Some("someca.crt".to_string()), ado.trust_cert_ca());

        Ok(())
    }

    #[test]
    fn parsing_sql_server_authentication() -> crate::Result<()> {
        let test_str = "uid=Musti; pwd=Naukio;";
        let ado: AdoNetConfig = test_str.parse()?;

        assert_eq!(
            AuthMethod::sql_server("Musti", "Naukio"),
            ado.authentication()?
        );

        Ok(())
    }

    #[test]
    #[cfg(all(windows, feature = "winauth"))]
    fn parsing_sspi_authentication() -> crate::Result<()> {
        let test_str = "IntegratedSecurity=SSPI;";
        let ado: AdoNetConfig = test_str.parse()?;

        assert_eq!(AuthMethod::Integrated, ado.authentication()?);

        let test_str = "Integrated Security=SSPI;";
        let ado: AdoNetConfig = test_str.parse()?;

        assert_eq!(AuthMethod::Integrated, ado.authentication()?);

        Ok(())
    }

    #[test]
    #[cfg(all(feature = "integrated-auth-gssapi", unix))]
    fn parsing_sspi_authentication() -> crate::Result<()> {
        let test_str = "IntegratedSecurity=true;";
        let ado: AdoNetConfig = test_str.parse()?;

        assert_eq!(AuthMethod::Integrated, ado.authentication()?);

        let test_str = "Integrated Security=true;";
        let ado: AdoNetConfig = test_str.parse()?;

        assert_eq!(AuthMethod::Integrated, ado.authentication()?);

        Ok(())
    }

    #[test]
    #[cfg(all(windows, feature = "winauth"))]
    fn parsing_windows_authentication() -> crate::Result<()> {
        let test_str = "uid=Musti;pwd=Naukio; IntegratedSecurity=SSPI;";
        let ado: AdoNetConfig = test_str.parse()?;

        assert_eq!(
            AuthMethod::windows("Musti", "Naukio"),
            ado.authentication()?
        );

        let test_str = "uid=Musti;pwd=Naukio; Integrated Security=SSPI;";
        let ado: AdoNetConfig = test_str.parse()?;

        assert_eq!(
            AuthMethod::windows("Musti", "Naukio"),
            ado.authentication()?
        );

        Ok(())
    }

    #[test]
    fn parsing_database() -> crate::Result<()> {
        let test_str = "database=Cats;";
        let ado: AdoNetConfig = test_str.parse()?;

        assert_eq!(Some("Cats".to_string()), ado.database());

        Ok(())
    }

    #[test]
    fn parsing_login_credentials_escaping() -> crate::Result<()> {
        let test_str = "User ID=musti; Password='abc;}45';";
        let ado: AdoNetConfig = test_str.parse()?;

        assert_eq!(
            AuthMethod::sql_server("musti", "abc;}45"),
            ado.authentication()?
        );

        Ok(())
    }

    #[test]
    #[cfg(any(
        feature = "rustls",
        feature = "native-tls",
        feature = "vendored-openssl"
    ))]
    fn encryption_parsing_on() -> crate::Result<()> {
        let test_str = "encrypt=true";
        let ado: AdoNetConfig = test_str.parse()?;

        assert_eq!(EncryptionLevel::Required, ado.encrypt()?);

        Ok(())
    }

    #[test]
    #[cfg(any(
        feature = "rustls",
        feature = "native-tls",
        feature = "vendored-openssl"
    ))]
    fn encryption_parsing_off() -> crate::Result<()> {
        let test_str = "encrypt=false";
        let ado: AdoNetConfig = test_str.parse()?;

        assert_eq!(EncryptionLevel::Off, ado.encrypt()?);

        Ok(())
    }

    #[test]
    #[cfg(any(
        feature = "rustls",
        feature = "native-tls",
        feature = "vendored-openssl"
    ))]
    fn encryption_parsing_plaintext() -> crate::Result<()> {
        let test_str = "encrypt=DANGER_PLAINTEXT";
        let ado: AdoNetConfig = test_str.parse()?;

        assert_eq!(EncryptionLevel::NotSupported, ado.encrypt()?);

        Ok(())
    }

    #[test]
    #[cfg(any(
        feature = "rustls",
        feature = "native-tls",
        feature = "vendored-openssl"
    ))]
    fn encryption_parsing_missing() -> crate::Result<()> {
        let test_str = "";
        let ado: AdoNetConfig = test_str.parse()?;

        assert_eq!(EncryptionLevel::Required, ado.encrypt()?);

        Ok(())
    }

    #[test]
    #[cfg(feature = "tds80")]
    fn encryption_parsing_strict() -> crate::Result<()> {
        let test_str = "encrypt=strict";
        let ado: AdoNetConfig = test_str.parse()?;

        assert_eq!(EncryptionLevel::Strict, ado.encrypt()?);

        Ok(())
    }

    #[test]
    fn client_name_parsing() -> crate::Result<()> {
        let test_str = "workstationid=meow";
        let ado: AdoNetConfig = test_str.parse()?;

        assert_eq!(Some("meow".into()), ado.client_name());

        let test_str = "Workstation ID=meow";
        let ado: AdoNetConfig = test_str.parse()?;

        assert_eq!(Some("meow".into()), ado.client_name());

        Ok(())
    }

    #[test]
    fn hostname_in_certificate_parsing() -> crate::Result<()> {
        let test_str = "HostNameInCertificate=foo.example.com";
        let ado: AdoNetConfig = test_str.parse()?;

        assert_eq!(
            Some("foo.example.com".into()),
            ado.hostname_in_certificate()
        );

        let test_str = "HostName In Certificate=foo.example.com";
        let ado: AdoNetConfig = test_str.parse()?;

        assert_eq!(
            Some("foo.example.com".into()),
            ado.hostname_in_certificate()
        );

        Ok(())
    }

    #[test]
    fn application_name_parsing() -> crate::Result<()> {
        let test_str = "Application Name=meow";
        let ado: AdoNetConfig = test_str.parse()?;

        assert_eq!(Some("meow".into()), ado.application_name());

        let test_str = "ApplicationName=meow";
        let ado: AdoNetConfig = test_str.parse()?;

        assert_eq!(Some("meow".into()), ado.application_name());

        Ok(())
    }

    #[test]
    fn application_intent_readonly_parsing() -> crate::Result<()> {
        // Exact spelling from the ADO.NET connection string.
        let ado: AdoNetConfig = "ApplicationIntent=ReadOnly".parse()?;
        assert!(ado.readonly());

        // ADO.NET treats the value case-insensitively.
        let ado: AdoNetConfig = "applicationintent=readonly".parse()?;
        assert!(ado.readonly());

        // ReadWrite (the default) must not request read-only intent.
        let ado: AdoNetConfig = "ApplicationIntent=ReadWrite".parse()?;
        assert!(!ado.readonly());

        // Absent altogether.
        let ado: AdoNetConfig = "server=tcp:localhost,1433".parse()?;
        assert!(!ado.readonly());

        Ok(())
    }

    #[test]
    fn multi_subnet_failover_parsing() -> crate::Result<()> {
        let test_str = "MultiSubnetFailover=true";
        let ado: AdoNetConfig = test_str.parse()?;
        assert!(ado.multi_subnet_failover()?);

        let test_str = "MultiSubnetFailover=yes";
        let ado: AdoNetConfig = test_str.parse()?;
        assert!(ado.multi_subnet_failover()?);

        let test_str = "MultiSubnetFailover=false";
        let ado: AdoNetConfig = test_str.parse()?;
        assert!(!ado.multi_subnet_failover()?);

        Ok(())
    }

    #[test]
    fn multi_subnet_failover_parsing_missing() -> crate::Result<()> {
        let test_str = "";
        let ado: AdoNetConfig = test_str.parse()?;
        assert!(!ado.multi_subnet_failover()?);

        Ok(())
    }

    #[test]
    fn multi_subnet_failover_from_ado_string() -> crate::Result<()> {
        let config = crate::Config::from_ado_string(
            "server=tcp:my-server.com,1433;MultiSubnetFailover=true",
        )?;
        assert!(config.get_multi_subnet_failover());

        let config = crate::Config::from_ado_string("server=tcp:my-server.com,1433")?;
        assert!(!config.get_multi_subnet_failover());

        Ok(())
    }

    /// The value `key` gets from the key/value grammar alone.
    fn value(s: &str, key: &str) -> Option<String> {
        let pairs = raw(s).unwrap();
        pairs.get(key).map(|v| v.as_str().to_owned())
    }

    fn raw(s: &str) -> crate::Result<HashMap<String, Zeroizing<String>>> {
        parse(s, |key| Ok(key.to_lowercase()))
    }

    #[test]
    fn doubled_quotes_are_one_literal_quote() {
        assert_eq!(value(r#"pwd="a""b";"#, "pwd").as_deref(), Some(r#"a"b"#));
        assert_eq!(value("pwd='a''b'", "pwd").as_deref(), Some("a'b"));
        assert_eq!(value(r#"pwd='a"b;c'"#, "pwd").as_deref(), Some(r#"a"b;c"#));
        assert_eq!(value(r#"pwd="""x""""#, "pwd").as_deref(), Some(r#""x""#));
        assert_eq!(value("pwd=''", "pwd").as_deref(), Some(""));
    }

    #[test]
    fn non_ascii_values_are_accepted() {
        assert_eq!(
            value("password=pässwörd€😀", "password").as_deref(),
            Some("pässwörd€😀")
        );
        assert_eq!(value("password='ü;ß'", "password").as_deref(), Some("ü;ß"));
        assert_eq!(value("Kennwört=x", "kennwört").as_deref(), Some("x"));
    }

    #[test]
    fn whitespace_before_a_key_is_skipped() {
        for sep in ["\t", "\r", "\n", "\r\n", " \t "] {
            let s = format!("server=x;{sep}password=secret;{sep}user=sa");
            assert_eq!(value(&s, "password").as_deref(), Some("secret"), "{sep:?}");
            assert_eq!(value(&s, "user").as_deref(), Some("sa"), "{sep:?}");
        }
    }

    #[test]
    fn unparsable_strings_are_errors() {
        for s in [
            "password",            // no `=`
            "=x",                  // empty key
            "password='abc",       // unclosed quote
            "password=\"abc",      // unclosed quote
            "password='a'b",       // text after a quoted value
            "password=abc'",       // unquoted value ending in a quote
            "password=abc\"",      // ditto
            "pass\u{1}word=x",     // control character in a key
            "server=x;\u{7}pwd=y", // control character before a key
        ] {
            assert!(raw(s).is_err(), "{s:?} parsed");
        }
    }

    #[test]
    fn sqlclient_key_value_rules() {
        // Last occurrence wins; an empty value unsets the key.
        assert_eq!(value("pwd=a;pwd=b", "pwd").as_deref(), Some("b"));
        assert_eq!(value("pwd=a;pwd=", "pwd"), None);
        assert_eq!(value("pwd=a;pwd= ;", "pwd"), None);
        // `==` escapes `=` in a key; `=` in a value needs no escaping.
        assert_eq!(value("a==b=c", "a=b").as_deref(), Some("c"));
        assert_eq!(value("pwd=a=b", "pwd").as_deref(), Some("a=b"));
        // Unquoted values are trimmed, inner whitespace kept; keys too.
        assert_eq!(
            value("  User  ID  =  a b  ;", "user  id").as_deref(),
            Some("a b")
        );
        // Quoted values keep their whitespace; quotes inside unquoted values
        // are literal.
        assert_eq!(value("pwd=' a '", "pwd").as_deref(), Some(" a "));
        assert_eq!(
            value("Data Source=my'Server", "data source").as_deref(),
            Some("my'Server")
        );
        // Trailing NULs and separators are accepted.
        assert_eq!(value("pwd=x;;\0\0", "pwd").as_deref(), Some("x"));
    }

    #[test]
    #[cfg(any(
        feature = "rustls",
        feature = "native-tls",
        feature = "vendored-openssl"
    ))]
    fn encrypt_accepts_sqlclient_5_values() -> crate::Result<()> {
        for (v, level) in [
            ("Mandatory", EncryptionLevel::Required),
            (" mandatory ", EncryptionLevel::Required),
            ("TRUE", EncryptionLevel::Required),
            ("Optional", EncryptionLevel::Off),
            ("No", EncryptionLevel::Off),
        ] {
            let ado: AdoNetConfig = format!("encrypt='{v}'").parse()?;
            assert_eq!(level, ado.encrypt()?, "{v}");
        }
        let ado: AdoNetConfig = "encrypt=sometimes".parse()?;
        assert!(ado.encrypt().is_err());
        Ok(())
    }

    #[test]
    fn connect_timeout_parsing() -> crate::Result<()> {
        use std::time::Duration;
        let t = |s: &str| s.parse::<AdoNetConfig>().unwrap().connect_timeout();
        assert_eq!(t("")?, Some(Duration::from_secs(15)));
        assert_eq!(t("Connect Timeout=30")?, Some(Duration::from_secs(30)));
        assert_eq!(t("connection timeout= 5 ")?, Some(Duration::from_secs(5)));
        assert_eq!(t("Timeout=7")?, Some(Duration::from_secs(7)));
        assert_eq!(t("Connect Timeout=0")?, None);
        assert!(t("Connect Timeout=-1").is_err());
        assert!(t("Connect Timeout=soon").is_err());
        assert!(t("Connect Timeout=2147483648").is_err());
        Ok(())
    }

    fn config(s: &str) -> crate::Config {
        crate::Config::from_ado_string(s).unwrap_or_else(|e| panic!("{s:?}: {e}"))
    }

    #[test]
    fn sqlclient_keywords_and_synonyms() {
        for key in [
            "Addr",
            "Address",
            "Network Address",
            "Server",
            "Data Source",
        ] {
            assert_eq!(config(&format!("{key}=h,1500")).get_addr(), "h:1500");
        }
        let c = config("App=a;WSID=w;Packet Size=8192");
        assert_eq!(c.application_name.as_deref(), Some("a"));
        assert_eq!(c.client_name.as_deref(), Some("w"));
        assert_eq!(c.get_packet_size(), Some(8192));
        assert_eq!(config("packetsize=512").get_packet_size(), Some(512));
        assert_eq!(config("").get_packet_size(), None);

        let c = config(
            "Application Intent=ReadOnly;Multi Subnet Failover=true;\
             Host Name In Certificate=cert.host;connecttimeout=5",
        );
        assert!(c.readonly && c.multi_subnet_failover);
        assert_eq!(c.hostname_in_certificate.as_deref(), Some("cert.host"));
        assert_eq!(c.connect_timeout(), Some(Duration::from_secs(5)));
        assert!(matches!(
            config("Trust Server Certificate=true").trust,
            super::super::TrustConfig::TrustAll
        ));
        assert!(matches!(
            config("Trusted_Connection=no;uid=u").auth,
            AuthMethod::SqlServer(_)
        ));

        for bad in [
            "Packet Size=511",
            "Packet Size=32769",
            "Packet Size=big",
            "Trusted_Connection=maybe",
        ] {
            assert!(crate::Config::from_ado_string(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn last_of_synonyms_wins() {
        assert_eq!(config("Server=a;Data Source=b").get_host(), "b");
        assert_eq!(config("Data Source=b;Addr=a").get_host(), "a");
        assert_eq!(config("Server=a;Address=").get_host(), "localhost");
        assert_eq!(
            config("uid=a;User ID=b;pwd=x;Password=y").auth,
            AuthMethod::sql_server("b", "y")
        );
        assert_eq!(
            config("User=a;uid=b;Password=x;pwd=y").auth,
            AuthMethod::sql_server("b", "y")
        );
        assert_eq!(
            config("Initial Catalog=a;Database=b").database.as_deref(),
            Some("b")
        );
        assert_eq!(
            config("Application Name=a;App=b")
                .application_name
                .as_deref(),
            Some("b")
        );
        assert_eq!(
            config("WSID=a;Workstation ID=b").client_name.as_deref(),
            Some("b")
        );
        assert_eq!(
            config("Timeout=1;Connect Timeout=2").connect_timeout(),
            Some(Duration::from_secs(2))
        );
    }

    #[test]
    fn protocol_prefix_is_case_insensitive_and_parts_are_trimmed() {
        for s in [
            "server=TCP:h,1433",
            "server=Tcp:h,1433",
            "server= tcp : h , 1433 ",
            "server=h , 1433",
        ] {
            let server = s.parse::<AdoNetConfig>().unwrap().server().unwrap();
            assert_eq!(server.host.as_deref(), Some("h"), "{s}");
            assert_eq!(server.port, Some(1433), "{s}");
        }
        let server = r"server=tcp: h \ INST "
            .parse::<AdoNetConfig>()
            .unwrap()
            .server()
            .unwrap();
        assert_eq!(server.host.as_deref(), Some("h"));
        assert_eq!(server.instance.as_deref(), Some("INST"));

        for s in [
            "server=np:h",
            r"server=NP:h\INST",
            "server=lpc:h",
            "server=admin:h",
            r"server=\\h\pipe\sql\query",
        ] {
            match crate::Config::from_ado_string(s) {
                Err(crate::Error::Conversion(msg)) => {
                    assert!(msg.contains("not supported"), "{s}: {msg}")
                }
                other => panic!("{s}: {other:?}"),
            }
        }
    }

    /// Without an integrated-auth backend, `Integrated Security` used to log
    /// in with SQL authentication as user "".
    #[test]
    #[cfg(not(any(
        all(windows, feature = "winauth"),
        all(unix, any(feature = "integrated-auth-gssapi", feature = "sspi-rs"))
    )))]
    fn integrated_security_without_a_backend_is_an_error() {
        for s in [
            "IntegratedSecurity=SSPI",
            "Integrated Security=true",
            "Trusted_Connection=yes;uid=a;pwd=b",
        ] {
            match crate::Config::from_ado_string(s) {
                Err(crate::Error::Conversion(msg)) => {
                    #[cfg(windows)]
                    assert!(msg.contains("`winauth`"), "{msg}");
                    #[cfg(unix)]
                    assert!(msg.contains("`integrated-auth-gssapi`"), "{msg}");
                }
                other => panic!("{s}: {other:?}"),
            }
        }
        assert!(matches!(
            config("Integrated Security=false;uid=a").auth,
            AuthMethod::SqlServer(_)
        ));
    }
}
