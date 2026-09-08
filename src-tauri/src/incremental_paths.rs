use std::path::{Component, Path, PathBuf};

/// 変更pathをscan root基準で解釈した結果。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ChangeScope {
    /// scan root配下の変更。保持するのはroot基準の相対pathで、root自身は `.`。
    Inside(PathBuf),
    /// scan rootの厳密な祖先に対する変更。単独では走査結果に影響しないが、
    /// 部分木全体の再走査要求を伴う場合はroot配下すべてが対象になる。
    Ancestor,
    /// scan rootと無関係な変更。走査対象ではないため無視する。
    Outside,
    /// 相対pathとして解釈できない変更。fail closedでフルスキャンへ戻す。
    Invalid,
}

/// 相対pathを検証しつつ正規化する。root自身は `.` として表す。
pub fn normalize_relative(path: &Path) -> Option<PathBuf> {
    if path.as_os_str().is_empty() || path.to_string_lossy().contains('\0') {
        return None;
    }
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::Normal(value) => normalized.push(value),
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    if normalized.as_os_str().is_empty() {
        Some(PathBuf::from("."))
    } else {
        Some(normalized)
    }
}

/// device-relative pathを、選択されたscan root基準の相対pathへ変換する。
///
/// `device_relative_root` はvolume root自身のとき `.`、それ以外はvolume root基準の
/// 相対pathを表す。scan rootの厳密な祖先に対する変更は、単独ではscan結果に現れないが
/// 部分木再走査要求と組み合わさるとroot配下全体を指すため `Ancestor` として区別する。
/// root自身の同一性はcheckpointのvolume／root identityで別途検証する。
pub fn to_scan_root_relative(device_relative_root: &Path, change: &Path) -> ChangeScope {
    let (Some(root), Some(change)) = (
        normalize_relative(device_relative_root),
        normalize_relative(change),
    ) else {
        return ChangeScope::Invalid;
    };
    if root == Path::new(".") {
        return ChangeScope::Inside(change);
    }
    if change == root {
        return ChangeScope::Inside(PathBuf::from("."));
    }
    if change == Path::new(".") || root.starts_with(&change) {
        return ChangeScope::Ancestor;
    }
    match change.strip_prefix(&root) {
        Ok(relative) => ChangeScope::Inside(relative.to_path_buf()),
        Err(_) => ChangeScope::Outside,
    }
}

#[cfg(target_os = "macos")]
pub use platform::device_relative_root;

#[cfg(target_os = "macos")]
mod platform {
    use super::*;
    use cap_std::ambient_authority;
    use cap_std::fs::{Dir, MetadataExt};
    use std::ffi::{CStr, CString, OsStr, OsString};
    use std::os::unix::ffi::OsStrExt;

    fn mount_point(root: &Path) -> Result<PathBuf, String> {
        let root_c = CString::new(root.as_os_str().as_bytes())
            .map_err(|_| "走査rootにNULが含まれています".to_owned())?;
        let mut info = std::mem::MaybeUninit::<libc::statfs>::uninit();
        if unsafe { libc::statfs(root_c.as_ptr(), info.as_mut_ptr()) } != 0 {
            return Err(format!(
                "mount pointを取得できません: {}",
                std::io::Error::last_os_error()
            ));
        }
        let info = unsafe { info.assume_init() };
        let bytes = unsafe { CStr::from_ptr(info.f_mntonname.as_ptr()) }.to_bytes();
        let mount = PathBuf::from(OsStr::from_bytes(bytes));
        if root.starts_with(&mount) {
            Ok(mount)
        } else {
            Ok(PathBuf::from("/"))
        }
    }

    /// directory entryのうち、要求名にbyte一致するものを返す。
    fn exact_name(parent: &Dir, requested: &OsStr) -> Result<Option<OsString>, String> {
        for entry in parent
            .entries()
            .map_err(|error| format!("走査rootの親directoryを読み取れません: {error}"))?
        {
            let entry =
                entry.map_err(|error| format!("走査rootの親directoryを読み取れません: {error}"))?;
            if entry.file_name() == requested {
                return Ok(Some(entry.file_name()));
            }
        }
        Ok(None)
    }

