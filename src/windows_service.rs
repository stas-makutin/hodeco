use super::ServiceManager as ServiceManagerTrait;
use std::env;
use std::ffi::c_void;
use std::ptr;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicPtr, Ordering};
use std::time::{Duration, Instant};
use std::{
    ffi::{OsStr, OsString},
    os::windows::ffi::OsStrExt,
};
use tokio_util::sync::CancellationToken;
use windows::{
    Win32::Foundation::{
        ERROR_FAILED_SERVICE_CONTROLLER_CONNECT, ERROR_FILE_NOT_FOUND,
        ERROR_SERVICE_ALREADY_RUNNING, ERROR_SERVICE_DOES_NOT_EXIST, ERROR_SERVICE_EXISTS,
        ERROR_SERVICE_NOT_ACTIVE, ERROR_SERVICE_SPECIFIC_ERROR, HANDLE, NO_ERROR, WIN32_ERROR,
    },
    Win32::System::EventLog::{
        DeregisterEventSource, EVENTLOG_ERROR_TYPE, EVENTLOG_INFORMATION_TYPE,
        EVENTLOG_WARNING_TYPE, RegisterEventSourceW, ReportEventW,
    },
    Win32::System::Registry::{
        HKEY, HKEY_LOCAL_MACHINE, KEY_WRITE, REG_DWORD, REG_OPTION_NON_VOLATILE, REG_SZ,
        RegCloseKey, RegCreateKeyExW, RegDeleteKeyW, RegSetValueExW,
    },
    Win32::System::Services::{
        ChangeServiceConfig2W, CloseServiceHandle, ControlService, CreateServiceW, DeleteService,
        OpenSCManagerW, OpenServiceW, QueryServiceStatusEx, RegisterServiceCtrlHandlerExW,
        SC_HANDLE, SC_MANAGER_CONNECT, SC_MANAGER_CREATE_SERVICE, SC_STATUS_PROCESS_INFO,
        SERVICE_ACCEPT_SHUTDOWN, SERVICE_ACCEPT_STOP, SERVICE_ALL_ACCESS, SERVICE_AUTO_START,
        SERVICE_CONFIG_DESCRIPTION, SERVICE_CONTINUE_PENDING, SERVICE_CONTROL_SHUTDOWN,
        SERVICE_CONTROL_STOP, SERVICE_DESCRIPTIONW, SERVICE_ERROR_NORMAL, SERVICE_PAUSE_PENDING,
        SERVICE_PAUSED, SERVICE_QUERY_STATUS, SERVICE_RUNNING, SERVICE_START,
        SERVICE_START_PENDING, SERVICE_STATUS, SERVICE_STATUS_CURRENT_STATE, SERVICE_STATUS_HANDLE,
        SERVICE_STATUS_PROCESS, SERVICE_STOP, SERVICE_STOP_PENDING, SERVICE_STOPPED,
        SERVICE_TABLE_ENTRYW, SERVICE_WIN32_OWN_PROCESS, SetServiceStatus,
        StartServiceCtrlDispatcherW, StartServiceW,
    },
    core::{PCWSTR, PWSTR, w},
};

use crate::command_line::ServiceOptions;
use crate::logging::ServiceLogLevel;
use crate::service_main;

const SERVICE_NAME: PCWSTR = w!("Hodeco Service");
const DISPLAY_NAME: &str = "Hodeco Service";
const DESCRIPTION: &str = "Hodeco - Home devices HTTP gateway service";

static SERVICE_HANDLE: AtomicPtr<c_void> = AtomicPtr::new(ptr::null_mut());
static EVENT_LOG_HANDLE: AtomicPtr<c_void> = AtomicPtr::new(ptr::null_mut());
static STOP_TOKEN: OnceLock<CancellationToken> = OnceLock::new();

