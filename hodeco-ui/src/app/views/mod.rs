//! Tab views and modal dialogs.

use super::App;
use egui::Ui;

pub(crate) mod builder;
pub(crate) mod commands;
pub(crate) mod config_ui;
pub(crate) mod configuration;
pub(crate) mod console;
pub(crate) mod dialogs;
pub(crate) mod discovery;
pub(crate) mod messages;
pub(crate) mod runs;
pub(crate) mod schedules;
pub(crate) mod server;
pub(crate) mod services;
pub(crate) mod task_history;
pub(crate) mod tasks;

impl App {
    pub(super) fn ui_services_area(&mut self, ui: &mut Ui) {
        ui.horizontal(|ui| {
            for tab in super::ServicesTab::ALL {
                if ui
                    .add(egui::Button::selectable(
                        self.services_tab == tab,
                        tab.label(),
                    ))
                    .clicked()
                {
                    self.services_tab = tab;
                }
            }
        });
        ui.separator();
        match self.services_tab {
            super::ServicesTab::Services => services::ui(self, ui),
            super::ServicesTab::Discovery => discovery::ui(self, ui),
        }
    }

    pub(super) fn ui_tasks_area(&mut self, ui: &mut Ui) {
        ui.horizontal(|ui| {
            for tab in super::TasksTab::ALL {
                if ui
                    .add(egui::Button::selectable(self.tasks_tab == tab, tab.label()))
                    .clicked()
                {
                    self.request_tasks_tab(tab);
                }
            }
        });
        ui.separator();
        match self.tasks_tab {
            super::TasksTab::Tasks => tasks::ui(self, ui),
            super::TasksTab::Schedules => schedules::ui(self, ui),
            super::TasksTab::Commands => commands::ui(self, ui),
            super::TasksTab::Runs => runs::ui(self, ui),
        }
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