    /// byte一致しない場合に限り、inodeが一致するentryのon-disk表記を探す。
    fn name_by_identity(parent: &Dir, identity: (u64, u64)) -> Result<Option<OsString>, String> {
        for entry in parent
            .entries()
            .map_err(|error| format!("走査rootの親directoryを読み取れません: {error}"))?
        {
            let entry =
                entry.map_err(|error| format!("走査rootの親directoryを読み取れません: {error}"))?;
            let Ok(metadata) = entry.metadata() else {
                continue;
            };
            if (metadata.dev(), metadata.ino()) == identity {
                return Ok(Some(entry.file_name()));
            }
        }
        Ok(None)
    }

    fn open_child_directory(parent: &Dir, name: &OsStr) -> Result<(Dir, (u64, u64)), String> {
        let link = parent
            .symlink_metadata(name)
            .map_err(|error| format!("走査rootの構成要素を確認できません: {error}"))?;
        if !link.is_dir() {
            return Err("走査rootの構成要素がdirectoryではありません".to_owned());
        }
        let child = parent
            .open_dir(name)
            .map_err(|error| format!("走査rootの構成要素を開けません: {error}"))?;
        let opened = child
            .dir_metadata()
            .map_err(|error| format!("走査rootの構成要素のidentityを取得できません: {error}"))?;
        if (opened.dev(), opened.ino()) != (link.dev(), link.ino()) {
            return Err("走査rootの構成要素が開いた対象と一致しません".to_owned());
        }
        Ok((child, (opened.dev(), opened.ino())))
    }

