use std::path::Path;

pub(crate) fn distribution() -> Result<String, String> {
    let value =
        std::env::var("CENTAERIS_WSL_DISTRIBUTION").unwrap_or_else(|_| "Ubuntu-24.04".into());
    if value.is_empty()
        || !value.as_bytes()[0].is_ascii_alphanumeric()
        || !value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"._-".contains(&c))
    {
        return Err("WSL distribution name is invalid".into());
    }
    Ok(value)
}

pub(crate) fn linux_path(value: &str, distribution: &str, import: bool) -> Result<String, String> {
    let value = if let Some(rest) = value.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{rest}")
    } else {
        value.strip_prefix(r"\\?\").unwrap_or(value).to_string()
    };
    if value.is_empty() || value.contains('\0') {
        return Err("WSL path is invalid".into());
    }
    let slash = value.replace('\\', "/");
    let mapped = if let Some(unc) = slash.strip_prefix("//") {
        let mut parts = unc.splitn(3, '/');
        let host = parts.next().unwrap_or_default();
        if !host.eq_ignore_ascii_case("wsl.localhost") && !host.eq_ignore_ascii_case("wsl$") {
            return Err("An absolute Linux or selected WSL distribution path is required".into());
        }
        if !parts
            .next()
            .unwrap_or_default()
            .eq_ignore_ascii_case(distribution)
        {
            return Err("Path belongs to another WSL distribution".into());
        }
        format!("/{}", parts.next().unwrap_or_default())
    } else if slash
        .as_bytes()
        .first()
        .is_some_and(u8::is_ascii_alphabetic)
        && slash.as_bytes().get(1..3) == Some(b":/")
    {
        format!("/mnt/{}{}", slash[..1].to_ascii_lowercase(), &slash[2..])
    } else if value.starts_with('/') {
        value
    } else {
        return Err("An absolute Linux or selected WSL distribution path is required".into());
    };
    let mut parts = Vec::new();
    for part in mapped.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            _ => parts.push(part),
        }
    }
    let mut normalized = format!("/{}", parts.join("/"));
    if mapped.ends_with('/') && normalized != "/" {
        normalized.push('/');
    }
    if !import
        && parts.len() >= 2
        && parts[0] == "mnt"
        && parts[1].len() == 1
        && parts[1].as_bytes()[0].is_ascii_alphabetic()
    {
        return Err(
            "Execution resources must be on the WSL Linux filesystem; choose a WSL workspace"
                .into(),
        );
    }
    Ok(normalized)
}

pub(crate) fn desktop_path(value: &str, distribution: &str) -> Result<String, String> {
    let mapped = linux_path(value, distribution, true)?;
    if let Some(tail) = mapped.strip_prefix("/mnt/") {
        let mut parts = tail.splitn(2, '/');
        let drive = parts.next().unwrap_or_default();
        if drive.len() == 1 && drive.as_bytes()[0].is_ascii_alphabetic() {
            return Ok(format!(
                "{}:\\{}",
                drive.to_ascii_uppercase(),
                parts.next().unwrap_or_default().replace('/', "\\")
            ));
        }
    }
    Ok(format!(
        r"\\wsl.localhost\{}{}",
        distribution,
        mapped.replace('/', "\\")
    ))
}

pub(crate) fn workspace_paths(value: &Path) -> Result<(std::path::PathBuf, String), String> {
    let distribution = distribution()?;
    let linux = linux_path(&value.to_string_lossy(), &distribution, false)?;
    let local = std::path::PathBuf::from(desktop_path(&linux, &distribution)?);
    let canonical = local
        .canonicalize()
        .map_err(|e| format!("workspace path is not readable: {e}"))?;
    if !canonical.is_dir() {
        return Err("workspace path is not a directory".into());
    }
    let linux = linux_path(&canonical.to_string_lossy(), &distribution, false)?;
    Ok((canonical, linux))
}

#[cfg(windows)]
const BOOTSTRAP: &str = include_str!("../../runtime/host/wsl-bootstrap.sh");

#[cfg(windows)]
pub(crate) struct WslRuntime {
    pub(crate) distribution: String,
    binary: String,
    workspace: String,
    data: String,
}

#[cfg(windows)]
impl WslRuntime {
    pub(crate) fn prepare(executable: &Path, digest: &str) -> Result<Self, String> {
        let distribution = distribution()?;
        let source = linux_path(&executable.to_string_lossy(), &distribution, true)?;
        let output = capture(command(&distribution, "install", &[&source, digest]))?;
        let paths: Vec<_> = output.trim().lines().collect();
        if paths.len() != 3 {
            return Err("Invalid WSL Runtime installation result".into());
        }
        let data = std::env::var("CENTAERIS_WSL_DATA_DIR").unwrap_or_else(|_| paths[2].into());
        Ok(Self {
            distribution: distribution.clone(),
            binary: linux_path(paths[0], &distribution, false)?,
            workspace: linux_path(paths[1], &distribution, false)?,
            data: linux_path(&data, &distribution, false)?,
        })
    }
    pub(crate) fn command(&self, operation: &str) -> std::process::Command {
        command(
            &self.distribution,
            operation,
            &[&self.binary, &self.workspace, &self.data],
        )
    }
    pub(crate) fn start(&self) -> Result<(), String> {
        capture(self.command("start")).map(|_| ())
    }
}

