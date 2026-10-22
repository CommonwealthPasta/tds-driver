use super::{ConfigString, ServerDefinition};
use crate::error::Error;
use std::{collections::HashMap, str::FromStr, time::Duration};
use zeroize::Zeroizing;

/// `loginTimeout` default of mssql-jdbc 11.2 and later.
const JDBC_DEFAULT_LOGIN_TIMEOUT: Duration = Duration::from_secs(30);

/// A JDBC URL, split with mssql-jdbc's rules.
pub(crate) struct JdbcConfig {
    dict: HashMap<String, Zeroizing<String>>,
}

impl FromStr for JdbcConfig {
    type Err = Error;

    fn from_str(s: &str) -> crate::Result<Self> {
        Ok(Self { dict: parse(s)? })
    }
}

/// mssql-jdbc's name for a lowercase property name.
fn property(name: &str) -> String {
    name.to_owned()
}

/// Java's `String.trim`.
fn trim(s: &str) -> &str {
    s.trim_matches(|c: char| c <= ' ')
}

/// Splits a `jdbc:sqlserver://` URL into its properties, each stored under
/// [`property`] of its lowercase name.
///
/// - The prefix is matched without regard to ASCII case.
/// - Then comes `serverName[\instanceName][:portNumber]`, up to the first
///   `;`. The server name may be an IPv6 address in `[...]`, which may hold
///   any character; outside brackets the name's first character is always
///   taken as part of it. The parts are trimmed, stored unless empty and must
///   not contain `=`. A port, once its `:` is there, must be ASCII digits
///   with a value of at most 65535.
/// - Then `;name=value` properties. A name runs to the `=` and is trimmed; it
///   must not be empty. A segment holding only whitespace is skipped.
/// - A value runs to the next `;` and is trimmed, and must not contain `{`,
///   unless it starts with `{` after whitespace. Then it is kept as it is up
///   to the matching `}`, where `}}` stands for a literal `}`, and only
///   spaces may follow the brace before the `;` or the end.
/// - The last occurrence of a name wins.
///
/// Each value is copied once, into a [`Zeroizing`] buffer that never grows.
fn parse(url: &str) -> crate::Result<HashMap<String, Zeroizing<String>>> {
    const PREFIX: &str = "jdbc:sqlserver://";
    match url.get(..PREFIX.len()) {
        Some(prefix) if prefix.eq_ignore_ascii_case(PREFIX) => (),
        _ => {
            return Err(Error::Conversion(
                format!("Connection string: a JDBC URL must begin with {PREFIX}").into(),
            ))
        }
    }

    let mut props = HashMap::new();
    let mut cursor = Cursor {
        url,
        pos: PREFIX.len(),
    };
    cursor.head(&mut props)?;
    while cursor.eat(';') {
        cursor.property(&mut props)?;
    }
    Ok(props)
}

fn malformed_at(index: usize) -> Error {
    Error::Conversion(format!("Connection string: malformed JDBC URL at index {index}").into())
}

