use indexmap::IndexMap;
use miette::{miette, Context, IntoDiagnostic};
use pretty_yaml::config::FormatOptions;
use serde::{Deserialize, Serialize};
use std::{fs, io::Write, path::PathBuf};
use walkdir::WalkDir;

use crate::{manifest::Manifest, package::PackageStore, resolver::DependencyGraph};

use super::path_util::PathUtil;

const BUF_YAML_FILE: &str = "buf.yaml";

/// Representation of a Buf YAML file
pub struct BufYamlFile {
    config: Config,
    /// The configuration as read from disk, if it was loaded from a file
    on_disk: Option<Config>,
    buf_yaml_path: PathBuf,
    proto_rel_path: String,
    vendor_rel_path: String,
}

impl BufYamlFile {
    /// Create default `BufYamlFile` with default values
    pub fn new(store: &PackageStore) -> miette::Result<Self> {
        Self::new_from_str(DEFAULT_YAML, store)
    }

    /// Create a new `BufYamlFile` from a string
    pub fn new_from_str(s: &str, store: &PackageStore) -> miette::Result<Self> {
        let config: Config = serde_norway::from_str(s).into_diagnostic()?;

        // Version needs to be v2
        if config.version != "v2" {
            return Err(miette!("Only v2 is supported for buf.yaml files"));
        }

        let proto_path = store.proto_path();
        let proto_vendor_path = store.proto_vendor_path();
        let buf_yaml_path = proto_path.join(BUF_YAML_FILE);
        let proto_rel_path = ".".to_owned();
        let vendor_rel_path = proto_vendor_path
            .relative_to(&proto_path)
            .unwrap_or(proto_vendor_path.clone())
            .to_posix_string();

        Ok(Self {
            config,
            on_disk: None,
            buf_yaml_path,
            proto_rel_path,
            vendor_rel_path,
        })
    }

    /// Load `BufYamlFile` from buf.yaml file
    pub fn from_file(store: &PackageStore) -> miette::Result<Self> {
        let proto_path = store.proto_path();
        let buf_yaml_path = proto_path.join(BUF_YAML_FILE);
        let yaml_content = fs::read_to_string(buf_yaml_path).into_diagnostic()?;
        let mut buf_yaml = Self::new_from_str(&yaml_content, store)?;
        buf_yaml.on_disk = Some(buf_yaml.config.clone());
        Ok(buf_yaml)
    }

    /// Whether the configuration still matches the file it was loaded from.
    ///
    /// Map comparisons ignore key order, as buf does.
    pub fn is_unchanged(&self) -> bool {
        self.on_disk.as_ref() == Some(&self.config)
    }

    /// Serialize the `BufYamlFile` to a string
    pub fn to_string(&self) -> miette::Result<String> {
        let yaml = serde_norway::to_string(&self.config).into_diagnostic()?;

        // prettyfy the output
        let options = FormatOptions::default();
        pretty_yaml::format_text(&yaml, &options).into_diagnostic()
    }

    /// Write `BufYamlFile` to a YAML file
    pub fn to_file(&self) -> miette::Result<()> {
        let yaml_content = self.to_string()?;
        let mut file = fs::File::create(&self.buf_yaml_path).into_diagnostic()?;
        file.write_all(yaml_content.as_bytes()).into_diagnostic()?;
        Ok(())
    }

    /// Clear all modules from the Buf YAML file
    pub fn clear_modules(&mut self) {
        self.config.modules.clear();
    }

    /// Add non-vendor module to the Buf YAML file
    pub fn add_module(&mut self) {
        self.config.modules.push(Module {
            path: self.proto_rel_path.clone(),
            excludes: vec![self.vendor_rel_path.clone()],
            ..Default::default()
        });
    }

    /// Add vendor modules to the Buf YAML file
    pub fn set_vendor_modules(&mut self, vendor_modules: Vec<String>) {
        // Add vendor modules
        for module in vendor_modules {
            self.config.modules.push(Module {
                path: format!("{}/{}", self.vendor_rel_path, module),
                ..Default::default()
            });
        }
    }
}