#[cfg(windows)]
fn command(distribution: &str, operation: &str, arguments: &[&str]) -> std::process::Command {
    use std::os::windows::process::CommandExt;
    let mut command = std::process::Command::new("wsl.exe");
    command
        .args([
            "--distribution",
            distribution,
            "--exec",
            "/bin/sh",
            "-c",
            BOOTSTRAP,
            "centaeris-wsl",
            operation,
        ])
        .args(arguments)
        .creation_flags(0x08000000);
    command
}

#[cfg(windows)]
fn capture(mut command: std::process::Command) -> Result<String, String> {
    use std::{
        io::Read,
        process::Stdio,
        thread,
        time::{Duration, Instant},
    };
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("start WSL failed: {e}"))?;
    let stdout = child.stdout.take().expect("piped stdout");
    let stderr = child.stderr.take().expect("piped stderr");
    let read = |stream: Box<dyn Read + Send>| {
        thread::spawn(move || {
            let mut text = String::new();
            stream.take(65537).read_to_string(&mut text).map(|_| text)
        })
    };
    let out = read(Box::new(stdout));
    let err = read(Box::new(stderr));
    let deadline = Instant::now() + Duration::from_secs(30);
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(25)),
            other => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("WSL setup timed out or failed: {other:?}"));
            }
        }
    };
    let output = out
        .join()
        .map_err(|_| "WSL stdout reader stopped")?
        .map_err(|e| e.to_string())?;
    let diagnostic = err
        .join()
        .map_err(|_| "WSL stderr reader stopped")?
        .map_err(|e| e.to_string())?;
    if !status.success() {
        return Err(format!(
            "WSL setup failed: {}",
            super::runtime_client::sanitize_runtime_stderr(&diagnostic)
        ));
    }
    if output.len() > 65536 {
        return Err("WSL setup response exceeds limit".into());
    }
    Ok(output)
}

pub(crate) fn map_request_paths(
    method: &str,
    mut params: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let Some(request) = params
        .get_mut("request")
        .and_then(serde_json::Value::as_object_mut)
    else {
        return Ok(params);
    };
    static PATH_FIELDS: std::sync::LazyLock<serde_json::Value> = std::sync::LazyLock::new(|| {
        serde_json::from_str(include_str!("../../runtime/host/wsl-request-paths.json"))
            .expect("checked-in WSL request paths")
    });
    let entry = &PATH_FIELDS[method];
    let fields = entry["fields"].as_array().cloned().unwrap_or_default();
    let distro = distribution()?;
    for field in &fields {
        let field = field.as_str().expect("path field name");
        if let Some(value) = request.get(field).and_then(serde_json::Value::as_str) {
            let mapped = linux_path(
                value,
                &distro,
                entry["importSource"].as_bool().unwrap_or(false),
            )?;
            request.insert(field.into(), mapped.into());
        }
    }
    if method == "session/prompt" {
        if let Some(attachments) = request
            .get_mut("attachments")
            .and_then(serde_json::Value::as_array_mut)
        {
            for attachment in attachments {
                if let Some(value) = attachment
                    .get("localPath")
                    .and_then(serde_json::Value::as_str)
                {
                    let mapped = linux_path(value, &distro, true)?;
                    attachment["localPath"] = mapped.into();
                }
            }
        }
    }
    Ok(params)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn request_mapping_preserves_prompt_text_and_imports_attachments() {
        let input = serde_json::json!({"request": {
            "text": "Read D:\\notes\\example.txt literally",
            "attachments": [{"localPath": "D:\\Images\\图 one.png", "mimeType": "image/png"}]
        }});
        let mapped = map_request_paths("session/prompt", input.clone()).unwrap();
        assert_eq!(mapped["request"]["text"], input["request"]["text"]);
        assert_eq!(
            mapped["request"]["attachments"][0]["localPath"],
            "/mnt/d/Images/图 one.png"
        );
        assert_eq!(mapped["request"]["attachments"][0]["mimeType"], "image/png");
        assert!(map_request_paths(
            "session/new",
            serde_json::json!({"request": {"cwd": "D:\\workspace"}})
        )
        .is_err());
    }

    #[test]
    fn wsl_paths_match_the_shared_host_contract() {
        let cases: serde_json::Value =
            serde_json::from_str(include_str!("../../runtime/host/wsl-path-cases.json")).unwrap();
        for case in cases.as_array().unwrap() {
            let input = case["input"].as_str().unwrap();
            let result = linux_path(
                input,
                "Ubuntu-24.04",
                case["importSource"].as_bool().unwrap_or(false),
            );
            if case["error"] == true {
                assert!(result.is_err(), "{input}");
            } else {
                assert_eq!(result.unwrap(), case["linux"].as_str().unwrap(), "{input}");
            }
        }
    }
    #[test]
    fn linux_paths_round_trip_to_windows_without_losing_unicode() {
        let linux = "/home/user/项目 one";
        assert_eq!(
            linux_path(
                &desktop_path(linux, "Ubuntu-24.04").unwrap(),
                "Ubuntu-24.04",
                false
            )
            .unwrap(),
            linux
        );
    }
}
