//! The `rds` inventory source: `aws rds describe-db-instances`, parsed.
//! Gives each instance's endpoint, engine version, status and replica
//! links without connecting to any database.

use crate::aws_cli::AwsScope;

/// The fields of one `DBInstances[]` entry the overview shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RdsInstance {
    pub id: String,
    pub host: Option<String>,
    pub port: Option<u16>,
    pub engine: String,
    pub engine_version: String,
    pub status: String,
    pub class: String,
    pub multi_az: bool,
    /// The primary this instance replicates, when it is a read replica.
    pub source: Option<String>,
    /// Read replicas of this instance.
    pub replicas: Vec<String>,
    /// The database RDS created the instance with (`DBName`).
    pub db_name: Option<String>,
    /// The VPC the instance sits in, for finding a bastion beside it.
    pub vpc_id: Option<String>,
    /// Versions this engine version can be upgraded to, when known.
    pub upgrades: Option<Upgrades>,
}

/// The newest versions an engine version can move to.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Upgrades {
    /// Newest minor version: same major, a patch-and-restart away.
    pub minor: Option<String>,
    /// Newest major version.
    pub major: Option<String>,
}

impl Upgrades {
    /// `↑16.15 · 18.6`, or `None` when it's already the newest. Pure.
    pub fn label(&self) -> Option<String> {
        let parts: Vec<&str> = [self.minor.as_deref(), self.major.as_deref()]
            .into_iter()
            .flatten()
            .collect();
        (!parts.is_empty()).then(|| format!("↑{}", parts.join(" · ")))
    }
}

/// `16.10` as numbers, for ordering; anything that isn't a number sorts
/// as zero. Pure.
fn version_key(v: &str) -> Vec<u64> {
    v.split('.').map(|p| p.parse().unwrap_or(0)).collect()
}

/// `aws rds describe-db-engine-versions` for one engine version.
pub fn engine_versions_args(scope: &AwsScope, engine: &str, version: &str) -> Vec<String> {
    let mut args = vec![
        "rds".to_string(),
        "describe-db-engine-versions".to_string(),
        "--engine".to_string(),
        engine.to_string(),
        "--engine-version".to_string(),
        version.to_string(),
    ];
    args.extend(scope.common_args("json"));
    args
}

/// The newest minor and major targets from the CLI's JSON. Pure.
pub fn parse_upgrades(json: &str) -> Result<Upgrades, String> {
    let v: serde_json::Value = serde_json::from_str(json)
        .map_err(|e| format!("describe-db-engine-versions returned unreadable JSON: {e}"))?;
    let targets = v
        .get("DBEngineVersions")
        .and_then(|l| l.as_array())
        .and_then(|l| l.first())
        .and_then(|e| e.get("ValidUpgradeTarget"))
        .and_then(|t| t.as_array())
        .cloned()
        .unwrap_or_default();
    let newest = |major: bool| {
        targets
            .iter()
            .filter(|t| t.get("IsMajorVersionUpgrade").and_then(|m| m.as_bool()) == Some(major))
            .filter_map(|t| t.get("EngineVersion").and_then(|e| e.as_str()))
            .max_by_key(|v| version_key(v))
            .map(str::to_string)
    };
    Ok(Upgrades {
        minor: newest(false),
        major: newest(true),
    })
}

impl RdsInstance {
    pub fn is_postgres(&self) -> bool {
        self.engine == "postgres" || self.engine == "aurora-postgresql"
    }
}

/// `aws rds describe-db-instances` arguments. The CLI pages through
/// every result itself.
pub fn describe_args(scope: &AwsScope) -> Vec<String> {
    let mut args = vec!["rds".to_string(), "describe-db-instances".to_string()];
    args.extend(scope.common_args("json"));
    args
}

