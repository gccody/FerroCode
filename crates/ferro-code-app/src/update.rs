use ferro_code_protocol::hidden_command;
use serde_json::Value;
use std::process::Stdio;

pub fn check() -> Result<Option<String>, String> {
    let output = hidden_command("codex")
        .args(["doctor", "--json"])
        .stdin(Stdio::null())
        .output()
        .map_err(|error| format!("Could not check the Codex version: {error}"))?;

    if !output.status.success() {
        return Err(command_error(
            "Codex could not check for updates",
            &output.stdout,
            &output.stderr,
        ));
    }

    parse_available_version(&output.stdout)
}

pub fn install() -> Result<(), String> {
    let output = hidden_command("codex")
        .arg("update")
        .stdin(Stdio::null())
        .output()
        .map_err(|error| format!("Could not start the Codex update: {error}"))?;

    if output.status.success() {
        Ok(())
    } else {
        Err(command_error(
            "Codex update failed",
            &output.stdout,
            &output.stderr,
        ))
    }
}

fn parse_available_version(bytes: &[u8]) -> Result<Option<String>, String> {
    let report: Value = serde_json::from_slice(bytes)
        .map_err(|error| format!("Codex returned an unreadable update report: {error}"))?;
    let details = report
        .pointer("/checks/updates.status/details")
        .and_then(Value::as_object)
        .ok_or_else(|| "Codex did not include update information in its report".to_owned())?;
    let status = details
        .get("latest version status")
        .and_then(Value::as_str)
        .unwrap_or_default();

    if status != "newer version is available" {
        return Ok(None);
    }

    details
        .get("latest version")
        .and_then(Value::as_str)
        .filter(|version| !version.trim().is_empty())
        .map(|version| Some(version.to_owned()))
        .ok_or_else(|| "Codex reported an update without a version number".to_owned())
}

fn command_error(context: &str, stdout: &[u8], stderr: &[u8]) -> String {
    let detail = [stderr, stdout]
        .into_iter()
        .filter_map(|bytes| std::str::from_utf8(bytes).ok())
        .flat_map(str::lines)
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("unknown error");
    format!("{context}: {detail}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_available_update_from_doctor_report() {
        let report = br#"{
            "checks": {
                "updates.status": {
                    "details": {
                        "latest version": "0.147.0",
                        "latest version status": "newer version is available"
                    }
                }
            }
        }"#;

        assert_eq!(
            parse_available_version(report).unwrap(),
            Some("0.147.0".into())
        );
    }

    #[test]
    fn current_version_does_not_create_an_update_notification() {
        let report = br#"{
            "checks": {
                "updates.status": {
                    "details": {
                        "latest version": "0.146.0",
                        "latest version status": "current version is not older"
                    }
                }
            }
        }"#;

        assert_eq!(parse_available_version(report).unwrap(), None);
    }
}