    /// canonical rootを、FSEventsが返すのと同じon-disk表記のdevice-relative pathへ解決する。
    ///
    /// 大文字小文字やUnicode正規化を区別しないvolumeでは、canonicalizeがon-disk表記を
    /// 返さない。文字列prefixだけで一致を判断すると変更を取りこぼすため、各構成要素を
    /// inodeで照合したon-disk表記に置き換える。確認できない場合はfail closedで失敗させる。
    pub fn device_relative_root(root: &Path) -> Result<PathBuf, String> {
        if !root.is_absolute() {
            return Err("device-relative rootの解決には絶対pathが必要です".to_owned());
        }
        let mount = mount_point(root)?;
        let lexical = root.strip_prefix(&mount).map_err(|_| {
            format!(
                "走査rootをmount point {} から相対化できません",
                mount.display()
            )
        })?;
        let mut current = Dir::open_ambient_dir(&mount, ambient_authority())
            .map_err(|error| format!("mount pointを安全に開けません: {error}"))?;
        let mount_device = current
            .dir_metadata()
            .map_err(|error| format!("mount pointのidentityを取得できません: {error}"))?
            .dev();
        let mut resolved = PathBuf::new();
        for component in lexical.components() {
            let Component::Normal(name) = component else {
                return Err("走査rootに解決できない構成要素が含まれています".to_owned());
            };
            let (child, identity) = open_child_directory(&current, name)?;
            if identity.0 != mount_device {
                return Err("走査rootがmount pointと別volumeを跨いでいます".to_owned());
            }
            let on_disk = match exact_name(&current, name)? {
                Some(value) => value,
                None => name_by_identity(&current, identity)?
                    .ok_or_else(|| "走査rootの構成要素のon-disk表記を確認できません".to_owned())?,
            };
            resolved.push(on_disk);
            current = child;
        }
        let expected = Dir::open_ambient_dir(root, ambient_authority())
            .map_err(|error| format!("走査rootを安全に開けません: {error}"))?
            .dir_metadata()
            .map_err(|error| format!("走査rootのidentityを取得できません: {error}"))?;
        let reached = current
            .dir_metadata()
            .map_err(|error| format!("解決したrootのidentityを取得できません: {error}"))?;
        if (reached.dev(), reached.ino()) != (expected.dev(), expected.ino()) {
            return Err("解決したdevice-relative rootが走査rootと一致しません".to_owned());
        }
        if resolved.as_os_str().is_empty() {
            Ok(PathBuf::from("."))
        } else {
            Ok(resolved)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inside(path: &str) -> ChangeScope {
        ChangeScope::Inside(PathBuf::from(path))
    }

    #[test]
    fn normalizes_relative_paths_and_rejects_escapes() {
        assert_eq!(normalize_relative(Path::new("a/b")), Some("a/b".into()));
        assert_eq!(normalize_relative(Path::new("./a")), Some("a".into()));
        assert_eq!(normalize_relative(Path::new(".")), Some(".".into()));
        for rejected in ["", "../a", "/a", "a/../b", "a\0b"] {
            assert_eq!(normalize_relative(Path::new(rejected)), None, "{rejected}");
        }
    }

    #[test]
    fn converts_device_relative_changes_for_a_nested_scan_root() {
        let root = Path::new("work/project");
        assert_eq!(
            to_scan_root_relative(root, Path::new("work/project")),
            inside(".")
        );
        assert_eq!(
            to_scan_root_relative(root, Path::new("work/project/src/main.rs")),
            inside("src/main.rs")
        );
    }

    #[test]
    fn passes_changes_through_when_the_scan_root_is_the_volume_root() {
        let root = Path::new(".");
        assert_eq!(to_scan_root_relative(root, Path::new(".")), inside("."));
        assert_eq!(to_scan_root_relative(root, Path::new("a/b")), inside("a/b"));
    }

    #[test]
    fn treats_siblings_and_text_prefixes_as_outside() {
        let root = Path::new("work/project");
        for outside in [
            "work/other",
            "work/projector/file",
            "other/work/project/file",
        ] {
            assert_eq!(
                to_scan_root_relative(root, Path::new(outside)),
                ChangeScope::Outside,
                "{outside}"
            );
        }
    }

    /// 祖先への変更は`Outside`と混ぜない。部分木再走査要求が付くとroot配下全体を
    /// 指すため、呼び出し側が範囲を判断できるよう区別して返す。
    #[test]
    fn distinguishes_strict_ancestors_from_unrelated_changes() {
        let root = Path::new("work/project");
        for ancestor in [".", "work"] {
            assert_eq!(
                to_scan_root_relative(root, Path::new(ancestor)),
                ChangeScope::Ancestor,
                "{ancestor}"
            );
        }
        // volume rootがscan rootのときは、祖先が存在しない。
        assert_eq!(
            to_scan_root_relative(Path::new("."), Path::new(".")),
            inside(".")
        );
    }

    #[test]
    fn rejects_unsafe_change_paths_without_guessing_scope() {
        for unsafe_path in [
            "",
            "/work/project/file",
            "../file",
            "work/../../file",
            "a\0b",
        ] {
            assert_eq!(
                to_scan_root_relative(Path::new("work"), Path::new(unsafe_path)),
                ChangeScope::Invalid,
                "{unsafe_path}"
            );
        }
        assert_eq!(
            to_scan_root_relative(Path::new(""), Path::new("file")),
            ChangeScope::Invalid
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn resolves_on_disk_spelling_for_case_insensitive_roots() {
        use std::fs;
        let base = std::env::temp_dir().join(format!(
            "disk-visualizer-on-disk-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let nested = base.join("MixedCase");
        fs::create_dir_all(&nested).unwrap();
        let canonical = base.canonicalize().unwrap();
        let exact = device_relative_root(&canonical.join("MixedCase")).unwrap();
        let requested = canonical.join("mixedcase");
        if requested.is_dir() {
            assert_eq!(device_relative_root(&requested).unwrap(), exact);
        }
        assert!(exact.ends_with("MixedCase"));
        assert!(device_relative_root(Path::new("relative")).is_err());
        fs::remove_dir_all(base).unwrap();
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn represents_the_volume_root_as_a_single_dot() {
        assert_eq!(
            device_relative_root(Path::new("/")).unwrap(),
            PathBuf::from(".")
        );
    }
}
