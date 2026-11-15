//! Tab views and modal dialogs.

use super::App;
use egui::Ui;

pub(crate) mod builder;
pub(crate) mod config_ui;
pub(crate) mod configuration;
pub(crate) mod console;
pub(crate) mod dialogs;
pub(crate) mod discovery;
pub(crate) mod messages;
pub(crate) mod server;
pub(crate) mod services;

impl App {
    pub(super) fn ui_services(&mut self, ui: &mut Ui) {
        services::ui(self, ui);
    }

    pub(super) fn ui_discovery(&mut self, ui: &mut Ui) {
        discovery::ui(self, ui);
    }

    pub(super) fn ui_messages(&mut self, ui: &mut Ui) {
        messages::ui(self, ui);
    }

    pub(super) fn ui_configuration(&mut self, ui: &mut Ui) {
        configuration::ui(self, ui);
    }

    pub(super) fn ui_server(&mut self, ui: &mut Ui) {
        server::ui(self, ui);
    }

    pub(super) fn ui_console(&mut self, ui: &mut Ui) {
        console::ui(self, ui);
    }
}