/// A byte position in a JDBC URL.
struct Cursor<'a> {
    url: &'a str,
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn rest(&self) -> &'a str {
        &self.url[self.pos..]
    }

    fn peek(&self) -> Option<char> {
        self.rest().chars().next()
    }

    fn eat(&mut self, c: char) -> bool {
        let found = self.peek() == Some(c);
        if found {
            self.pos += c.len_utf8();
        }
        found
    }

    /// Moves to the next character in `stops`, or the end, and returns the
    /// text passed over.
    fn take_until(&mut self, stops: &[char]) -> &'a str {
        let rest = self.rest();
        let len = rest.find(stops).unwrap_or(rest.len());
        self.pos += len;
        &rest[..len]
    }

    /// Reads `serverName[\instanceName][:portNumber]`, up to the first `;`.
    fn head(&mut self, props: &mut HashMap<String, Zeroizing<String>>) -> crate::Result<()> {
        let start = self.pos;
        match self.peek() {
            // A bracketed IPv6 address; its `:`s (and anything else) are its own.
            Some('[') => match self.rest().find(']') {
                Some(close) => self.pos += close + 1,
                None => return Err(malformed_at(self.url.len())),
            },
            Some(';') | None => (),
            // The first character belongs to the server name even if it is
            // `\` or `:`.
            Some(first) => self.pos += first.len_utf8(),
        }
        self.take_until(&['\\', ':', ';']);
        let server = &self.url[start..self.pos];
        let instance = if self.eat('\\') {
            self.take_until(&[':', ';'])
        } else {
            ""
        };
        let port = if self.eat(':') {
            Some(trim(self.take_until(&[';'])))
        } else {
            None
        };

        for (name, part) in [("servername", server), ("instancename", instance)] {
            let part = trim(part);
            if part.contains('=') {
                return Err(Error::Conversion(
                    format!("Connection string: the {name} in a JDBC URL must not contain `=`")
                        .into(),
                ));
            }
            if !part.is_empty() {
                props.insert(name.to_owned(), Zeroizing::new(part.to_owned()));
            }
        }
        if let Some(port) = port {
            // `parse` alone would also take a sign; leading zeros are fine.
            let valid = port.bytes().all(|b| b.is_ascii_digit()) && port.parse::<u16>().is_ok();
            if !valid {
                return Err(Error::Conversion(
                    "Connection string: the port in a JDBC URL must be a number from 0 to 65535"
                        .into(),
                ));
            }
            props.insert("portnumber".to_owned(), Zeroizing::new(port.to_owned()));
        }
        Ok(())
    }

    /// Reads a `name=value` property after its `;`.
    fn property(&mut self, props: &mut HashMap<String, Zeroizing<String>>) -> crate::Result<()> {
        let name = trim(self.take_until(&['=', ';']));
        if !self.eat('=') {
            // Only an empty segment, as in `;;`, may lack the `=`.
            if name.is_empty() {
                return Ok(());
            }
            return Err(malformed_at(self.pos));
        }
        if name.is_empty() {
            return Err(malformed_at(self.pos - 1));
        }
        let name = property(&name.to_lowercase());
        let value = self.value()?;
        props.insert(name, value);
        Ok(())
    }

    /// Reads a property value, braced or not, up to its `;` or the end.
    fn value(&mut self) -> crate::Result<Zeroizing<String>> {
        let rest = self.rest();
        // The leading characters `trim` would remove.
        let lead = rest.len() - rest.trim_start_matches(|c| c <= ' ').len();
        if rest[lead..].starts_with('{') {
            self.pos += lead + 1;
            return self.braced();
        }
        let start = self.pos;
        let raw = self.take_until(&[';']);
        if let Some(brace) = raw.find('{') {
            return Err(malformed_at(start + brace));
        }
        Ok(Zeroizing::new(trim(raw).to_owned()))
    }

    /// Reads a braced value after its `{`, through the closing `}`.
    fn braced(&mut self) -> crate::Result<Zeroizing<String>> {
        let body = self.rest();
        let bytes = body.as_bytes();
        let mut i = 0;
        let end = loop {
            match bytes.get(i) {
                None => return Err(malformed_at(self.url.len())),
                Some(b'}') if bytes.get(i + 1) == Some(&b'}') => i += 2,
                Some(b'}') => break i,
                Some(_) => i += 1,
            }
        };
        let mut value = Zeroizing::new(String::with_capacity(end));
        let mut chars = body[..end].chars();
        while let Some(c) = chars.next() {
            value.push(c);
            if c == '}' {
                // The second of a doubled brace.
                chars.next();
            }
        }
        self.pos += end + 1;
        let rest = self.rest();
        self.pos += rest.len() - rest.trim_start_matches(' ').len();
        match self.peek() {
            None | Some(';') => Ok(value),
            Some(_) => Err(malformed_at(self.pos)),
        }
    }
}

impl ConfigString for JdbcConfig {
    fn get(&self, key: &str) -> Option<&str> {
        self.dict.get(key).map(|v| v.as_str())
    }

    /// `loginTimeout` (alias `connectTimeout`): seconds, 0..=65535, where 0
    /// means the driver default (mssql-jdbc).
    fn connect_timeout(&self) -> crate::Result<Option<Duration>> {
        match self.first(&["logintimeout", "connecttimeout"]) {
            None => Ok(Some(JDBC_DEFAULT_LOGIN_TIMEOUT)),
            Some(v) => match v.trim().parse::<u16>() {
                Ok(0) => Ok(Some(JDBC_DEFAULT_LOGIN_TIMEOUT)),
                Ok(secs) => Ok(Some(Duration::from_secs(secs.into()))),
                Err(_) => Err(Error::Conversion(
                    "Connection string: loginTimeout must be an integer from 0 to 65535".into(),
                )),
            },
        }
    }