/// Parse the CLI's JSON. Missing optional fields are `None`/empty; an
/// entry without an identifier is skipped rather than invented.
pub fn parse_describe(json: &str) -> Result<Vec<RdsInstance>, String> {
    let v: serde_json::Value = serde_json::from_str(json)
        .map_err(|e| format!("describe-db-instances returned unreadable JSON: {e}"))?;
    let list = v
        .get("DBInstances")
        .and_then(|l| l.as_array())
        .ok_or("describe-db-instances returned no DBInstances list")?;
    let s = |i: &serde_json::Value, k: &str| i.get(k).and_then(|x| x.as_str()).map(str::to_string);
    Ok(list
        .iter()
        .filter_map(|i| {
            Some(RdsInstance {
                id: s(i, "DBInstanceIdentifier")?,
                host: i
                    .get("Endpoint")
                    .and_then(|e| e.get("Address"))
                    .and_then(|a| a.as_str())
                    .map(str::to_string),
                port: i
                    .get("Endpoint")
                    .and_then(|e| e.get("Port"))
                    .and_then(|p| p.as_u64())
                    .and_then(|p| u16::try_from(p).ok()),
                engine: s(i, "Engine").unwrap_or_default(),
                engine_version: s(i, "EngineVersion").unwrap_or_default(),
                status: s(i, "DBInstanceStatus").unwrap_or_default(),
                class: s(i, "DBInstanceClass").unwrap_or_default(),
                multi_az: i.get("MultiAZ").and_then(|m| m.as_bool()).unwrap_or(false),
                source: s(i, "ReadReplicaSourceDBInstanceIdentifier").map(|v| instance_id(&v)),
                replicas: i
                    .get("ReadReplicaDBInstanceIdentifiers")
                    .and_then(|r| r.as_array())
                    .map(|r| {
                        r.iter()
                            .filter_map(|x| x.as_str().map(instance_id))
                            .collect()
                    })
                    .unwrap_or_default(),
                db_name: s(i, "DBName"),
                vpc_id: i
                    .get("DBSubnetGroup")
                    .and_then(|g| g.get("VpcId"))
                    .and_then(|v| v.as_str())
                    .map(str::to_string),
                upgrades: None,
            })
        })
        .collect())
}

/// Shell-style match with `*` as the only wildcard. Case-sensitive, as
/// RDS identifiers are lowercase.
pub fn glob_match(pattern: &str, s: &str) -> bool {
    let parts: Vec<&str> = pattern.split('*').collect();
    if parts.len() == 1 {
        return pattern == s;
    }
    let (first, last) = (parts[0], parts[parts.len() - 1]);
    if !s.starts_with(first) || s.len() < first.len() + last.len() || !s.ends_with(last) {
        return false;
    }
    let mut rest = &s[first.len()..s.len() - last.len()];
    for mid in &parts[1..parts.len() - 1] {
        match rest.find(mid) {
            Some(i) => rest = &rest[i + mid.len()..],
            None => return false,
        }
    }
    true
}

/// The Postgres instances an environment covers: those matching any of
/// `patterns`, plus the replicas of every match, each primary followed
/// by its replicas. A replica matched on its own still lists after its
/// primary when the primary is in the set too.
pub fn select<'a>(all: &'a [RdsInstance], patterns: &[String]) -> Vec<&'a RdsInstance> {
    let by_id = |id: &str| all.iter().find(|i| i.id == id);
    let mut chosen: Vec<&RdsInstance> = all
        .iter()
        .filter(|i| i.is_postgres() && patterns.iter().any(|p| glob_match(p, &i.id)))
        .collect();
    let replicas: Vec<&RdsInstance> = chosen
        .iter()
        .flat_map(|i| i.replicas.iter().filter_map(|r| by_id(r)))
        .filter(|r| r.is_postgres())
        .collect();
    for r in replicas {
        if !chosen.iter().any(|c| c.id == r.id) {
            chosen.push(r);
        }
    }
    // Group: each instance sorts under its primary's id, primaries first.
    let ids: std::collections::HashSet<String> = chosen.iter().map(|c| c.id.clone()).collect();
    chosen.sort_by_key(|i| {
        let root = i
            .source
            .clone()
            .filter(|s| ids.contains(s))
            .unwrap_or_else(|| i.id.clone());
        (root, i.source.is_some(), i.id.clone())
    });
    chosen
}

/// An instance identifier from what RDS lists: a replica in another
/// region names its source (and a primary its replicas) by ARN,
/// `arn:aws:rds:eu-west-1:123456789012:db:demo`. Pure.
pub fn instance_id(v: &str) -> String {
    match v.strip_prefix("arn:") {
        Some(_) => v.rsplit(":db:").next().unwrap_or(v).to_string(),
        None => v.to_string(),
    }
}