pub fn run_as_service() -> bool {
    let dispatch_table = [
        SERVICE_TABLE_ENTRYW {
            // 2. Pass the compile-time PCWSTR directly
            lpServiceName: PWSTR(SERVICE_NAME.as_ptr() as *mut u16),
            lpServiceProc: Some(win32_service_main),
        },
        SERVICE_TABLE_ENTRYW {
            lpServiceName: PWSTR(std::ptr::null_mut()),
            lpServiceProc: None,
        },
    ];
    unsafe {
        match StartServiceCtrlDispatcherW(dispatch_table.as_ptr()) {
            Ok(()) => true,
            Err(err)
                if err.code()
                    == WIN32_ERROR(ERROR_FAILED_SERVICE_CONTROLLER_CONNECT.0).to_hresult() =>
            {
                false
            }
            Err(_) => true,
        }
    }
}

unsafe extern "system" fn service_ctrl_handler(
    dw_control: u32,
    _: u32,
    _: *mut std::ffi::c_void,
    _: *mut std::ffi::c_void,
) -> u32 {
    if dw_control == SERVICE_CONTROL_SHUTDOWN || dw_control == SERVICE_CONTROL_STOP {
        if let Some(token) = STOP_TOKEN.get() {
            report_status(SERVICE_STOP_PENDING);
            token.cancel(); // stop service signal
        }
        NO_ERROR.0
    } else {
        NO_ERROR.0
    }
}

extern "system" fn win32_service_main(_: u32, _: *mut PWSTR) {
    let handle = unsafe {
        match RegisterServiceCtrlHandlerExW(SERVICE_NAME, Some(service_ctrl_handler), None) {
            Ok(handle) => handle,
            Err(_) => return,
        }
    };
    SERVICE_HANDLE.store(handle.0, Ordering::Release);

    if let Ok(event_log) = unsafe { RegisterEventSourceW(None, SERVICE_NAME) } {
        EVENT_LOG_HANDLE.store(event_log.0, Ordering::Release);
    }

    report_status(SERVICE_START_PENDING);
    report_status(SERVICE_RUNNING);
    let success = service_main::service_main(
        service_log,
        STOP_TOKEN.get_or_init(CancellationToken::new).clone(),
    );

    let event_log = EVENT_LOG_HANDLE.swap(ptr::null_mut(), Ordering::AcqRel);
    if !event_log.is_null() {
        let _ = unsafe { DeregisterEventSource(HANDLE(event_log)) };
    }

    report_stopped(success);
}

/// Writes a log entry to the Windows Event Log for the service's event source.
fn service_log(level: ServiceLogLevel, message: &str) {
    let event_type = match level {
        ServiceLogLevel::Debug => return,
        ServiceLogLevel::Error => EVENTLOG_ERROR_TYPE,
        ServiceLogLevel::Warn => EVENTLOG_WARNING_TYPE,
        ServiceLogLevel::Info => EVENTLOG_INFORMATION_TYPE,
    };

    let handle = EVENT_LOG_HANDLE.load(Ordering::Acquire);
    if handle.is_null() {
        eprintln!("{}", message);
        return;
    }

    let message_wide = to_wide(message);
    let strings = [PCWSTR(message_wide.as_ptr())];

    unsafe {
        // Event ID 1 maps to the message-table entry in messages.mc.
        let _ = ReportEventW(
            HANDLE(handle),
            event_type,
            0,
            1,
            None,
            0,
            Some(&strings),
            None,
        );
    }
}

fn report_status(current_state: SERVICE_STATUS_CURRENT_STATE) {
    let handle = SERVICE_HANDLE.load(Ordering::Acquire);
    if handle.is_null() {
        return;
    }

    let status = SERVICE_STATUS {
        dwServiceType: SERVICE_WIN32_OWN_PROCESS,
        dwCurrentState: current_state,
        dwControlsAccepted: SERVICE_ACCEPT_STOP | SERVICE_ACCEPT_SHUTDOWN,
        dwWin32ExitCode: NO_ERROR.0,
        dwServiceSpecificExitCode: 0,
        dwCheckPoint: 0,
        dwWaitHint: 0,
    };
    let _ = unsafe { SetServiceStatus(SERVICE_STATUS_HANDLE(handle), &status) };
}

