use crate::command_line::ServiceOptions;

#[cfg(unix)]
#[path = "linux_service.rs"]
pub mod service_factory;
#[cfg(windows)]
#[path = "windows_service.rs"]
pub mod service_factory;

pub trait ServiceManager {
    fn install(&self, options: &ServiceOptions) -> bool;
    fn uninstall(&self) -> bool;
    fn start(&self) -> bool;
    fn stop(&self) -> bool;
    fn status(&self) -> bool;
}
