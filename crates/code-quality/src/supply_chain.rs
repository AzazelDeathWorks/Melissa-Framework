//! Standard CycloneDX document from resolved Cargo metadata. Unknown license
//! information is a blocker, not an inferred license or a distribution waiver.
use crate::{Error, Result, digest, source::FrozenSource};
use melissa_repository::{Limits, Repository};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    sync::atomic::AtomicBool,
};
pub struct CargoTool {
    executable: PathBuf,
    sha256: String,
}
impl CargoTool {
    /// Explicit trusted-host executable pin. Never accepted from a model tool.
    pub fn pin(executable: PathBuf) -> Result<Self> {
        if !executable.is_absolute() || !executable.is_file() {
            return Err(Error::Invalid("absolute Cargo executable"));
        }
        let bytes = fs::read(&executable).map_err(|_| Error::Denied("Cargo executable"))?;
        Ok(Self {
            executable,
            sha256: format!("{:x}", Sha256::digest(&bytes)),
        })
    }
    fn validate(&self) -> Result {
        let b = fs::read(&self.executable).map_err(|_| Error::Denied("Cargo executable"))?;
        if format!("{:x}", Sha256::digest(b)) != self.sha256 {
            return Err(Error::Stale("Cargo executable pin"));
        }
        Ok(())
    }
}
#[derive(serde::Serialize)]
pub struct SupplyChainEvidence {
    source_digest: String,
    lock_digest: String,
    metadata_digest: String,
    document: Value,
    blockers: Vec<String>,
    tool_digest: String,
    notices: String,
}
impl SupplyChainEvidence {
    pub fn document(&self) -> &Value {
        &self.document
    }
    pub fn blockers(&self) -> &[String] {
        &self.blockers
    }
    pub fn notices(&self) -> &str {
        &self.notices
    }
    pub fn source_digest(&self) -> &str {
        &self.source_digest
    }
    pub fn digest(&self) -> Result<String> {
        Ok(digest(&serde_json::to_vec(self)?))
    }
}
pub fn capture_cargo(
    source: &FrozenSource,
    repo: &Repository,
    tool: &CargoTool,
    cancel: &AtomicBool,
) -> Result<SupplyChainEvidence> {
    source.current(repo, cancel)?;
    tool.validate()?;
    // Cargo metadata is data inspection only, not `cargo build` or test execution.
    // Reject project-local executable/config overrides before host inspection.
    if source.snapshot().files.iter().any(|f| {
        f.path
            .split('/')
            .any(|c| c == ".cargo" || c == "rust-toolchain" || c == "rust-toolchain.toml")
    }) {
        return Err(Error::Denied("Cargo config/toolchain overrides"));
    }
    let output = crate::host_process::capture(
        &tool.executable,
        &["metadata", "--format-version=1", "--locked", "--offline"].map(str::to_owned),
        repo.root_path(),
        cancel,
    )?;
    if output.exit_code != 0 {
        return Err(Error::Invalid("Cargo metadata failed or oversized"));
    }
    let value = melissa_model_gateway::strict_json(
        std::str::from_utf8(&output.stdout)
            .map_err(|_| Error::Invalid("Cargo metadata encoding"))?,
    )
    .map_err(|_| Error::Invalid("Cargo metadata JSON"))?;
    let mut evidence = from_metadata(source, &value, digest(&output.stdout), tool.sha256.clone())?;
    let registry = std::env::var_os("CARGO_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("USERPROFILE").map(|p| PathBuf::from(p).join(".cargo")))
        .ok_or(Error::Denied("explicit Cargo registry location required"))?
        .join("registry/src");
    for p in value["packages"].as_array().unwrap() {
        let manifest = Path::new(
            p["manifest_path"]
                .as_str()
                .ok_or(Error::Invalid("manifest location"))?,
        );
        let directory = manifest.parent().ok_or(Error::Invalid("manifest parent"))?;
        let in_workspace = directory.starts_with(repo.root_path());
        if !in_workspace && !directory.starts_with(&registry) {
            return Err(Error::Denied(
                "license outside workspace or approved Cargo registry",
            ));
        }
        let license_root = if in_workspace {
            repo.root_path()
        } else {
            directory
        };
        match license_files(license_root).or_else(|_| upstream_license_files(source, p, directory))
        {
            Ok(files) => {
                evidence.notices.push_str(&format!(
                    "\n=== {} {} ===\n",
                    p["name"].as_str().unwrap(),
                    p["version"].as_str().unwrap()
                ));
                for (name, bytes) in files {
                    evidence
                        .notices
                        .push_str(&format!("\n--- {name}; {} ---\n", digest(&bytes)));
                    evidence.notices.push_str(
                        std::str::from_utf8(&bytes)
                            .map_err(|_| Error::Invalid("license text UTF8"))?,
                    );
                    evidence.notices.push('\n');
                }
            }
            Err(_) => evidence.blockers.push(format!(
                "{}: license/notice text unavailable",
                p["name"].as_str().unwrap()
            )),
        }
    }
    source.current(repo, cancel)?;
    tool.validate()?;
    Ok(evidence)
}
fn from_metadata(
    source: &FrozenSource,
    value: &Value,
    metadata_digest: String,
    tool_digest: String,
) -> Result<SupplyChainEvidence> {
    let lock: toml::Value = toml::from_str(
        std::str::from_utf8(source.bytes("Cargo.lock")?)
            .map_err(|_| Error::Invalid("lock encoding"))?,
    )
    .map_err(|_| Error::Invalid("lock TOML"))?;
    let locked = lock["package"]
        .as_array()
        .ok_or(Error::Invalid("lock packages"))?;
    let packages = value["packages"]
        .as_array()
        .ok_or(Error::Invalid("Cargo packages"))?;
    let nodes = value["resolve"]["nodes"]
        .as_array()
        .ok_or(Error::Invalid("resolved dependency graph required"))?;
    if packages.is_empty() || packages.len() > 10000 || packages.len() != nodes.len() {
        return Err(Error::Invalid("metadata resolved coverage"));
    }
    let ids: BTreeSet<_> = packages.iter().filter_map(|p| p["id"].as_str()).collect();
    if ids.len() != packages.len() {
        return Err(Error::Invalid("duplicate package identities"));
    }
    let refs: BTreeMap<_, _> = ids
        .iter()
        .map(|id| {
            (
                *id,
                format!("urn:melissa:component:{}", blake3::hash(id.as_bytes())),
            )
        })
        .collect();
    let mut components = vec![];
    let mut blockers = vec![];
    for p in packages {
        let name = p["name"].as_str().ok_or(Error::Invalid("package name"))?;
        let version = p["version"]
            .as_str()
            .ok_or(Error::Invalid("package version"))?;
        let id = p["id"].as_str().unwrap();
        // Cargo is the source of the declared SPDX expression. No legal
        // compatibility decision is inferred from its mere presence.
        let license = p["license"].as_str().filter(|s| !s.trim().is_empty());
        if !license.is_some_and(license_allowed) {
            blockers.push(format!(
                "{name}@{version}: missing, invalid or unapproved SPDX expression"
            ));
        }
        let licenses = license
            .map(|s| json!([{"expression":s.replace('/'," OR ")}]))
            .unwrap_or(json!([]));
        let mut component = json!({"type":"library","bom-ref":refs[id],"name":name,"version":version,"licenses":licenses,"properties":[{"name":"melissa:source","value":p["source"].as_str().unwrap_or("workspace/path")}]});
        if p["source"]
            .as_str()
            .is_some_and(|s| s.starts_with("registry+"))
        {
            component["purl"] = json!(format!("pkg:cargo/{name}@{version}"));
            let matches: Vec<_> = locked
                .iter()
                .filter(|l| {
                    l["name"].as_str() == Some(name)
                        && l["version"].as_str() == Some(version)
                        && l.get("source").and_then(toml::Value::as_str) == p["source"].as_str()
                })
                .collect();
            let checksum = matches
                .first()
                .and_then(|l| l.get("checksum"))
                .and_then(toml::Value::as_str)
                .filter(|s| s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit()));
            if matches.len() != 1 || checksum.is_none() {
                return Err(Error::Invalid("registry package lock checksum binding"));
            }
            component["hashes"] = json!([{"alg":"SHA-256","content":checksum.unwrap()}]);
        }
        components.push(component);
    }
    let mut dependencies = vec![];
    let mut node_ids = BTreeSet::new();
    for n in nodes {
        let id = n["id"].as_str().ok_or(Error::Invalid("dependency id"))?;
        if !ids.contains(id) || !node_ids.insert(id) {
            return Err(Error::Invalid("unknown or duplicate dependency node"));
        }
        let deps = n["dependencies"]
            .as_array()
            .ok_or(Error::Invalid("dependency edges"))?;
        if deps
            .iter()
            .any(|d| !d.as_str().is_some_and(|s| ids.contains(s)))
        {
            return Err(Error::Invalid("unresolved dependency edge"));
        }
        dependencies.push(json!({"ref":refs[id],"dependsOn":deps.iter().map(|d|&refs[d.as_str().unwrap()]).collect::<Vec<_>>()}));
    }
    let lock_digest = digest(source.bytes("Cargo.lock")?);
    let document = json!({"bomFormat":"CycloneDX","specVersion":"1.6","version":1,"components":components,"dependencies":dependencies,"metadata":{"properties":[{"name":"melissa:source-digest","value":source.snapshot().digest},{"name":"melissa:lock-digest","value":lock_digest},{"name":"melissa:scope","value":"resolved Cargo metadata; declared licenses; legal distribution review required"}]}});
    Ok(SupplyChainEvidence {
        source_digest: source.snapshot().digest.clone(),
        lock_digest,
        metadata_digest,
        document,
        blockers,
        tool_digest,
        notices: String::new(),
    })
}
#[derive(serde::Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct UpstreamLicense {
    schema_version: String,
    name: String,
    version: String,
    crate_checksum: String,
    repository: String,
    revision: String,
    files: Vec<UpstreamLicenseFile>,
}
#[derive(serde::Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct UpstreamLicenseFile {
    path: String,
    url: String,
    sha256: String,
}
fn upstream_license_files(
    source: &FrozenSource,
    package: &Value,
    directory: &Path,
) -> Result<Vec<(String, Vec<u8>)>> {
    let name = package["name"]
        .as_str()
        .ok_or(Error::Invalid("license package"))?;
    let version = package["version"]
        .as_str()
        .ok_or(Error::Invalid("license version"))?;
    let prefix = format!("third_party/licenses/{name}/{version}/");
    let bytes = source.bytes(&format!("{prefix}manifest.json"))?;
    let json = melissa_model_gateway::strict_json(
        std::str::from_utf8(bytes).map_err(|_| Error::Invalid("license manifest encoding"))?,
    )
    .map_err(|_| Error::Invalid("license manifest JSON"))?;
    let manifest: UpstreamLicense = serde_json::from_value(json)?;
    let protected_pins: Vec<UpstreamLicense> = serde_json::from_str(include_str!(
        "../../../tools/code11/upstream-license-pins.json"
    ))?;
    if !protected_pins.contains(&manifest) {
        return Err(Error::Denied("license override lacks protected host pin"));
    }
    let repo = Repository::open(directory, Limits::default(), vec![])?;
    let vcs: Value = serde_json::from_slice(&repo.read(".cargo_vcs_info.json")?.1)?;
    let lock: toml::Value = toml::from_str(
        std::str::from_utf8(source.bytes("Cargo.lock")?)
            .map_err(|_| Error::Invalid("lock encoding"))?,
    )
    .map_err(|_| Error::Invalid("lock TOML"))?;
    let checksum = lock["package"]
        .as_array()
        .and_then(|a| {
            a.iter().find(|p| {
                p["name"].as_str() == Some(name)
                    && p["version"].as_str() == Some(version)
                    && p.get("source").and_then(toml::Value::as_str) == package["source"].as_str()
            })
        })
        .and_then(|p| p.get("checksum"))
        .and_then(toml::Value::as_str);
    if manifest.schema_version != "melissa.upstream-license/v1"
        || manifest.name != name
        || manifest.version != version
        || Some(manifest.crate_checksum.as_str()) != checksum
        || package["repository"]
            .as_str()
            .map(|s| s.trim_end_matches('/').trim_end_matches(".git"))
            != Some(manifest.repository.as_str())
        || vcs["git"]["sha1"].as_str() != Some(manifest.revision.as_str())
        || manifest.revision.len() != 40
        || !manifest.revision.bytes().all(|b| b.is_ascii_hexdigit())
        || !manifest.repository.starts_with("https://github.com/")
        || manifest.files.is_empty()
        || manifest.files.len() > 32
    {
        return Err(Error::Denied("upstream license provenance mismatch"));
    }
    let url_prefix = format!(
        "https://raw.githubusercontent.com/{}/{}/",
        manifest
            .repository
            .trim_start_matches("https://github.com/"),
        manifest.revision
    );
    let mut out = vec![];
    for file in manifest.files {
        if !file.path.starts_with(&prefix)
            || !file.url.starts_with(&url_prefix)
            || file.url[url_prefix.len()..].contains(['?', '#'])
        {
            return Err(Error::Denied("upstream license source path"));
        }
        let content = source.bytes(&file.path)?;
        if format!("{:x}", Sha256::digest(content)) != file.sha256 {
            return Err(Error::Stale("upstream license text changed"));
        }
        out.push((format!("{} [{}]", file.path, file.url), content.to_vec()));
    }
    Ok(out)
}
pub fn license_allowed(expression: &str) -> bool {
    let normalized = expression.replace('/', " OR ");
    spdx::Expression::parse(&normalized).is_ok_and(|e| {
        e.evaluate(|r| {
            r.addition.is_none()
                && r.license.id().is_some_and(|id| {
                    matches!(
                        id.name,
                        "MIT"
                            | "MIT-0"
                            | "CDLA-Permissive-2.0"
                            | "Apache-2.0"
                            | "BSD-2-Clause"
                            | "BSD-3-Clause"
                            | "ISC"
                            | "Zlib"
                            | "CC0-1.0"
                            | "Unicode-3.0"
                            | "Unlicense"
                            | "BSL-1.0"
                            | "0BSD"
                    )
                })
        })
    })
}
/// Copy license/notice files only from an explicit host-selected dependency
/// directory, with the same link/path rejection used for all repository reads.
pub fn license_files(root: &Path) -> Result<Vec<(String, Vec<u8>)>> {
    let repo = Repository::open(root, melissa_repository::Limits::default(), vec![])?;
    let mut out = vec![];
    for entry in fs::read_dir(root).map_err(|_| Error::Denied("license directory"))? {
        let entry = entry.map_err(|_| Error::Denied("license entry"))?;
        let file_name = entry.file_name();
        let path = file_name
            .to_str()
            .ok_or(Error::Invalid("license name UTF8"))?;
        let name = path.to_ascii_uppercase();
        let license_name = name.starts_with("LICENSE")
            || name.starts_with("COPYING")
            || name.starts_with("NOTICE")
            || name.starts_with("COPYRIGHT")
            || name == "AUTHORS";
        if !license_name {
            continue;
        }
        if entry
            .file_type()
            .map_err(|_| Error::Denied("license metadata"))?
            .is_file()
        {
            let bytes = repo.read(path)?.1;
            if name != "AUTHORS"
                || std::str::from_utf8(&bytes)
                    .is_ok_and(|t| t.contains("Permission is hereby granted"))
            {
                out.push((path.to_owned(), bytes));
            }
        }
    }
    if out.is_empty() {
        return Err(Error::Denied("dependency license/notice text missing"));
    }
    Ok(out)
}
#[cfg(all(test, windows))]
mod tests {
    use super::*;
    #[test]
    fn upstream_license_requires_protected_pin_exact_revision_checksum_and_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let candidate = dir.path().join("source");
        let cache = dir.path().join("cache");
        fs::create_dir(&candidate).unwrap();
        fs::create_dir(&cache).unwrap();
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap();
        let pins: Value = serde_json::from_str(include_str!(
            "../../../tools/code11/upstream-license-pins.json"
        ))
        .unwrap();
        let approved = pins[0].clone();
        let name = approved["name"].as_str().unwrap();
        let version = approved["version"].as_str().unwrap();
        let prefix = format!("third_party/licenses/{name}/{version}");
        fs::create_dir_all(candidate.join(&prefix)).unwrap();
        for file in approved["files"].as_array().unwrap() {
            let path = file["path"].as_str().unwrap();
            fs::copy(root.join(path), candidate.join(path)).unwrap();
        }
        fs::copy(root.join("Cargo.lock"), candidate.join("Cargo.lock")).unwrap();
        let manifest = candidate.join(&prefix).join("manifest.json");
        fs::write(&manifest, serde_json::to_vec(&approved).unwrap()).unwrap();
        fs::write(
            cache.join(".cargo_vcs_info.json"),
            serde_json::to_vec(&json!({"git":{"sha1":approved["revision"]}})).unwrap(),
        )
        .unwrap();
        let package = json!({"name":name,"version":version,"repository":approved["repository"],"source":"registry+https://github.com/rust-lang/crates.io-index"});
        let repo = Repository::open(&candidate, Limits::default(), vec![]).unwrap();
        let no = AtomicBool::new(false);
        let source = FrozenSource::capture(&repo, &no).unwrap();
        assert!(
            !upstream_license_files(&source, &package, &cache)
                .unwrap()
                .is_empty()
        );
        let file = approved["files"][0]["path"].as_str().unwrap();
        fs::write(candidate.join(file), b"invented license").unwrap();
        let source = FrozenSource::capture(&repo, &no).unwrap();
        assert!(upstream_license_files(&source, &package, &cache).is_err());
        fs::copy(root.join(file), candidate.join(file)).unwrap();
        let mut forged = approved;
        forged["revision"] = json!("0".repeat(40));
        fs::write(manifest, serde_json::to_vec(&forged).unwrap()).unwrap();
        let source = FrozenSource::capture(&repo, &no).unwrap();
        assert!(upstream_license_files(&source, &package, &cache).is_err());
    }
}