fn report_stopped(success: bool) {
    let handle = SERVICE_HANDLE.load(Ordering::Acquire);
    if handle.is_null() {
        return;
    }

    let status = SERVICE_STATUS {
        dwServiceType: SERVICE_WIN32_OWN_PROCESS,
        dwCurrentState: SERVICE_STOPPED,
        dwControlsAccepted: SERVICE_ACCEPT_STOP | SERVICE_ACCEPT_SHUTDOWN,
        dwWin32ExitCode: if success {
            NO_ERROR.0
        } else {
            ERROR_SERVICE_SPECIFIC_ERROR.0
        },
        dwServiceSpecificExitCode: u32::from(!success),
        dwCheckPoint: 0,
        dwWaitHint: 0,
    };
    let _ = unsafe { SetServiceStatus(SERVICE_STATUS_HANDLE(handle), &status) };
}

struct WindowsServiceManager {
    logger: fn(&str),
}

pub fn get_service_manager(logger: fn(&str)) -> impl ServiceManagerTrait {
    WindowsServiceManager { logger }
}

const SERVICE_NAME_STR: &str = "Hodeco Service";
const EVENTLOG_APPLICATION_KEY: &str = r"SYSTEM\CurrentControlSet\Services\EventLog\Application";
const EVENTLOG_TYPES_SUPPORTED: u32 = EVENTLOG_ERROR_TYPE.0 as u32
    | EVENTLOG_WARNING_TYPE.0 as u32
    | EVENTLOG_INFORMATION_TYPE.0 as u32;
const SERVICE_WAIT_TIMEOUT: Duration = Duration::from_secs(30);
const SERVICE_POLL_INTERVAL: Duration = Duration::from_millis(250);

impl ServiceManagerTrait for WindowsServiceManager {
    fn install(&self, options: &ServiceOptions) -> bool {
        let executable = match env::current_exe() {
            Ok(path) => path,
            Err(e) => {
                (self.logger)(&format!("Failed to get executable path: {}", e));
                return false;
            }
        };

        let config_path = match options.config_file.as_deref() {
            Some(path) => match crate::config::absolute_path(std::path::Path::new(path)) {
                Ok(path) => Some(path),
                Err(e) => {
                    (self.logger)(&format!(
                        "Failed to resolve configuration path '{}': {}",
                        path, e
                    ));
                    return false;
                }
            },
            None => None,
        };

        // Quote the executable and config path so SCM preserves spaces correctly.
        let mut command_line = OsString::from("\"");
        command_line.push(&executable);
        command_line.push("\"");
        if let Some(config) = &config_path {
            command_line.push(format!(" --config \"{}\"", config.display()));
        }

        let manager = match unsafe { OpenSCManagerW(None, None, SC_MANAGER_CREATE_SERVICE) } {
            Ok(handle) => handle,
            Err(e) => {
                (self.logger)(&format!("Failed to open service control manager: {}", e));
                return false;
            }
        };

        let name = to_wide(SERVICE_NAME_STR);
        let display_name = to_wide(DISPLAY_NAME);
        let binary_path = to_wide(&command_line);

        let service = match unsafe {
            CreateServiceW(
                manager,
                PCWSTR(name.as_ptr()),
                PCWSTR(display_name.as_ptr()),
                SERVICE_ALL_ACCESS,
                SERVICE_WIN32_OWN_PROCESS,
                SERVICE_AUTO_START,
                SERVICE_ERROR_NORMAL,
                PCWSTR(binary_path.as_ptr()),
                None,
                None,
                None,
                None,
                None,
            )
        } {
            Ok(handle) => handle,
            Err(e) => {
                let message = if e.code() == ERROR_SERVICE_EXISTS.to_hresult() {
                    format!("Service '{}' is already installed.", DISPLAY_NAME)
                } else {
                    format!("Failed to create service '{}': {}", DISPLAY_NAME, e)
                };
                (self.logger)(&message);
                let _ = unsafe { CloseServiceHandle(manager) };
                return false;
            }
        };

        let mut description_wide = to_wide(DESCRIPTION);
        let description = SERVICE_DESCRIPTIONW {
            lpDescription: PWSTR(description_wide.as_mut_ptr()),
        };
        if let Err(e) = unsafe {
            ChangeServiceConfig2W(
                service,
                SERVICE_CONFIG_DESCRIPTION,
                Some(&description as *const _ as *const std::ffi::c_void),
            )
        } {
            (self.logger)(&format!(
                "Warning: failed to set description for service '{}': {}",
                DISPLAY_NAME, e
            ));
        }

        if let Err(e) = register_event_source() {
            (self.logger)(&format!(
                "Failed to register Event Log source for service '{}': {}",
                DISPLAY_NAME, e
            ));
            // Roll back the service so install is all-or-nothing.
            let _ = unsafe { DeleteService(service) };
            let _ = unsafe { CloseServiceHandle(service) };
            let _ = unsafe { CloseServiceHandle(manager) };
            return false;
        }

        let _ = unsafe { CloseServiceHandle(service) };
        let _ = unsafe { CloseServiceHandle(manager) };
        (self.logger)(&format!(
            "Service '{}' installed successfully.",
            DISPLAY_NAME
        ));
        true
    }