    /// As mssql-jdbc's `connectInternal`: `serverName` may end in
    /// `\instance`, and an `instanceName` property wins over that.
    fn server(&self) -> crate::Result<ServerDefinition> {
        let (host, instance) = match self.first(&["servername", "server"]) {
            Some(name) => match name.split_once('\\') {
                Some((host, instance)) => (Some(host), Some(instance)),
                None => (Some(name), None),
            },
            None => (None, None),
        };
        let instance = self.get("instancename").or(instance);
        let port = self
            .first(&["portnumber", "port"])
            .map(|port| port.trim().parse())
            .transpose()?;

        Ok(ServerDefinition {
            host: host.map(str::to_owned),
            port,
            instance: instance.map(str::to_owned),
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
        let test_str = "jdbc:sqlserver://my-server.com:4200";
        let jdbc: JdbcConfig = test_str.parse()?;
        let server = jdbc.server()?;

        assert_eq!(Some("my-server.com".to_string()), server.host);
        assert_eq!(Some(4200), server.port);
        assert_eq!(None, server.instance);

        Ok(())
    }

    #[test]
    fn server_parsing_no_jdbc_no_browser() -> crate::Result<()> {
        let test_str = "jdbc:sqlserver://my-server.com:4200";
        let jdbc: JdbcConfig = test_str.parse()?;
        let server = jdbc.server()?;

        assert_eq!(Some("my-server.com".to_string()), server.host);
        assert_eq!(Some(4200), server.port);
        assert_eq!(None, server.instance);

        Ok(())
    }

    #[test]
    fn server_parsing_with_browser() -> crate::Result<()> {
        let test_str = "jdbc:sqlserver://my-server.com\\TIBERIUS";
        let jdbc: JdbcConfig = test_str.parse()?;
        let server = jdbc.server()?;

        assert_eq!(Some("my-server.com".to_string()), server.host);
        assert_eq!(None, server.port);
        assert_eq!(Some("TIBERIUS".to_string()), server.instance);

        Ok(())
    }

    #[test]
    fn server_parsing_with_browser_and_port() -> crate::Result<()> {
        let test_str = "jdbc:sqlserver://my-server.com\\TIBERIUS:666";
        let jdbc: JdbcConfig = test_str.parse()?;
        let server = jdbc.server()?;

        assert_eq!(Some("my-server.com".to_string()), server.host);
        assert_eq!(Some(666), server.port);
        assert_eq!(Some("TIBERIUS".to_string()), server.instance);

        Ok(())
    }

    #[test]
    fn database_parsing() -> crate::Result<()> {
        let test_str = "jdbc:sqlserver://myserver.com:4200;database=Foo";
        let jdbc: JdbcConfig = test_str.parse()?;

        assert_eq!(Some("Foo".to_string()), jdbc.database());

        let test_str = "jdbc:sqlserver://myserver.com:4200;databaseName=Foo";
        let jdbc: JdbcConfig = test_str.parse()?;

        assert_eq!(Some("Foo".to_string()), jdbc.database());

        let test_str = "jdbc:sqlserver://myserver.com:4200;Initial Catalog=Foo";
        let jdbc: JdbcConfig = test_str.parse()?;

        assert_eq!(Some("Foo".to_string()), jdbc.database());

        Ok(())
    }

    #[test]
    fn trust_cert_parsing_true() -> crate::Result<()> {
        let test_str = "jdbc:sqlserver://my-server.com:4200;TrustServerCertificate=true;";
        let jdbc: JdbcConfig = test_str.parse()?;

        assert!(jdbc.trust_cert()?);

        Ok(())
    }

    #[test]
    fn trust_cert_parsing_false() -> crate::Result<()> {
        let test_str = "jdbc:sqlserver://my-server.com:4200;TrustServerCertificate=false;";
        let jdbc: JdbcConfig = test_str.parse()?;

        assert!(!jdbc.trust_cert()?);

        Ok(())
    }

    #[test]
    fn trust_cert_parsing_yes() -> crate::Result<()> {
        let test_str = "jdbc:sqlserver://my-server.com:4200;TrustServerCertificate=yes;";
        let jdbc: JdbcConfig = test_str.parse()?;

        assert!(jdbc.trust_cert()?);

        Ok(())
    }

    #[test]
    fn trust_cert_parsing_no() -> crate::Result<()> {
        let test_str = "jdbc:sqlserver://my-server.com:4200;TrustServerCertificate=no;";
        let jdbc: JdbcConfig = test_str.parse()?;

        assert!(!jdbc.trust_cert()?);

        Ok(())
    }

    #[test]
    fn trust_cert_parsing_missing() -> crate::Result<()> {
        let test_str = "jdbc:sqlserver://my-server.com:4200;";
        let jdbc: JdbcConfig = test_str.parse()?;

        assert!(!jdbc.trust_cert()?);

        Ok(())
    }

    #[test]
    fn trust_cert_parsing_faulty() -> crate::Result<()> {
        let test_str = "jdbc:sqlserver://my-server.com:4200;TrustServerCertificate=musti;";
        let jdbc: JdbcConfig = test_str.parse()?;

        assert!(jdbc.trust_cert().is_err());

        Ok(())
    }

    #[test]
    fn trust_cert_ca_parsing_ok() -> crate::Result<()> {
        let test_str = "jdbc:sqlserver://my-server.com:4200;TrustServerCertificateCA=someca.crt;";
        let ado: JdbcConfig = test_str.parse()?;

        assert_eq!(Some("someca.crt".to_string()), ado.trust_cert_ca());

        Ok(())
    }

    #[test]
    fn parsing_sql_server_authentication() -> crate::Result<()> {
        let test_str = "jdbc:sqlserver://my-server.com:4200;User ID=Musti;pwd=Naukio;";
        let jdbc: JdbcConfig = test_str.parse()?;

        assert_eq!(
            AuthMethod::sql_server("Musti", "Naukio"),
            jdbc.authentication()?
        );

        Ok(())
    }

    #[test]
    #[cfg(all(windows, feature = "winauth"))]
    fn parsing_sspi_authentication() -> crate::Result<()> {
        let test_str = "jdbc:sqlserver://my-server.com:4200;IntegratedSecurity=SSPI;";
        let jdbc: JdbcConfig = test_str.parse()?;

        assert_eq!(AuthMethod::Integrated, jdbc.authentication()?);

        Ok(())
    }

    #[test]
    #[cfg(all(feature = "integrated-auth-gssapi", unix))]
    fn parsing_sspi_authentication() -> crate::Result<()> {
        let test_str = "jdbc:sqlserver://my-server.com:4200;IntegratedSecurity=true;";
        let jdbc: JdbcConfig = test_str.parse()?;

        assert_eq!(AuthMethod::Integrated, jdbc.authentication()?);

        Ok(())
    }

    #[test]
    #[cfg(all(windows, feature = "winauth"))]
    fn parsing_windows_authentication() -> crate::Result<()> {
        let test_str =
            "jdbc:sqlserver://my-server.com:4200;uid=Musti;pwd=Naukio;IntegratedSecurity=SSPI;";
        let jdbc: JdbcConfig = test_str.parse()?;

        assert_eq!(
            AuthMethod::windows("Musti", "Naukio"),
            jdbc.authentication()?
        );

        Ok(())
    }

    #[test]
    fn parsing_database() -> crate::Result<()> {
        let test_str = "jdbc:sqlserver://my-server.com:4200;database=Cats;";
        let jdbc: JdbcConfig = test_str.parse()?;

        assert_eq!(Some("Cats".to_string()), jdbc.database());

        Ok(())
    }

    #[test]
    fn parsing_login_credentials_escaping() -> crate::Result<()> {
        let test_str = "jdbc:sqlserver://my-server.com:4200;User ID=musti;Password={abc;}}45}";
        let jdbc: JdbcConfig = test_str.parse()?;

        assert_eq!(
            AuthMethod::sql_server("musti", "abc;}45"),
            jdbc.authentication()?
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
        let test_str = "jdbc:sqlserver://my-server.com:4200;encrypt=true;";
        let jdbc: JdbcConfig = test_str.parse()?;

        assert_eq!(EncryptionLevel::Required, jdbc.encrypt()?);

        Ok(())
    }

    #[test]
    #[cfg(any(
        feature = "rustls",
        feature = "native-tls",
        feature = "vendored-openssl"
    ))]
    fn encryption_parsing_off() -> crate::Result<()> {
        let test_str = "jdbc:sqlserver://my-server.com:4200;encrypt=false;";
        let jdbc: JdbcConfig = test_str.parse()?;

        assert_eq!(EncryptionLevel::Off, jdbc.encrypt()?);

        Ok(())
    }