/// Generates a buf.yaml file matching the current dependency graph
pub fn generate_buf_yaml_file(
    dependency_graph: &DependencyGraph,
    manifest: &Manifest,
    store: &PackageStore,
) -> miette::Result<()> {
    // The file will be created in the "store" directory
    let store_dir = store.proto_path();
    let buf_yaml_file = store_dir.join(BUF_YAML_FILE);

    let mut buf_yaml = if buf_yaml_file.exists() {
        BufYamlFile::from_file(store).wrap_err(miette!(
            "failed to read buf.yaml file at {}.",
            store_dir.display()
        ))?
    } else {
        BufYamlFile::new(store)?
    };

    let vendor_modules: Vec<String> = dependency_graph.vendor_module_names();
    buf_yaml.clear_modules();

    if manifest.package.is_some() {
        // double-check that the package really contains proto files
        // under proto/** (but not under proto/vendor/**)
        let vendor_path = store.proto_vendor_path();
        let mut has_protos = false;
        for entry in WalkDir::new(store.proto_path()).into_iter().flatten() {
            if entry.path().is_file() {
                let path = entry.path();
                if path.starts_with(&vendor_path) {
                    continue;
                }

                if path.extension().is_some_and(|ext| ext == "proto") {
                    has_protos = true;
                    break;
                }
            }
        }

        if has_protos {
            buf_yaml.add_module();
        }
    }
    buf_yaml.set_vendor_modules(vendor_modules);

    // Leave an up-to-date file alone, so that its comments and formatting
    // survive and its modification time does not change.
    if buf_yaml.is_unchanged() {
        tracing::debug!("{} is up to date", buf_yaml_file.display());
        return Ok(());
    }

    buf_yaml
        .to_file()
        .wrap_err(miette!("failed to write buf.yaml file"))?;
    Ok(())
}

/// Default buf.yaml file
const DEFAULT_YAML: &str = r"
version: v2

modules:
lint:
  use:
    - STANDARD
  except:
    - PACKAGE_VERSION_SUFFIX
breaking:
  use:
    - FILE
deps:
  - buf.build/googleapis/googleapis
  - buf.build/grpc/grpc
";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct Config {
    version: String,
    modules: Vec<Module>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    deps: Vec<String>,
    lint: Option<LintConfig>,
    breaking: Option<BreakingConfig>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    plugins: Vec<Plugin>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