    fn uninstall(&self) -> bool {
        let service_deleted = self.with_service(SERVICE_ALL_ACCESS, |service| {
            let stopped = match query_service_status(service) {
                Ok(status) if status.dwCurrentState == SERVICE_STOPPED => true,
                Ok(_) => {
                    if !self.stop_and_wait(service) {
                        (self.logger)(&format!(
                            "Failed to stop service '{}' before uninstalling.",
                            DISPLAY_NAME
                        ));
                        return false;
                    }
                    true
                }
                Err(e) => {
                    (self.logger)(&format!(
                        "Failed to query service '{}' status: {}",
                        DISPLAY_NAME, e
                    ));
                    false
                }
            };

            if !stopped {
                return false;
            }

            match unsafe { DeleteService(service) } {
                Ok(()) => true,
                Err(e) => {
                    (self.logger)(&format!(
                        "Failed to delete service '{}': {}",
                        DISPLAY_NAME, e
                    ));
                    false
                }
            }
        });

        let event_source_removed = match deregister_event_source() {
            Ok(()) => true,
            Err(e) => {
                (self.logger)(&format!(
                    "Warning: failed to remove Event Log source for service '{}': {}",
                    DISPLAY_NAME, e
                ));
                false
            }
        };

        if service_deleted && event_source_removed {
            (self.logger)(&format!(
                "Service '{}' uninstalled successfully.",
                DISPLAY_NAME
            ));
            true
        } else {
            false
        }
    }

    fn start(&self) -> bool {
        self.with_service(SERVICE_START | SERVICE_QUERY_STATUS, |service| {
            let status = match query_service_status(service) {
                Ok(status) => status,
                Err(e) => {
                    (self.logger)(&format!(
                        "Failed to query service '{}' status: {}",
                        DISPLAY_NAME, e
                    ));
                    return false;
                }
            };

            if status.dwCurrentState == SERVICE_RUNNING {
                (self.logger)(&format!("Service '{}' is already running.", DISPLAY_NAME));
                return false;
            }

            match unsafe { StartServiceW(service, None) } {
                Ok(()) => {
                    if self.wait_for_state(service, SERVICE_RUNNING) {
                        (self.logger)(&format!("Service '{}' started successfully.", DISPLAY_NAME));
                        true
                    } else {
                        (self.logger)(&format!(
                            "Timed out waiting for service '{}' to start.",
                            DISPLAY_NAME
                        ));
                        false
                    }
                }
                Err(e) if e.code() == ERROR_SERVICE_ALREADY_RUNNING.to_hresult() => {
                    (self.logger)(&format!("Service '{}' is already running.", DISPLAY_NAME));
                    false
                }
                Err(e) => {
                    (self.logger)(&format!(
                        "Failed to start service '{}': {}",
                        DISPLAY_NAME, e
                    ));
                    false
                }
            }
        })
    }

    fn stop(&self) -> bool {
        self.with_service(SERVICE_STOP | SERVICE_QUERY_STATUS, |service| {
            let status = match query_service_status(service) {
                Ok(status) => status,
                Err(e) => {
                    (self.logger)(&format!(
                        "Failed to query service '{}' status: {}",
                        DISPLAY_NAME, e
                    ));
                    return false;
                }
            };

            if status.dwCurrentState == SERVICE_STOPPED {
                (self.logger)(&format!("Service '{}' is already stopped.", DISPLAY_NAME));
                return false;
            }

            self.stop_and_wait(service)
        })
    }

