//! The system `aws` CLI as a subprocess — shared by the Secrets Manager
//! resolver (`creds::aws_secrets`) and the RDS inventory source
//! (`inventory::rds`).
//!
//! Subprocess for the reasons `tunnel.rs` gives for `ssh`: the operator's
//! profiles, SSO cache and `credential_process` work unchanged, and no
//! crate joins the dependency tree. Every value pgman passes is validated
//! ([`AwsScope::validate`] and the callers' own checks), so a config value
//! can never be read by the CLI as an option.

use std::process::{Command, Stdio};

/// Where a call runs: region, profile, and the account the caller identity
/// must belong to. `None` means the CLI's own resolution (the ambient
/// profile, its default region).
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct AwsScope {
    pub region: Option<String>,
    pub profile: Option<String>,
    /// When set, [`AwsScope::check_account`] must pass before anything is
    /// read — a wrong ambient profile then fails, rather than reading a
    /// same-named resource in another account.
    pub account: Option<String>,
}

/// `v` is a plain CLI argument value: non-empty, at most `max` bytes, not
/// starting with `-`, and only ASCII alphanumerics plus `extra`.
pub fn plain_arg(v: &str, extra: &str, max: usize) -> bool {
    !v.is_empty()
        && v.len() <= max
        && !v.starts_with('-')
        && v.chars()
            .all(|c| c.is_ascii_alphanumeric() || extra.contains(c))
}

impl AwsScope {
    pub fn validate(&self) -> Result<(), String> {
        if let Some(r) = &self.region {
            if !plain_arg(r, "-", 32) {
                return Err(format!("aws_region {r:?} is not a region name"));
            }
        }
        if let Some(p) = &self.profile {
            if !plain_arg(p, "_.-", 128) {
                return Err(format!("aws_profile {p:?} is not a profile name"));
            }
        }
        if let Some(a) = &self.account {
            if a.len() != 12 || !a.chars().all(|c| c.is_ascii_digit()) {
                return Err(format!("aws_account {a:?} is not a 12-digit account id"));
            }
        }
        Ok(())
    }

    /// Output format, timeouts, region and profile flags for one call.
    pub fn common_args(&self, output: &str) -> Vec<String> {
        let mut args = vec![
            "--output".to_string(),
            output.to_string(),
            "--cli-connect-timeout".to_string(),
            "10".to_string(),
            "--cli-read-timeout".to_string(),
            "10".to_string(),
        ];
        if let Some(region) = &self.region {
            args.extend(["--region".to_string(), region.clone()]);
        }
        if let Some(profile) = &self.profile {
            args.extend(["--profile".to_string(), profile.clone()]);
        }
        args
    }

    /// `aws sts get-caller-identity …` arguments.
    pub fn caller_account_args(&self) -> Vec<String> {
        let mut args = vec![
            "sts".to_string(),
            "get-caller-identity".to_string(),
            "--query".to_string(),
            "Account".to_string(),
        ];
        args.extend(self.common_args("text"));
        args
    }

    /// Fail unless the credentials in use belong to `self.account` (when
    /// one is set). `what` names what would otherwise have been read.
    /// Blocking.
    pub fn check_account(&self, what: &str) -> Result<(), String> {
        let Some(want) = &self.account else {
            return Ok(());
        };
        let got = run(&self.caller_account_args())?;
        let got = got.trim();
        if got != want {
            return Err(format!(
                "the AWS credentials in use are for account {got}, not {want}; \
                 refusing to read {what}"
            ));
        }
        Ok(())
    }
}

/// Run `aws` with `args`; stdout on success. A failure carries the CLI's
/// own stderr (trimmed, capped), which names the problem — expired SSO,
/// access denied, no such resource — and never holds a secret value.
/// Blocking.
pub fn run(args: &[String]) -> Result<String, String> {
    let out = Command::new("aws")
        .args(args)
        .env("AWS_PAGER", "")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| format!("could not run the aws CLI ({e}); is it installed and on PATH?"))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        let err: String = err.trim().chars().take(500).collect();
        return Err(format!("aws {} failed: {err}", args[..2].join(" ")));
    }
    String::from_utf8(out.stdout).map_err(|_| "the aws CLI returned non-UTF-8 output".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scope_values_can_never_become_options() {
        let ok = AwsScope {
            region: Some("us-west-1".into()),
            profile: Some("examplecorp_prod.ro".into()),
            account: Some("210987654321".into()),
        };
        assert!(ok.validate().is_ok());
        for bad in [
            AwsScope {
                region: Some("--debug".into()),
                ..AwsScope::default()
            },
            AwsScope {
                region: Some("us west".into()),
                ..AwsScope::default()
            },
            AwsScope {
                profile: Some("-p".into()),
                ..AwsScope::default()
            },
            AwsScope {
                account: Some("54084755703".into()),
                ..AwsScope::default()
            },
            AwsScope {
                account: Some("54084755703x".into()),
                ..AwsScope::default()
            },
        ] {
            assert!(bad.validate().is_err(), "{bad:?}");
        }
    }

    #[test]
    fn common_args_carry_format_timeouts_region_and_profile() {
        let s = AwsScope {
            region: Some("us-west-1".into()),
            profile: Some("p".into()),
            account: None,
        };
        let a = s.common_args("json").join(" ");
        assert!(a.starts_with("--output json"), "{a}");
        assert!(a.contains("--cli-read-timeout 10"), "{a}");
        assert!(
            a.contains("--region us-west-1") && a.contains("--profile p"),
            "{a}"
        );
        assert!(!AwsScope::default()
            .common_args("text")
            .join(" ")
            .contains("--region"));
        assert_eq!(
            s.caller_account_args()[..4],
            ["sts", "get-caller-identity", "--query", "Account"]
        );
    }
}
