use super::{ConfigString, ServerDefinition};
use std::{collections::HashMap, str::FromStr};

/// An ADO.NET connection string, split with SqlClient's rules.
pub(crate) struct AdoNetConfig {
    dict: HashMap<String, String>,
}

impl FromStr for AdoNetConfig {
    type Err = crate::error::Error;

    fn from_str(s: &str) -> crate::Result<Self> {
        Ok(Self { dict: parse(s)? })
    }
}

/// Splits an ADO.NET connection string into its pairs, keys lowercased.
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
/// offending character, starts.
fn parse(input: &str) -> crate::Result<HashMap<String, String>> {
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
        let key = key.to_lowercase();
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
    fn quoted(&mut self, quote: char) -> Option<String> {
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
        let mut value = String::with_capacity(end);
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
    fn unquoted(&mut self) -> Option<Option<String>> {
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
        Some(Some(value.to_owned()))
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

    fn server(&self) -> crate::Result<ServerDefinition> {
        fn parse_port(parts: &[&str]) -> crate::Result<Option<u16>> {
            Ok(match parts.first() {
                Some(s) => Some(s.parse()?),
                None => None,
            })
        }

        fn parse_server(parts: Vec<&str>) -> crate::Result<ServerDefinition> {
            if parts.is_empty() || parts.len() >= 3 {
                return Err(crate::Error::Conversion("Server value faulty.".into()));
            }

            let definition = if parts[0].contains('\\') {
                let port = parse_port(&parts[1..])?;
                let parts: Vec<&str> = parts[0].split('\\').collect();

                ServerDefinition {
                    host: Some(parts[0].replace("(local)", "localhost")),
                    port,
                    instance: Some(parts[1].into()),
                }
            } else {
                // Connect using a TCP target
                ServerDefinition {
                    host: Some(parts[0].replace("(local)", "localhost")),
                    port: parse_port(&parts[1..])?,
                    instance: None,
                }
            };

            Ok(definition)
        }

        match self.first(&["server", "data source"]) {
            Some(value) if value.starts_with("tcp:") => {
                parse_server(value[4..].split(',').collect())
            }
            Some(value) => parse_server(value.split(',').collect()),
            None => Ok(ServerDefinition {
                host: None,
                port: None,
                instance: None,
            }),
        }
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

    fn value(s: &str, key: &str) -> Option<String> {
        let ado: AdoNetConfig = s.parse().unwrap();
        ado.get(key).map(str::to_owned)
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
            assert!(s.parse::<AdoNetConfig>().is_err(), "{s:?} parsed");
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
}
