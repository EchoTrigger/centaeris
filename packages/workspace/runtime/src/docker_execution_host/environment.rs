//! Trusted execution-host defaults, separate from the control plane's proxy.
use std::collections::BTreeMap;

pub(super) fn public_internet_proxy_environment() -> Result<BTreeMap<String, String>, String> {
    proxy_environment(|name| match std::env::var(name) {
        Ok(value) => Ok(Some(value)),
        Err(std::env::VarError::NotPresent) => Ok(None),
        Err(_) => Err("execution_proxy_configuration_invalid".into()),
    })
}

fn proxy_environment(
    get: impl Fn(&str) -> Result<Option<String>, String>,
) -> Result<BTreeMap<String, String>, String> {
    let mut values = BTreeMap::new();
    for (setting, name) in [
        ("EXECUTION_HTTP_PROXY", "HTTP_PROXY"),
        ("EXECUTION_HTTPS_PROXY", "HTTPS_PROXY"),
    ] {
        if let Some(value) = get(setting)? {
            let url =
                reqwest::Url::parse(&value).map_err(|_| "execution_proxy_configuration_invalid")?;
            if value.len() > 4096
                || value.chars().any(char::is_control)
                || !matches!(url.scheme(), "http" | "https")
                || url.host_str().is_none()
                || url.fragment().is_some()
            {
                return Err("execution_proxy_configuration_invalid".into());
            }
            values.insert(name.into(), value.clone());
            values.insert(name.to_ascii_lowercase(), value);
        }
    }
    if !values.is_empty() {
        let bypass = get("EXECUTION_NO_PROXY")?.unwrap_or_else(|| "localhost,127.0.0.1,::1".into());
        if bypass.len() > 4096 || bypass.chars().any(char::is_control) {
            return Err("execution_proxy_configuration_invalid".into());
        }
        values.insert("NO_PROXY".into(), bypass.clone());
        values.insert("no_proxy".into(), bypass);
    }
    Ok(values)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn execution_proxy_defaults_do_not_inherit_control_plane_environment() {
        let env = BTreeMap::from([
            ("HTTP_PROXY", "http://control-plane.invalid:7890"),
            ("EXECUTION_HTTP_PROXY", "http://172.17.0.1:17890"),
            ("EXECUTION_HTTPS_PROXY", "http://172.17.0.1:17890"),
        ]);
        let actual = proxy_environment(|name| Ok(env.get(name).map(|s| s.to_string()))).unwrap();
        assert_eq!(actual["HTTP_PROXY"], "http://172.17.0.1:17890");
        assert_eq!(actual["http_proxy"], actual["HTTP_PROXY"]);
        assert_eq!(actual["HTTPS_PROXY"], actual["HTTP_PROXY"]);
        assert_eq!(actual["https_proxy"], actual["HTTPS_PROXY"]);
        assert_eq!(actual["NO_PROXY"], "localhost,127.0.0.1,::1");
        assert_eq!(actual["no_proxy"], actual["NO_PROXY"]);
        assert!(proxy_environment(|_| Ok(None)).unwrap().is_empty());
    }

    #[test]
    fn invalid_execution_proxy_reports_no_configuration_values() {
        for value in [
            "not a url with private value",
            "socks5://proxy.invalid:7890",
            "http://proxy.invalid/#secret",
            "http://proxy.invalid/\n",
        ] {
            assert_eq!(
                proxy_environment(
                    |name| Ok((name == "EXECUTION_HTTP_PROXY").then(|| value.into()))
                ),
                Err("execution_proxy_configuration_invalid".into())
            );
        }
        let actual = proxy_environment(|name| {
            Ok(match name {
                "EXECUTION_HTTPS_PROXY" => Some("http://proxy.invalid:7890".into()),
                "EXECUTION_NO_PROXY" => Some("localhost,127.0.0.1,::1,internal.test".into()),
                _ => None,
            })
        })
        .unwrap();
        assert_eq!(actual["NO_PROXY"], "localhost,127.0.0.1,::1,internal.test");
    }
}
