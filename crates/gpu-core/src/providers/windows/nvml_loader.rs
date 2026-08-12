use crate::model::UnavailableReason;
use nvml_wrapper::Nvml;
use nvml_wrapper::error::NvmlError;
use std::ffi::OsString;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use windows::Win32::Foundation::{FreeLibrary, HMODULE};
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::System::LibraryLoader::{
    GetModuleFileNameW, LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR, LOAD_LIBRARY_SEARCH_SYSTEM32,
    LoadLibraryExW,
};
use windows::Win32::System::SystemInformation::GetSystemDirectoryW;
use windows::Win32::UI::Shell::{FOLDERID_ProgramFiles, KNOWN_FOLDER_FLAG, SHGetKnownFolderPath};
use windows::core::PCWSTR;

const NVML_DLL_NAME: &str = "nvml.dll";
const MAX_WINDOWS_PATH_UNITS: usize = 32_768;

#[derive(Debug)]
pub(crate) struct NvmlLoadFailure {
    pub(crate) reason: UnavailableReason,
    pub(crate) message: String,
}

struct PreloadedModule(HMODULE);

impl Drop for PreloadedModule {
    fn drop(&mut self) {
        if !self.0.is_invalid() {
            // SAFETY: this guard owns one successful LoadLibraryExW reference.
            unsafe {
                let _ = FreeLibrary(self.0);
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TrustedLocation {
    SystemDirectory,
    LegacyNvsmi,
}

impl TrustedLocation {
    const fn description(self) -> &'static str {
        match self {
            Self::SystemDirectory => "the Windows system directory",
            Self::LegacyNvsmi => "the NVIDIA NVSMI directory under Program Files",
        }
    }
}

/// Initializes NVML from an absolute OS-derived path. This function never
/// passes a bare DLL name to `nvml-wrapper`, so the current directory and PATH
/// cannot participate in library resolution. A secure preload also constrains
/// dependent-DLL resolution without changing process-global search state.
pub(crate) fn initialize() -> Result<(Nvml, String), NvmlLoadFailure> {
    let mut discovery_errors = Vec::new();
    let system_directory = match windows_system_directory() {
        Ok(path) => Some(path),
        Err(message) => {
            discovery_errors.push(message);
            None
        }
    };
    let program_files = match program_files_directory() {
        Ok(path) => Some(path),
        Err(message) => {
            discovery_errors.push(message);
            None
        }
    };

    let candidates = trusted_candidates(system_directory.as_deref(), program_files.as_deref());
    let mut load_failures = Vec::new();
    let mut load_reason = None;
    for (path, location) in candidates {
        let preload = match securely_preload(&path) {
            Ok(preload) => preload,
            Err(error) => {
                let reason = classify_windows_load_error(&error);
                if load_reason != Some(UnavailableReason::PermissionDenied)
                    || reason == UnavailableReason::PermissionDenied
                {
                    load_reason = Some(reason);
                }
                load_failures.push(format!(
                    "secure NVML load from {} failed: {error}",
                    location.description()
                ));
                continue;
            }
        };

        // The preload resolves NVML and its dependencies with per-call search
        // flags. nvml-wrapper then acquires its own reference to this validated
        // absolute module for the lifetime of Nvml.
        let initialized = Nvml::builder().lib_path(path.as_os_str()).init();
        drop(preload);
        match initialized {
            Ok(nvml) => {
                return Ok((
                    nvml,
                    format!("NVML loaded securely from {}", location.description()),
                ));
            }
            Err(error) => {
                let reason = classify_nvml_error(&error);
                if load_reason != Some(UnavailableReason::PermissionDenied)
                    || reason == UnavailableReason::PermissionDenied
                {
                    load_reason = Some(reason);
                }
                load_failures.push(format!(
                    "NVML initialization from {} failed: {error}",
                    location.description()
                ));
            }
        }
    }

    Err(load_failure(discovery_errors, load_failures, load_reason))
}

fn load_failure(
    mut discovery_errors: Vec<String>,
    load_failures: Vec<String>,
    load_reason: Option<UnavailableReason>,
) -> NvmlLoadFailure {
    discovery_errors.extend(load_failures);
    let details = discovery_errors;
    let message = if details.is_empty() {
        "NVML was not found in the Windows system directory or the NVIDIA NVSMI directory under Program Files"
            .to_owned()
    } else {
        format!(
            "could not initialize NVML from a trusted Windows location: {}",
            details.join("; ")
        )
    };
    NvmlLoadFailure {
        reason: load_reason.unwrap_or(UnavailableReason::DriverLibraryMissing),
        message,
    }
}

fn securely_preload(path: &Path) -> std::result::Result<PreloadedModule, windows::core::Error> {
    debug_assert!(path.is_absolute());
    let mut wide: Vec<u16> = path.as_os_str().encode_wide().collect();
    if wide.contains(&0) {
        return Err(windows::core::Error::new(
            windows::core::HRESULT::from_win32(87),
            "NVML path contains an embedded nul",
        ));
    }
    wide.push(0);
    // SAFETY: the path is a live, nul-terminated absolute UTF-16 string. These
    // per-call flags restrict dependencies to NVML's directory and System32;
    // no process-global DLL search setting is modified.
    let module = unsafe {
        LoadLibraryExW(
            PCWSTR(wide.as_ptr()),
            None,
            LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | LOAD_LIBRARY_SEARCH_SYSTEM32,
        )
    }?;
    let loaded = PreloadedModule(module);
    let mapped_path = loaded_module_path(loaded.0)?;
    if !same_windows_path(path, &mapped_path) {
        return Err(windows::core::Error::new(
            windows::core::HRESULT::from_win32(123),
            format!(
                "Windows mapped NVML from an unexpected path: {}",
                mapped_path.display()
            ),
        ));
    }
    Ok(loaded)
}

fn loaded_module_path(module: HMODULE) -> std::result::Result<PathBuf, windows::core::Error> {
    let mut buffer = vec![0_u16; MAX_WINDOWS_PATH_UNITS];
    // SAFETY: module is a live LoadLibraryExW handle and the output slice is
    // valid for the duration of this read-only path query.
    let length = unsafe { GetModuleFileNameW(Some(module), &mut buffer) } as usize;
    if length == 0 {
        return Err(windows::core::Error::from_thread());
    }
    if length >= buffer.len() {
        return Err(windows::core::Error::new(
            windows::core::HRESULT::from_win32(122),
            "loaded NVML module path exceeded the bounded buffer",
        ));
    }
    Ok(PathBuf::from(OsString::from_wide(&buffer[..length])))
}

fn same_windows_path(expected: &Path, actual: &Path) -> bool {
    let normalize = |path: &Path| {
        path.to_string_lossy()
            .trim_start_matches(r"\\?\")
            .replace('/', r"\")
    };
    normalize(expected).eq_ignore_ascii_case(&normalize(actual))
}

fn classify_windows_load_error(error: &windows::core::Error) -> UnavailableReason {
    match error.code().0 as u32 & 0xffff {
        2 | 3 | 126 | 127 | 193 => UnavailableReason::DriverLibraryMissing,
        5 => UnavailableReason::PermissionDenied,
        _ => UnavailableReason::ProviderError,
    }
}

fn classify_nvml_error(error: &NvmlError) -> UnavailableReason {
    match error {
        NvmlError::NoPermission | NvmlError::OperatingSystem => UnavailableReason::PermissionDenied,
        NvmlError::DriverNotLoaded
        | NvmlError::LibraryNotFound
        | NvmlError::LibloadingError(_)
        | NvmlError::LibRmVersionMismatch => UnavailableReason::DriverLibraryMissing,
        NvmlError::Timeout | NvmlError::NoData | NvmlError::InUse => {
            UnavailableReason::TemporarilyUnavailable
        }
        _ => UnavailableReason::ProviderError,
    }
}

fn trusted_candidates(
    system_directory: Option<&Path>,
    program_files: Option<&Path>,
) -> Vec<(PathBuf, TrustedLocation)> {
    let mut result = Vec::with_capacity(2);
    if let Some(root) = system_directory.filter(|path| is_trusted_absolute_root(path)) {
        result.push((root.join(NVML_DLL_NAME), TrustedLocation::SystemDirectory));
    }
    if let Some(root) = program_files.filter(|path| is_trusted_absolute_root(path)) {
        result.push((
            root.join("NVIDIA Corporation")
                .join("NVSMI")
                .join(NVML_DLL_NAME),
            TrustedLocation::LegacyNvsmi,
        ));
    }
    result
}

fn is_trusted_absolute_root(path: &Path) -> bool {
    path.is_absolute()
        && path.components().all(|component| {
            !matches!(
                component,
                std::path::Component::CurDir | std::path::Component::ParentDir
            )
        })
}

fn windows_system_directory() -> Result<PathBuf, String> {
    let mut buffer = vec![0_u16; MAX_WINDOWS_PATH_UNITS];
    // SAFETY: the mutable slice is valid for the duration of the call and its
    // capacity is represented exactly by the generated binding.
    let length = unsafe { GetSystemDirectoryW(Some(&mut buffer)) } as usize;
    if length == 0 {
        return Err(format!(
            "GetSystemDirectoryW failed: {}",
            std::io::Error::last_os_error()
        ));
    }
    if length >= buffer.len() {
        return Err(format!(
            "GetSystemDirectoryW returned an invalid length ({length})"
        ));
    }
    let path = PathBuf::from(OsString::from_wide(&buffer[..length]));
    if !path.is_absolute() {
        return Err("GetSystemDirectoryW returned a non-absolute path".into());
    }
    Ok(path)
}

fn program_files_directory() -> Result<PathBuf, String> {
    // SAFETY: FOLDERID_ProgramFiles is a static Windows GUID. The returned
    // CoTaskMem allocation is converted before being released exactly once.
    let value = unsafe { SHGetKnownFolderPath(&FOLDERID_ProgramFiles, KNOWN_FOLDER_FLAG(0), None) }
        .map_err(|error| format!("SHGetKnownFolderPath(ProgramFiles) failed: {error}"))?;
    // SAFETY: SHGetKnownFolderPath returned a nul-terminated UTF-16 string.
    let converted = unsafe { value.to_string() };
    // SAFETY: SHGetKnownFolderPath documents CoTaskMemFree for this allocation,
    // and `value` is not used after this call.
    unsafe {
        CoTaskMemFree(Some(value.0.cast()));
    }
    let path = PathBuf::from(
        converted.map_err(|_| "Program Files known-folder path was invalid UTF-16".to_owned())?,
    );
    if !path.is_absolute() {
        return Err("Program Files known-folder API returned a non-absolute path".into());
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_os_derived_absolute_nvml_paths_are_candidates() {
        let candidates = trusted_candidates(
            Some(Path::new(r"C:\Windows\System32")),
            Some(Path::new(r"C:\Program Files")),
        );
        let paths: Vec<_> = candidates.into_iter().map(|(path, _)| path).collect();
        assert_eq!(
            paths,
            vec![
                PathBuf::from(r"C:\Windows\System32\nvml.dll"),
                PathBuf::from(r"C:\Program Files\NVIDIA Corporation\NVSMI\nvml.dll"),
            ]
        );
        assert!(!paths.contains(&PathBuf::from(r"C:\malicious-cwd\nvml.dll")));
        assert!(!paths.contains(&PathBuf::from(r"C:\malicious-path\nvml.dll")));
    }

    #[test]
    fn relative_roots_and_bare_names_are_rejected() {
        assert!(
            trusted_candidates(
                Some(Path::new(".")),
                Some(Path::new(r"untrusted\program-files")),
            )
            .is_empty()
        );
        assert!(
            trusted_candidates(Some(Path::new(r"C:\Windows\System32\..\Temp")), None).is_empty()
        );
    }

    #[test]
    fn missing_nvml_initialization_is_an_optional_driver_library_failure() {
        assert!(trusted_candidates(None, None).is_empty());
        let failure = load_failure(Vec::new(), Vec::new(), None);
        assert_eq!(failure.reason, UnavailableReason::DriverLibraryMissing);
        assert!(failure.message.contains("NVML was not found"));
        assert_eq!(
            classify_nvml_error(&NvmlError::LibraryNotFound),
            UnavailableReason::DriverLibraryMissing
        );
    }

    #[test]
    fn windows_loader_errors_are_classified_without_panicking() {
        assert_eq!(
            classify_windows_load_error(&windows::core::Error::new(
                windows::core::HRESULT::from_win32(126),
                "missing",
            )),
            UnavailableReason::DriverLibraryMissing
        );
        assert_eq!(
            classify_windows_load_error(&windows::core::Error::new(
                windows::core::HRESULT::from_win32(5),
                "denied",
            )),
            UnavailableReason::PermissionDenied
        );
    }

    #[test]
    fn mapped_module_path_must_match_the_trusted_candidate() {
        assert!(same_windows_path(
            Path::new(r"C:\Windows\System32\nvml.dll"),
            Path::new(r"\\?\c:\WINDOWS\system32\nvml.dll")
        ));
        assert!(!same_windows_path(
            Path::new(r"C:\Windows\System32\nvml.dll"),
            Path::new(r"C:\malicious-path\nvml.dll")
        ));
    }
}