    fn status(&self) -> bool {
        self.with_service(SERVICE_QUERY_STATUS, |service| {
            match query_service_status(service) {
                Ok(status) => {
                    (self.logger)(&service_state_message(status.dwCurrentState));
                    true
                }
                Err(e) => {
                    (self.logger)(&format!(
                        "Failed to query service '{}' status: {}",
                        DISPLAY_NAME, e
                    ));
                    false
                }
            }
        })
    }
}

impl WindowsServiceManager {
    /// Opens the SCM and the service, runs `action`, then closes both handles.
    fn with_service<F>(&self, access: u32, action: F) -> bool
    where
        F: FnOnce(SC_HANDLE) -> bool,
    {
        let manager = match unsafe { OpenSCManagerW(None, None, SC_MANAGER_CONNECT) } {
            Ok(handle) => handle,
            Err(e) => {
                (self.logger)(&format!("Failed to open service control manager: {}", e));
                return false;
            }
        };

        let name = to_wide(SERVICE_NAME_STR);
        let service = match unsafe { OpenServiceW(manager, PCWSTR(name.as_ptr()), access) } {
            Ok(handle) => handle,
            Err(e) => {
                let message = if e.code() == ERROR_SERVICE_DOES_NOT_EXIST.to_hresult() {
                    format!("Service '{}' is not installed.", DISPLAY_NAME)
                } else {
                    format!("Failed to open service '{}': {}", DISPLAY_NAME, e)
                };
                (self.logger)(&message);
                let _ = unsafe { CloseServiceHandle(manager) };
                return false;
            }
        };

        let result = action(service);
        let _ = unsafe { CloseServiceHandle(service) };
        let _ = unsafe { CloseServiceHandle(manager) };
        result
    }

    fn stop_and_wait(&self, service: SC_HANDLE) -> bool {
        let mut status = SERVICE_STATUS::default();
        match unsafe { ControlService(service, SERVICE_CONTROL_STOP, &mut status) } {
            Ok(()) => {}
            Err(e) if e.code() == ERROR_SERVICE_NOT_ACTIVE.to_hresult() => return true,
            Err(e) => {
                (self.logger)(&format!("Failed to stop service '{}': {}", DISPLAY_NAME, e));
                return false;
            }
        }

        if self.wait_for_state(service, SERVICE_STOPPED) {
            (self.logger)(&format!("Service '{}' stopped successfully.", DISPLAY_NAME));
            true
        } else {
            (self.logger)(&format!(
                "Timed out waiting for service '{}' to stop.",
                DISPLAY_NAME
            ));
            false
        }
    }

    fn wait_for_state(&self, service: SC_HANDLE, target: SERVICE_STATUS_CURRENT_STATE) -> bool {
        let deadline = Instant::now() + SERVICE_WAIT_TIMEOUT;
        loop {
            match query_service_status(service) {
                Ok(status) if status.dwCurrentState == target => return true,
                Ok(status) => {
                    let pending = status.dwCurrentState == SERVICE_START_PENDING
                        || status.dwCurrentState == SERVICE_STOP_PENDING
                        || status.dwCurrentState == SERVICE_CONTINUE_PENDING
                        || status.dwCurrentState == SERVICE_PAUSE_PENDING;

                    if !pending || Instant::now() >= deadline {
                        return false;
                    }
                }
                Err(e) => {
                    (self.logger)(&format!(
                        "Failed to query service '{}' status: {}",
                        DISPLAY_NAME, e
                    ));
                    return false;
                }
            }

            std::thread::sleep(SERVICE_POLL_INTERVAL);
        }
    }
}

fn query_service_status(service: SC_HANDLE) -> windows::core::Result<SERVICE_STATUS_PROCESS> {
    let mut status = SERVICE_STATUS_PROCESS::default();
    let mut bytes_needed = 0u32;
    let size = std::mem::size_of::<SERVICE_STATUS_PROCESS>();
    let buffer = unsafe {
        std::slice::from_raw_parts_mut(
            (&mut status as *mut SERVICE_STATUS_PROCESS).cast::<u8>(),
            size,
        )
    };

    unsafe {
        QueryServiceStatusEx(
            service,
            SC_STATUS_PROCESS_INFO,
            Some(buffer),
            &mut bytes_needed,
        )
    }?;

    Ok(status)
}

