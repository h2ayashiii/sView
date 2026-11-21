#[cfg(target_os = "windows")]
pub(crate) mod windows;

#[cfg(target_os = "macos")]
pub(crate) mod macos;