struct Module {
    path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    excludes: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    includes: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    lint: Option<LintConfig>,
    #[serde(default, skip_serializing_if = "is_false")]
    disallow_comment_ignores: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    enum_zero_value_suffix: Option<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    rpc_allow_same_request_response: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    rpc_allow_google_protobuf_empty_requests: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    rpc_allow_google_protobuf_empty_responses: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    service_suffix: Option<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    disable_builtin: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    breaking: Option<BreakingConfig>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct LintConfig {
    #[serde(rename = "use", default, skip_serializing_if = "Vec::is_empty")]
    use_rules: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    except: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    ignore: Vec<String>,
    /// Kept in file order, so that rewriting the file does not reorder it
    #[serde(default, skip_serializing_if = "IndexMap::is_empty")]
    ignore_only: IndexMap<String, Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct BreakingConfig {
    #[serde(rename = "use", default, skip_serializing_if = "Vec::is_empty")]
    use_rules: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    except: Vec<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    ignore_unstable_packages: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    disable_builtin: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct Plugin {
    plugin: String,
    /// Kept in file order, so that rewriting the file does not reorder it
    #[serde(default, skip_serializing_if = "IndexMap::is_empty")]
    options: IndexMap<String, String>,
}

fn is_false(b: impl std::borrow::Borrow<bool>) -> bool {
    !b.borrow()
}

#[cfg(test)]
mod tests {
    use assert_fs::TempDir;
    use std::io::Write;

    use super::*;

    /// Seven top-level `ignore_only` keys and seven plugin options, deliberately
    /// not in alphabetical order, so that a hash-ordered or sorted
    /// serialisation matching the file order by luck is improbable.
    const UNSORTED_MAPS_YAML: &str = r#"
version: v2
modules:
  - path: "."
    excludes:
      - vendor
    lint:
      ignore_only:
        SERVICE_SUFFIX:
          - api/m
        ENUM_ZERO_VALUE_SUFFIX:
          - api/z
lint:
  use:
    - STANDARD
  ignore_only:
    RPC_RESPONSE_STANDARD_NAME:
      - api/a
    PACKAGE_VERSION_SUFFIX:
      - api/b
    ENUM_VALUE_PREFIX:
      - api/c
    RPC_REQUEST_STANDARD_NAME:
      - api/d
    FIELD_LOWER_SNAKE_CASE:
      - api/e
    RPC_REQUEST_RESPONSE_UNIQUE:
      - api/f
    COMMENT_FIELD:
      - api/g
plugins:
  - plugin: plugin-x
    options:
      zeta: "1"
      alpha: "2"
      mu: "3"
      beta: "4"
      omega: "5"
      gamma: "6"
      delta: "7"
"#;

    const LINT_IGNORE_ONLY_ORDER: [&str; 7] = [
        "RPC_RESPONSE_STANDARD_NAME",
        "PACKAGE_VERSION_SUFFIX",
        "ENUM_VALUE_PREFIX",
        "RPC_REQUEST_STANDARD_NAME",
        "FIELD_LOWER_SNAKE_CASE",
        "RPC_REQUEST_RESPONSE_UNIQUE",
        "COMMENT_FIELD",
    ];

    const PLUGIN_OPTION_ORDER: [&str; 7] =
        ["zeta", "alpha", "mu", "beta", "omega", "gamma", "delta"];

    fn reserialize(yaml: &str) -> String {
        let tmp_dir = TempDir::new().unwrap();
        let store = PackageStore::new(tmp_dir.path().to_owned());
        BufYamlFile::new_from_str(yaml, &store)
            .unwrap()
            .to_string()
            .unwrap()
    }

    /// Keys of a YAML mapping, in document order.
    fn keys_in_document_order(mapping: &serde_norway::Value) -> Vec<String> {
        mapping
            .as_mapping()
            .unwrap()
            .keys()
            .map(|k| k.as_str().unwrap().to_owned())
            .collect()
    }

    fn parse(yaml: &str) -> serde_norway::Value {
        serde_norway::from_str(yaml).unwrap()
    }

    #[test]
    fn test_reserialized_ignore_only_keeps_file_order() {
        // Each parse builds a fresh map; repeat to defeat a lucky hash order.
        for _ in 0..8 {
            let out = parse(&reserialize(UNSORTED_MAPS_YAML));
            assert_eq!(
                keys_in_document_order(&out["lint"]["ignore_only"]),
                LINT_IGNORE_ONLY_ORDER
            );
            assert_eq!(
                keys_in_document_order(&out["modules"][0]["lint"]["ignore_only"]),
                ["SERVICE_SUFFIX", "ENUM_ZERO_VALUE_SUFFIX"]
            );
        }
    }

    #[test]
    fn test_reserialized_plugin_options_keep_file_order() {
        for _ in 0..8 {
            let out = parse(&reserialize(UNSORTED_MAPS_YAML));
            assert_eq!(
                keys_in_document_order(&out["plugins"][0]["options"]),
                PLUGIN_OPTION_ORDER
            );
        }
    }

    #[test]
    fn test_reserializing_canonical_output_is_byte_identical() {
        let first = reserialize(UNSORTED_MAPS_YAML);
        for _ in 0..8 {
            assert_eq!(reserialize(&first), first);
        }
    }

    #[test]
    fn test_new_from_str() {
        let tmp_dir = TempDir::new().unwrap();
        let store = PackageStore::new(tmp_dir.path().to_owned());
        let buf_yaml = BufYamlFile::new_from_str(SAMPLE_YAML, &store).unwrap();

        // serialize to file
        let serialized = buf_yaml.to_string().unwrap();

        let yaml_path = tmp_dir.join(BUF_YAML_FILE);
        let writer = std::fs::File::create(yaml_path).unwrap();
        let mut writer = std::io::BufWriter::new(writer);
        writer.write_all(serialized.as_bytes()).unwrap();
    }

    const SAMPLE_YAML: &str = r#"
version: v2
modules:
  - path: proto/foo
    name: buf.build/acme/foo
  - path: proto/bar
    name: buf.build/acme/bar
    excludes:
      - proto/bar/a
      - proto/bar/b/e.proto
    lint:
      use:
        - STANDARD
      except:
        - IMPORT_USED
      ignore:
        - proto/bar/c
      ignore_only:
        ENUM_ZERO_VALUE_SUFFIX:
          - proto/bar/a
          - proto/bar/b/f.proto
    disallow_comment_ignores: false
    enum_zero_value_suffix: _UNSPECIFIED
    rpc_allow_same_request_response: false
    rpc_allow_google_protobuf_empty_requests: false
    rpc_allow_google_protobuf_empty_responses: false
    service_suffix: Service
    disable_builtin: false
    breaking:
      use:
        - FILE
      except:
        - EXTENSION_MESSAGE_NO_DELETE
      ignore_unstable_packages: true
      disable_builtin: false
  - path: proto/common
    module: buf.build/acme/weather
    includes:
      - proto/common/weather
  - path: proto/common
    module: buf.build/acme/location
    includes:
      - proto/common/location
    excludes:
      - proto/common/location/test
  - path: proto/common
    module: buf.build/acme/other
    excludes:
      - proto/common/location
      - proto/common/weather
deps:
  - buf.build/acme/paymentapis
  - buf.build/acme/pkg:47b927cbb41c4fdea1292bafadb8976f
  - buf.build/googleapis/googleapis:v1beta1.1.0
lint:
  use:
    - STANDARD
    - TIMESTAMP_SUFFIX
breaking:
  use:
    - FILE
plugins:
  - plugin: plugin-timestamp-suffix
    options:
      timestamp_suffix: _time
"#;
}