    #[test]
    #[cfg(any(
        feature = "rustls",
        feature = "native-tls",
        feature = "vendored-openssl"
    ))]
    fn encryption_parsing_plaintext() -> crate::Result<()> {
        let test_str = "jdbc:sqlserver://my-server.com:4200;encrypt=DANGER_PLAINTEXT;";
        let jdbc: JdbcConfig = test_str.parse()?;

        assert_eq!(EncryptionLevel::NotSupported, jdbc.encrypt()?);

        Ok(())
    }

    #[test]
    #[cfg(any(
        feature = "rustls",
        feature = "native-tls",
        feature = "vendored-openssl"
    ))]
    fn encryption_parsing_missing() -> crate::Result<()> {
        let test_str = "jdbc:sqlserver://my-server.com:4200;";
        let jdbc: JdbcConfig = test_str.parse()?;

        assert_eq!(EncryptionLevel::Required, jdbc.encrypt()?);

        Ok(())
    }

    #[test]
    fn application_name_parsing() -> crate::Result<()> {
        let test_str = "jdbc:sqlserver://my-server.com:4200;Application Name=meow";
        let jdbc: JdbcConfig = test_str.parse()?;

        assert_eq!(Some("meow".into()), jdbc.application_name());

        let test_str = "jdbc:sqlserver://my-server.com:4200;ApplicationName=meow";
        let jdbc: JdbcConfig = test_str.parse()?;

        assert_eq!(Some("meow".into()), jdbc.application_name());

        Ok(())
    }

    #[test]
    fn server_properties_are_used() -> crate::Result<()> {
        let jdbc: JdbcConfig =
            "jdbc:sqlserver://;serverName=db.example.com;portNumber=2345;instanceName=INST"
                .parse()?;
        let server = jdbc.server()?;
        assert_eq!(Some("db.example.com".to_string()), server.host);
        assert_eq!(Some(2345), server.port);
        assert_eq!(Some("INST".to_string()), server.instance);

        let jdbc: JdbcConfig = "jdbc:sqlserver://;server=h;port=7".parse()?;
        let server = jdbc.server()?;
        assert_eq!(Some("h".to_string()), server.host);
        assert_eq!(Some(7), server.port);

        // `\` needs no braces.
        let jdbc: JdbcConfig = r"jdbc:sqlserver://;serverName=h\INST".parse()?;
        let server = jdbc.server()?;
        assert_eq!(Some("h".to_string()), server.host);
        assert_eq!(Some("INST".to_string()), server.instance);

        // A property overrides the URL prefix.
        let jdbc: JdbcConfig = r"jdbc:sqlserver://a\X:1;serverName=b;portNumber=2".parse()?;
        let server = jdbc.server()?;
        assert_eq!(Some("b".to_string()), server.host);
        assert_eq!(Some(2), server.port);
        assert_eq!(Some("X".to_string()), server.instance);

        let jdbc: JdbcConfig = "jdbc:sqlserver://h;portNumber=x".parse()?;
        assert!(jdbc.server().is_err());

        let config = crate::Config::from_jdbc_string(
            "jdbc:sqlserver://;serverName=db.example.com;portNumber=2345",
        )?;
        assert_eq!("db.example.com:2345", config.get_addr());
        Ok(())
    }

    #[test]
    fn login_timeout_parsing() -> crate::Result<()> {
        use std::time::Duration;
        let t = |s: &str| s.parse::<JdbcConfig>().unwrap().connect_timeout();
        assert_eq!(t("jdbc:sqlserver://h")?, Some(Duration::from_secs(30)));
        assert_eq!(
            t("jdbc:sqlserver://h;loginTimeout=5")?,
            Some(Duration::from_secs(5))
        );
        assert_eq!(
            t("jdbc:sqlserver://h;connectTimeout=6")?,
            Some(Duration::from_secs(6))
        );
        assert_eq!(
            t("jdbc:sqlserver://h;loginTimeout=0")?,
            Some(Duration::from_secs(30))
        );
        assert!(t("jdbc:sqlserver://h;loginTimeout=65536").is_err());
        assert!(t("jdbc:sqlserver://h;loginTimeout=-1").is_err());
        Ok(())
    }

    fn value(s: &str, key: &str) -> Option<String> {
        let jdbc: JdbcConfig = s.parse().unwrap_or_else(|e| panic!("{s:?}: {e}"));
        jdbc.get(key).map(str::to_owned)
    }

    #[test]
    fn mssql_jdbc_value_rules() {
        let v = |props: &str| value(&format!("jdbc:sqlserver://h;{props}"), "password");
        // Only `;` and a leading `{` are special outside braces.
        assert_eq!(v(r"password=a\b").as_deref(), Some(r"a\b"));
        assert_eq!(v("password=a=b:c/d[e]}").as_deref(), Some("a=b:c/d[e]}"));
        assert_eq!(v("password=pässwörd€😀").as_deref(), Some("pässwörd€😀"));
        // Unescaped names and values are trimmed; braced values are not.
        assert_eq!(v("  password  =  a b  ;").as_deref(), Some("a b"));
        assert_eq!(v("password= { a;b } ;").as_deref(), Some(" a;b "));
        // `}}` is a literal `}` inside braces (mssql-jdbc 8.4+).
        assert_eq!(
            v("password={pass\";{}}word}").as_deref(),
            Some("pass\";{}word")
        );
        assert_eq!(v("password={ü\\{x}").as_deref(), Some("ü\\{x"));
        assert_eq!(v("password=").as_deref(), Some(""));
        // The last occurrence wins.
        assert_eq!(v("password=a;password=b").as_deref(), Some("b"));

        for bad in [
            "password={abc",   // unclosed brace
            "password={a}b",   // text after a braced value
            "password=a{b}",   // a brace after text
            "password",        // no `=`
            "password;user=x", // ditto
            "=x",              // empty name
        ] {
            let s = format!("jdbc:sqlserver://h;{bad}");
            assert!(s.parse::<JdbcConfig>().is_err(), "{bad:?} parsed");
        }
    }

    #[test]
    fn mssql_jdbc_url_prefix_rules() -> crate::Result<()> {
        let server = |s: &str| s.parse::<JdbcConfig>().and_then(|j| j.server());
        let s = server(r"JDBC:SQLServer:// my host \ inst : 1433 ;")?;
        assert_eq!(s.host.as_deref(), Some("my host"));
        assert_eq!(s.instance.as_deref(), Some("inst"));
        assert_eq!(s.port, Some(1433));
        // The instanceName property wins over `serverName=host\instance`.
        let s = server(r"jdbc:sqlserver://;serverName=h\A;instanceName=B")?;
        assert_eq!(
            (s.host.as_deref(), s.instance.as_deref()),
            (Some("h"), Some("B"))
        );
        let s = server(r"jdbc:sqlserver://;serverName=h\A")?;
        assert_eq!(
            (s.host.as_deref(), s.instance.as_deref()),
            (Some("h"), Some("A"))
        );
        // IPv6: brackets in the URL (the old parser's), bare in serverName.
        assert_eq!(
            server("jdbc:sqlserver://[::1]:1433")?.host.as_deref(),
            Some("[::1]")
        );
        let s = server("jdbc:sqlserver://;serverName=fe80::1;port=7")?;
        assert_eq!((s.host.as_deref(), s.port), (Some("fe80::1"), Some(7)));

        for bad in [
            "jdbc:mysql://h",
            "sqlserver://h",
            "jdbc:sqlserver://h:+5",
            "jdbc:sqlserver://h:65536",
            "jdbc:sqlserver://h:",
            "jdbc:sqlserver://a=b",
            r"jdbc:sqlserver://h\a=b",
            "jdbc:sqlserver://[::1",
            "jdbc:sqlserver:/",
        ] {
            assert!(bad.parse::<JdbcConfig>().is_err(), "{bad:?} parsed");
        }
        Ok(())
    }
}