#[cfg(test)]
pub(crate) mod tests {

    #[test]
    fn a_cross_region_replica_names_its_primary_by_identifier() {
        assert_eq!(
            instance_id("arn:aws:rds:us-east-1:210987654321:db:demo"),
            "demo"
        );
        assert_eq!(
            instance_id("demo-read-only-replica"),
            "demo-read-only-replica"
        );
        let all = parse_describe(
            r#"{"DBInstances": [{"DBInstanceIdentifier": "demo-dr", "Engine": "postgres",
              "ReadReplicaSourceDBInstanceIdentifier": "arn:aws:rds:us-west-1:210987654321:db:demo"}]}"#,
        )
        .unwrap();
        assert_eq!(all[0].source.as_deref(), Some("demo"));
    }

    use super::*;

    /// The shape the CLI returned for examplecorp's account on 2026-10-19,
    /// trimmed to the fields that matter (identifiers kept, nothing secret).
    pub(crate) const FIXTURE: &str = r#"{"DBInstances": [
      {"DBInstanceIdentifier": "demo", "Engine": "postgres", "EngineVersion": "16.10",
       "DBInstanceStatus": "available", "DBInstanceClass": "db.t4g.medium", "MultiAZ": false,
       "Endpoint": {"Address": "demo.x.us-west-1.rds.amazonaws.com", "Port": 5432},
       "ReadReplicaDBInstanceIdentifiers": ["demo-read-only-replica"], "DBName": "ledger",
       "DBSubnetGroup": {"VpcId": "vpc-0a1b2c3d"}},
      {"DBInstanceIdentifier": "demo-read-only-replica", "Engine": "postgres", "EngineVersion": "16.10",
       "DBInstanceStatus": "available", "DBInstanceClass": "db.t4g.medium",
       "Endpoint": {"Address": "demo-read-only-replica.x.us-west-1.rds.amazonaws.com", "Port": 5432},
       "ReadReplicaSourceDBInstanceIdentifier": "demo", "ReadReplicaDBInstanceIdentifiers": [], "DBName": "ledger",
       "DBSubnetGroup": {"VpcId": "vpc-0a1b2c3d"}},
      {"DBInstanceIdentifier": "development", "Engine": "postgres", "EngineVersion": "16.10",
       "DBInstanceStatus": "stopped", "DBInstanceClass": "db.t4g.medium",
       "Endpoint": {"Address": "development.x.us-west-1.rds.amazonaws.com", "Port": 5432}, "DBName": "demo"},
      {"DBInstanceIdentifier": "production", "Engine": "postgres", "EngineVersion": "16.10",
       "DBInstanceStatus": "available", "DBInstanceClass": "db.t4g.medium", "MultiAZ": true,
       "Endpoint": {"Address": "production.x.us-west-1.rds.amazonaws.com", "Port": 5432},
       "ReadReplicaDBInstanceIdentifiers": ["production-read-only-replica"], "DBName": "prod"},
      {"DBInstanceIdentifier": "production-read-only-replica", "Engine": "postgres", "EngineVersion": "16.10",
       "DBInstanceStatus": "available", "DBInstanceClass": "db.t4g.medium",
       "Endpoint": {"Address": "production-read-only-replica.x.us-west-1.rds.amazonaws.com", "Port": 5432},
       "ReadReplicaSourceDBInstanceIdentifier": "production", "DBName": "prod"},
      {"DBInstanceIdentifier": "reports", "Engine": "mysql", "EngineVersion": "8.0",
       "DBInstanceStatus": "available", "DBInstanceClass": "db.t4g.small"},
      {"Engine": "postgres"}
    ]}"#;

    #[test]
    fn parses_instances_and_their_replica_links() {
        let all = parse_describe(FIXTURE).unwrap();
        assert_eq!(all.len(), 6, "the entry without an identifier is skipped");
        let demo = &all[0];
        assert_eq!(demo.replicas, ["demo-read-only-replica"]);
        assert_eq!(demo.port, Some(5432));
        assert_eq!(all[1].source.as_deref(), Some("demo"));
        assert_eq!(all[2].status, "stopped");
        assert!(all[3].multi_az);
        assert_eq!(all[0].vpc_id.as_deref(), Some("vpc-0a1b2c3d"));
        assert_eq!(all[2].vpc_id, None);
        assert!(!all[5].is_postgres());
        assert!(parse_describe("{}").is_err());
        assert!(parse_describe("not json").is_err());
    }

    #[test]
    fn globs_match_with_star_only() {
        assert!(glob_match("demo", "demo"));
        assert!(!glob_match("demo", "demo-read-only-replica"));
        assert!(glob_match("demo*", "demo-read-only-replica"));
        assert!(glob_match("*replica", "demo-read-only-replica"));
        assert!(glob_match("prod*replica", "production-read-only-replica"));
        assert!(!glob_match("prod*replica", "production"));
        assert!(glob_match("*", "anything"));
        assert!(
            !glob_match("ab*ba", "aba"),
            "prefix and suffix must not overlap"
        );
    }

    #[test]
    fn select_pulls_in_replicas_and_groups_them_under_their_primary() {
        let all = parse_describe(FIXTURE).unwrap();
        let ids = |p: &[&str]| {
            let p: Vec<String> = p.iter().map(|s| s.to_string()).collect();
            select(&all, &p)
                .iter()
                .map(|i| i.id.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(ids(&["demo"]), ["demo", "demo-read-only-replica"]);
        assert_eq!(
            ids(&["production-read-only-replica", "production"]),
            ["production", "production-read-only-replica"]
        );
        assert_eq!(
            ids(&["*replica"]),
            ["demo-read-only-replica", "production-read-only-replica"]
        );
        assert_eq!(ids(&["reports"]), Vec::<String>::new(), "not postgres");
        assert_eq!(
            ids(&["*"]),
            [
                "demo",
                "demo-read-only-replica",
                "development",
                "production",
                "production-read-only-replica"
            ]
        );
    }

    #[test]
    fn describe_asks_for_json_in_the_scope() {
        let scope = AwsScope {
            region: Some("us-west-1".into()),
            ..AwsScope::default()
        };
        let a = describe_args(&scope).join(" ");
        assert!(
            a.starts_with("rds describe-db-instances --output json"),
            "{a}"
        );
        assert!(a.contains("--region us-west-1"), "{a}");
    }

    /// What the CLI returned for 16.10 on 2026-10-19, trimmed.
    const UPGRADES: &str = r#"{"DBEngineVersions": [{"Engine": "postgres", "EngineVersion": "16.10",
      "ValidUpgradeTarget": [
        {"EngineVersion": "16.11", "IsMajorVersionUpgrade": false},
        {"EngineVersion": "16.15", "IsMajorVersionUpgrade": false},
        {"EngineVersion": "16.9", "IsMajorVersionUpgrade": false},
        {"EngineVersion": "17.10", "IsMajorVersionUpgrade": true},
        {"EngineVersion": "17.9", "IsMajorVersionUpgrade": true},
        {"EngineVersion": "18.6", "IsMajorVersionUpgrade": true},
        {"EngineVersion": "18.10", "IsMajorVersionUpgrade": true}
      ]}]}"#;

    #[test]
    fn the_newest_minor_and_major_are_picked_by_number_not_text() {
        let u = parse_upgrades(UPGRADES).unwrap();
        assert_eq!(u.minor.as_deref(), Some("16.15"));
        assert_eq!(
            u.major.as_deref(),
            Some("18.10"),
            "18.10 is newer than 18.6"
        );
        assert_eq!(u.label().as_deref(), Some("↑16.15 · 18.10"));
        let none = parse_upgrades(r#"{"DBEngineVersions": [{"ValidUpgradeTarget": []}]}"#).unwrap();
        assert_eq!(none.label(), None, "already the newest");
        let major_only = Upgrades {
            minor: None,
            major: Some("18.6".into()),
        };
        assert_eq!(major_only.label().as_deref(), Some("↑18.6"));
        assert!(parse_upgrades("nope").is_err());
        let a = engine_versions_args(&AwsScope::default(), "postgres", "16.10").join(" ");
        assert!(
            a.starts_with(
                "rds describe-db-engine-versions --engine postgres --engine-version 16.10"
            ),
            "{a}"
        );
    }
}
