use super::error::{ApiError, ErrorCode};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Method {
    #[serde(rename = "system.restart")]
    SystemRestart,
    #[serde(rename = "server.info")]
    ServerInfo,
    #[serde(rename = "config.get")]
    ConfigGet,
    #[serde(rename = "config.update")]
    ConfigUpdate,
    #[serde(rename = "protocols.list")]
    ProtocolsList,
    #[serde(rename = "transports.list")]
    TransportsList,
    #[serde(rename = "protocols.info")]
    ProtocolsInfo,
    #[serde(rename = "protocols.params")]
    ProtocolsParams,
    #[serde(rename = "discovery.start")]
    DiscoveryStart,
    #[serde(rename = "discovery.get")]
    DiscoveryGet,
    #[serde(rename = "discovery.cancel")]
    DiscoveryCancel,
    #[serde(rename = "services.list")]
    ServicesList,
    #[serde(rename = "services.add")]
    ServicesAdd,
    #[serde(rename = "services.remove")]
    ServicesRemove,
    #[serde(rename = "services.alias")]
    ServicesAlias,
    #[serde(rename = "services.status")]
    ServicesStatus,
    #[serde(rename = "services.params")]
    ServicesParams,
    #[serde(rename = "services.send")]
    ServicesSend,
    #[serde(rename = "messages.get")]
    MessagesGet,
    #[serde(rename = "messages.list")]
    MessagesList,
    #[serde(rename = "tasks.list")]
    TasksList,
    #[serde(rename = "tasks.get")]
    TasksGet,
    #[serde(rename = "tasks.getRun")]
    TasksGetRun,
    #[serde(rename = "tasks.add")]
    TasksAdd,
    #[serde(rename = "tasks.update")]
    TasksUpdate,
    #[serde(rename = "tasks.remove")]
    TasksRemove,
    #[serde(rename = "tasks.fire")]
    TasksFire,
    #[serde(rename = "tasks.cancelRun")]
    TasksCancelRun,
    #[serde(rename = "tasks.history")]
    TasksHistory,
    #[serde(rename = "tasks.runFeed")]
    TasksRunFeed,
    #[serde(rename = "tasks.purgeHistory")]
    TasksPurgeHistory,
    #[serde(rename = "schedules.list")]
    SchedulesList,
    #[serde(rename = "schedules.get")]
    SchedulesGet,
    #[serde(rename = "schedules.add")]
    SchedulesAdd,
    #[serde(rename = "schedules.update")]
    SchedulesUpdate,
    #[serde(rename = "schedules.remove")]
    SchedulesRemove,
    #[serde(rename = "schedules.preview")]
    SchedulesPreview,
    #[serde(rename = "schedules.setDefaults")]
    SchedulesSetDefaults,
    #[serde(rename = "schedules.solarPreview")]
    SchedulesSolarPreview,
    #[serde(rename = "commands.list")]
    CommandsList,
    #[serde(rename = "commands.get")]
    CommandsGet,
    #[serde(rename = "commands.add")]
    CommandsAdd,
    #[serde(rename = "commands.update")]
    CommandsUpdate,
    #[serde(rename = "commands.remove")]
    CommandsRemove,
    #[serde(rename = "events.subscribe")]
    EventsSubscribe,
    #[serde(rename = "events.unsubscribe")]
    EventsUnsubscribe,
}

