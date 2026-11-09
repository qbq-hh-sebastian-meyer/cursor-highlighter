use std::{os::windows::ffi::OsStrExt, path::Path, ptr::null_mut};
use windows_sys::{
    Win32::{Foundation::*, System::Registry::*},
    core::w,
};

const RUN_KEY: *const u16 = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run");
const VALUE_NAME: *const u16 = w!("CursorHighlighter");

struct Key(HKEY);

impl Drop for Key {
    fn drop(&mut self) {
        unsafe {
            RegCloseKey(self.0);
        }
    }
}

fn registry_error(operation: &str, status: u32) -> String {
    format!(
        "{operation}: {}",
        std::io::Error::from_raw_os_error(status as i32)
    )
}

fn query_result(status: u32) -> Result<bool, String> {
    match status {
        ERROR_SUCCESS => Ok(true),
        ERROR_FILE_NOT_FOUND => Ok(false),
        _ => Err(registry_error("Read launch-at-login entry", status)),
    }
}

fn update_result(enabled: bool, status: u32) -> Result<(), String> {
    if status == ERROR_SUCCESS || (!enabled && status == ERROR_FILE_NOT_FOUND) {
        Ok(())
    } else {
        Err(registry_error("Update launch-at-login entry", status))
    }
}

// Quote the entire executable so spaces never change which program Windows runs.
fn command(executable: &Path) -> Result<Vec<u16>, String> {
    let path: Vec<u16> = executable.as_os_str().encode_wide().collect();
    if !executable.is_absolute() || path.iter().any(|&unit| unit == 0 || unit == b'"' as u16) {
        return Err(
            "The startup executable must have an absolute path without quotes or NULs.".into(),
        );
    }
    if path.len() + 2 > 260 {
        return Err(
            "The quoted executable path exceeds the Windows Run limit of 260 characters.".into(),
        );
    }
    let mut value = Vec::with_capacity(path.len() + 3);
    value.push(b'"' as u16);
    value.extend(path);
    value.extend([b'"' as u16, 0]);
    Ok(value)
}

pub fn enabled() -> Result<bool, String> {
    let mut key = null_mut();
    let status = unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, RUN_KEY, 0, KEY_QUERY_VALUE, &mut key) };
    if status == ERROR_FILE_NOT_FOUND {
        return Ok(false);
    }
    if status != ERROR_SUCCESS {
        return Err(registry_error("Read launch-at-login settings", status));
    }
    let key = Key(key);
    let status = unsafe {
        RegQueryValueExW(
            key.0,
            VALUE_NAME,
            null_mut(),
            null_mut(),
            null_mut(),
            null_mut(),
        )
    };
    query_result(status)
}

pub fn set_enabled(enabled: bool) -> Result<(), String> {
    let value = if enabled {
        Some(command(&std::env::current_exe().map_err(|error| {
            format!("Find startup executable: {error}")
        })?)?)
    } else {
        None
    };
    let mut key = null_mut();
    let status = unsafe {
        if enabled {
            RegCreateKeyExW(
                HKEY_CURRENT_USER,
                RUN_KEY,
                0,
                std::ptr::null(),
                REG_OPTION_NON_VOLATILE,
                KEY_SET_VALUE,
                std::ptr::null(),
                &mut key,
                null_mut(),
            )
        } else {
            RegOpenKeyExW(HKEY_CURRENT_USER, RUN_KEY, 0, KEY_SET_VALUE, &mut key)
        }
    };
    if !enabled && status == ERROR_FILE_NOT_FOUND {
        return Ok(());
    }
    if status != ERROR_SUCCESS {
        return Err(registry_error("Open launch-at-login settings", status));
    }
    let key = Key(key);
    let status = unsafe {
        match value {
            Some(value) => RegSetValueExW(
                key.0,
                VALUE_NAME,
                0,
                REG_SZ,
                value.as_ptr().cast(),
                (value.len() * size_of::<u16>()) as u32,
            ),
            None => RegDeleteValueW(key.0, VALUE_NAME),
        }
    };
    update_result(enabled, status)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_registry_errors_and_allows_idempotent_removal_without_registry_access() {
        assert_eq!(query_result(ERROR_SUCCESS), Ok(true));
        assert_eq!(query_result(ERROR_FILE_NOT_FOUND), Ok(false));
        assert!(query_result(ERROR_ACCESS_DENIED).is_err());
        for enabled in [false, true] {
            assert_eq!(update_result(enabled, ERROR_SUCCESS), Ok(()));
            let error = update_result(enabled, ERROR_ACCESS_DENIED).unwrap_err();
            assert!(error.starts_with("Update launch-at-login entry: "));
        }
        assert_eq!(update_result(false, ERROR_FILE_NOT_FOUND), Ok(()));
        assert!(update_result(true, ERROR_FILE_NOT_FOUND).is_err());
    }

    #[test]
    fn quotes_full_executable_path_and_terminates_registry_string() {
        for path in [
            r"C:\Apps\cursor-highlighter.exe",
            r"C:\Program Files\Cursor Highlighter\cursor-highlighter.exe",
            "C:\\Users\\\u{00e9}\\cursor-highlighter.exe",
            r"\\server\share\cursor-highlighter.exe",
        ] {
            let value = command(Path::new(path)).unwrap();
            assert_eq!(value.last(), Some(&0));
            assert_eq!(
                String::from_utf16(&value[..value.len() - 1]).unwrap(),
                format!("\"{path}\"")
            );
        }
    }

    #[test]
    fn rejects_invalid_and_overlong_commands_without_registry_access() {
        for path in [
            "cursor-highlighter.exe",
            r#"C:\Apps\bad"name.exe"#,
            "C:\\Apps\\bad\0name.exe",
            &format!("C:\\{}.exe", "a".repeat(260)),
        ] {
            assert!(command(Path::new(path)).is_err());
        }
        let maximum = format!("C:\\{}.exe", "a".repeat(251));
        assert_eq!(command(Path::new(&maximum)).unwrap().len(), 261);
    }
}
