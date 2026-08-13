use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::sync::OnceLock;

const BUNDLED_CATALOG_JSON: &str = include_str!("../definitions/cache-catalog-v1.json");

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum Platform {
    Macos,
    Windows,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum CacheCategory {
    BrowserCache,
    MediaCache,
    OperatingSystemCache,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum Confidence {
    High,
    Medium,
    Low,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum CachePathRoot {
    Home,
    LocalAppData,
    RoamingAppData,
    SystemCache,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum CachePathSource {
    Fixed,
    Configuration,
    UserSpecified,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CachePathRule {
    pub root: CachePathRoot,
    pub relative_path: String,
    pub source: CachePathSource,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CacheDefinition {
    pub id: String,
    pub definition_version: u32,
    pub platform: Platform,
    pub application_name: String,
    pub version_constraint: String,
    pub category: CacheCategory,
    pub path: CachePathRule,
    pub confidence: Confidence,
    pub evidence: Vec<String>,
    pub regenerable: bool,
    pub cleanup_impact: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CacheCatalog {
    pub catalog_version: String,
    pub definitions: Vec<CacheDefinition>,
}

impl CacheCatalog {
    pub fn parse(json: &str) -> Result<Self, String> {
        let catalog: Self = serde_json::from_str(json)
            .map_err(|error| format!("キャッシュ定義を読み取れません: {error}"))?;
        catalog.validate()?;
        Ok(catalog)
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.catalog_version.trim().is_empty() {
            return Err("キャッシュ定義のカタログバージョンが空です".to_owned());
        }
        if self.definitions.is_empty() {
            return Err("キャッシュ定義がありません".to_owned());
        }
        let mut ids = HashSet::new();
        for definition in &self.definitions {
            if definition.id.trim().is_empty() || !ids.insert(definition.id.as_str()) {
                return Err(format!("キャッシュ定義IDが空または重複しています: {}", definition.id));
            }
            if definition.definition_version == 0
                || definition.application_name.trim().is_empty()
                || definition.version_constraint.trim().is_empty()
                || definition.evidence.is_empty()
                || definition.evidence.iter().any(|value| value.trim().is_empty())
                || definition.cleanup_impact.trim().is_empty()
            {
                return Err(format!("キャッシュ定義の必須項目が不足しています: {}", definition.id));
            }
            relative_components(&definition.path.relative_path).map_err(|reason| {
                format!("キャッシュ定義の相対パスが不正です ({}): {reason}", definition.id)
            })?;
        }
        Ok(())
    }

    pub fn classify(
        &self,
        platform: Platform,
        root: CachePathRoot,
        relative_path: &str,
    ) -> Vec<&CacheDefinition> {
        self.definitions
            .iter()
            .filter(|definition| {
                definition.platform == platform
                    && definition.path.root == root
                    && path_has_component_prefix(
                        &definition.path.relative_path,
                        relative_path,
                        platform,
                    )
            })
            .collect()
    }
}

fn relative_components(path: &str) -> Result<Vec<&str>, &'static str> {
    if path.is_empty() || path.starts_with('/') || path.starts_with('\\') || path.contains('\\') {
        return Err("区切り文字は相対パスの / のみ使用できます");
    }
    let components: Vec<_> = path.split('/').collect();
    if components
        .iter()
        .any(|component| component.is_empty() || *component == "." || *component == "..")
    {
        return Err("空要素、.、.. は使用できません");
    }
    Ok(components)
}

fn path_has_component_prefix(definition: &str, candidate: &str, platform: Platform) -> bool {
    let Ok(definition) = relative_components(definition) else {
        return false;
    };
    let Ok(candidate) = relative_components(candidate) else {
        return false;
    };
    definition.len() <= candidate.len()
        && definition
            .iter()
            .zip(candidate.iter())
            .all(|(expected, actual)| match platform {
                Platform::Windows => expected.eq_ignore_ascii_case(actual),
                Platform::Macos => expected == actual,
            })
}

static BUNDLED_CATALOG: OnceLock<Result<CacheCatalog, String>> = OnceLock::new();

pub fn bundled_catalog() -> Result<&'static CacheCatalog, String> {
    match BUNDLED_CATALOG.get_or_init(|| CacheCatalog::parse(BUNDLED_CATALOG_JSON)) {
        Ok(catalog) => Ok(catalog),
        Err(error) => Err(error.clone()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_catalog_is_valid_and_versioned() {
        let catalog = bundled_catalog().unwrap();
        assert_eq!(catalog.catalog_version, "2026.08.1");
        assert!(catalog.definitions.len() >= 6);
    }

    #[test]
    fn matches_only_complete_path_components() {
        let catalog = bundled_catalog().unwrap();
        let matches = catalog.classify(
            Platform::Windows,
            CachePathRoot::LocalAppData,
            "Google/Chrome/User Data/Default/Cache/data_1",
        );
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].id, "chrome.windows.default-cache");
        assert!(catalog
            .classify(
                Platform::Windows,
                CachePathRoot::LocalAppData,
                "Google/Chrome/User Data/Default/CacheBackup/data_1",
            )
            .is_empty());
    }

    #[test]
    fn windows_matching_is_case_insensitive() {
        let catalog = bundled_catalog().unwrap();
        assert_eq!(
            catalog
                .classify(
                    Platform::Windows,
                    CachePathRoot::LocalAppData,
                    "microsoft/edge/user data/default/cache/data_0",
                )
                .len(),
            1
        );
    }

    #[test]
    fn rejects_parent_path_segments() {
        let mut catalog = bundled_catalog().unwrap().clone();
        catalog.definitions[0].path.relative_path = "Library/../Secrets".to_owned();
        assert!(catalog.validate().is_err());
    }
}