fn service_state_message(state: SERVICE_STATUS_CURRENT_STATE) -> String {
    let text = match state {
        SERVICE_STOPPED => "is stopped",
        SERVICE_START_PENDING => "is starting...",
        SERVICE_STOP_PENDING => "is stopping...",
        SERVICE_RUNNING => "is running",
        SERVICE_CONTINUE_PENDING => "is resuming...",
        SERVICE_PAUSE_PENDING => "is pausing...",
        SERVICE_PAUSED => "is paused",
        _ => {
            return format!(
                "Service '{}' is in an unknown state ({}).",
                DISPLAY_NAME, state.0
            );
        }
    };

    format!("Service '{}' {}.", DISPLAY_NAME, text)
}

/// Converts an OS string to a null-terminated UTF-16 buffer.
fn to_wide(value: impl AsRef<OsStr>) -> Vec<u16> {
    value.as_ref().encode_wide().chain(Some(0)).collect()
}

/// Creates the Event Log source registry key and points it at this executable's message table.
fn register_event_source() -> Result<(), String> {
    let executable =
        env::current_exe().map_err(|e| format!("Failed to get executable path: {}", e))?;
    let key_path = format!(r"{}\{}", EVENTLOG_APPLICATION_KEY, SERVICE_NAME_STR);
    let key_path_wide = to_wide(&key_path);
    let mut hkey = HKEY::default();

    let code = unsafe {
        RegCreateKeyExW(
            HKEY_LOCAL_MACHINE,
            PCWSTR(key_path_wide.as_ptr()),
            0,
            None,
            REG_OPTION_NON_VOLATILE,
            KEY_WRITE,
            None,
            &mut hkey,
            None,
        )
    };
    ensure_win32_success(code, "RegCreateKeyExW")?;

    let result = (|| -> Result<(), String> {
        let message_file_wide = to_wide(&executable);
        let message_file_bytes = unsafe {
            std::slice::from_raw_parts(
                message_file_wide.as_ptr().cast::<u8>(),
                message_file_wide.len() * std::mem::size_of::<u16>(),
            )
        };
        let value_name = to_wide("EventMessageFile");
        let code = unsafe {
            RegSetValueExW(
                hkey,
                PCWSTR(value_name.as_ptr()),
                0,
                REG_SZ,
                Some(message_file_bytes),
            )
        };
        ensure_win32_success(code, "RegSetValueExW(EventMessageFile)")?;

        let value_name = to_wide("TypesSupported");
        let types_supported = EVENTLOG_TYPES_SUPPORTED.to_le_bytes();
        let code = unsafe {
            RegSetValueExW(
                hkey,
                PCWSTR(value_name.as_ptr()),
                0,
                REG_DWORD,
                Some(&types_supported),
            )
        };
        ensure_win32_success(code, "RegSetValueExW(TypesSupported)")?;

        Ok(())
    })();

    let _ = unsafe { RegCloseKey(hkey) };
    result
}

/// Removes the Event Log source registry key created by [`register_event_source`].
fn deregister_event_source() -> Result<(), String> {
    let key_path = format!(r"{}\{}", EVENTLOG_APPLICATION_KEY, SERVICE_NAME_STR);
    let key_path_wide = to_wide(&key_path);
    let code = unsafe { RegDeleteKeyW(HKEY_LOCAL_MACHINE, PCWSTR(key_path_wide.as_ptr())) };

    // A missing key means there is nothing left to clean up.
    if code == ERROR_FILE_NOT_FOUND {
        return Ok(());
    }

    ensure_win32_success(code, "RegDeleteKeyW")
}

fn ensure_win32_success(code: WIN32_ERROR, operation: &str) -> Result<(), String> {
    if code == NO_ERROR {
        Ok(())
    } else {
        Err(format!("{} failed with Win32 error {}", operation, code.0))
    }
}