impl Method {
    pub const ALL: [Self; 46] = [
        Self::SystemRestart,
        Self::ServerInfo,
        Self::ConfigGet,
        Self::ConfigUpdate,
        Self::ProtocolsList,
        Self::TransportsList,
        Self::ProtocolsInfo,
        Self::ProtocolsParams,
        Self::DiscoveryStart,
        Self::DiscoveryGet,
        Self::DiscoveryCancel,
        Self::ServicesList,
        Self::ServicesAdd,
        Self::ServicesRemove,
        Self::ServicesAlias,
        Self::ServicesStatus,
        Self::ServicesParams,
        Self::ServicesSend,
        Self::MessagesGet,
        Self::MessagesList,
        Self::TasksList,
        Self::TasksGet,
        Self::TasksGetRun,
        Self::TasksAdd,
        Self::TasksUpdate,
        Self::TasksRemove,
        Self::TasksFire,
        Self::TasksCancelRun,
        Self::TasksHistory,
        Self::TasksRunFeed,
        Self::TasksPurgeHistory,
        Self::SchedulesList,
        Self::SchedulesGet,
        Self::SchedulesAdd,
        Self::SchedulesUpdate,
        Self::SchedulesRemove,
        Self::SchedulesPreview,
        Self::SchedulesSetDefaults,
        Self::SchedulesSolarPreview,
        Self::CommandsList,
        Self::CommandsGet,
        Self::CommandsAdd,
        Self::CommandsUpdate,
        Self::CommandsRemove,
        Self::EventsSubscribe,
        Self::EventsUnsubscribe,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SystemRestart => "system.restart",
            Self::ServerInfo => "server.info",
            Self::ConfigGet => "config.get",
            Self::ConfigUpdate => "config.update",
            Self::ProtocolsList => "protocols.list",
            Self::TransportsList => "transports.list",
            Self::ProtocolsInfo => "protocols.info",
            Self::ProtocolsParams => "protocols.params",
            Self::DiscoveryStart => "discovery.start",
            Self::DiscoveryGet => "discovery.get",
            Self::DiscoveryCancel => "discovery.cancel",
            Self::ServicesList => "services.list",
            Self::ServicesAdd => "services.add",
            Self::ServicesRemove => "services.remove",
            Self::ServicesAlias => "services.alias",
            Self::ServicesStatus => "services.status",
            Self::ServicesParams => "services.params",
            Self::ServicesSend => "services.send",
            Self::MessagesGet => "messages.get",
            Self::MessagesList => "messages.list",
            Self::TasksList => "tasks.list",
            Self::TasksGet => "tasks.get",
            Self::TasksGetRun => "tasks.getRun",
            Self::TasksAdd => "tasks.add",
            Self::TasksUpdate => "tasks.update",
            Self::TasksRemove => "tasks.remove",
            Self::TasksFire => "tasks.fire",
            Self::TasksCancelRun => "tasks.cancelRun",
            Self::TasksHistory => "tasks.history",
            Self::TasksRunFeed => "tasks.runFeed",
            Self::TasksPurgeHistory => "tasks.purgeHistory",
            Self::SchedulesList => "schedules.list",
            Self::SchedulesGet => "schedules.get",
            Self::SchedulesAdd => "schedules.add",
            Self::SchedulesUpdate => "schedules.update",
            Self::SchedulesRemove => "schedules.remove",
            Self::SchedulesPreview => "schedules.preview",
            Self::SchedulesSetDefaults => "schedules.setDefaults",
            Self::SchedulesSolarPreview => "schedules.solarPreview",
            Self::CommandsList => "commands.list",
            Self::CommandsGet => "commands.get",
            Self::CommandsAdd => "commands.add",
            Self::CommandsUpdate => "commands.update",
            Self::CommandsRemove => "commands.remove",
            Self::EventsSubscribe => "events.subscribe",
            Self::EventsUnsubscribe => "events.unsubscribe",
        }
    }
}

impl fmt::Display for Method {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for Method {
    type Err = ApiError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .into_iter()
            .find(|method| method.as_str() == value)
            .ok_or_else(|| {
                ApiError::new(
                    ErrorCode::UnknownMethod,
                    format!("unknown method '{value}'"),
                )
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serializes_method_names() {
        for method in Method::ALL {
            assert_eq!(
                serde_json::to_value(method).unwrap(),
                serde_json::json!(method.as_str())
            );
            assert_eq!(method.as_str().parse::<Method>().unwrap(), method);
        }
    }

    #[test]
    fn rejects_unknown_methods() {
        let error = "unknown.method".parse::<Method>().unwrap_err();
        assert_eq!(error.code, ErrorCode::UnknownMethod);
    }
}
