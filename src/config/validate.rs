use super::ConfigBundle;
use super::loader::ConfigError;
use crate::services::catalog;
use hodeco_contract::config::{Config, ServicesFile};
use hodeco_contract::domain::{
    ActionKind, CommandDefinition, DaySpec, MessagePayload, NextCondition, ScheduleDefinition,
    ServiceRef, TaskDefinition, TaskTrigger,
};
use hodeco_contract::utils::{Base64Bytes, Size, is_valid_id};
use std::collections::BTreeSet;
use std::str::FromStr;

const MIN_MESSAGE_LOG_SIZE: Size = Size::from_bytes(8 * 1024);
const MIN_TASK_LOG_SIZE: Size = Size::from_bytes(4 * 1024);
const MIN_TASK_HISTORY_SIZE: Size = Size::from_bytes(1024);
const MIN_PAYLOAD_SIZE: Size = Size::from_bytes(1);

/// Top-level configuration sections that only apply after a restart.
///
/// Services, tasks, schedules, and commands have their own live APIs. Comparing
/// them against the startup snapshot would produce a false restart warning
/// after any live change.
pub fn restart_required(previous: &Config, next: &Config) -> Vec<&'static str> {
    let mut fields = Vec::new();
    if previous.working_dir != next.working_dir {
        fields.push("workingDir");
    }
    if previous.log != next.log {
        fields.push("log");
    }
    if previous.http_server != next.http_server {
        fields.push("httpServer");
    }
    if previous.message_log != next.message_log {
        fields.push("messageLog");
    }
    if previous.files != next.files {
        fields.push("files");
    }
    if previous.task_log != next.task_log {
        fields.push("taskLog");
    }
    if previous.task_engine != next.task_engine {
        fields.push("taskEngine");
    }
    fields
}

pub fn validate(config: &Config) -> Result<(), ConfigError> {
    let mut errors = Vec::new();

    if config.working_dir.as_os_str().is_empty() {
        errors.push("workingDir must not be empty".to_string());
    }

    if config.log.dir.as_os_str().is_empty() {
        errors.push("log.dir must not be empty".to_string());
    }
    let log_file = config.log.file.trim();
    if log_file.is_empty() {
        errors.push("log.file must not be empty".to_string());
    } else if log_file == "."
        || log_file == ".."
        || log_file.contains(['/', '\\'])
        || std::path::Path::new(log_file).components().count() != 1
    {
        errors.push("log.file must be a file name, not a path".to_string());
    }

    if config.http_server.bind.trim().is_empty() {
        errors.push("httpServer.bind must not be empty".to_string());
    }
    if config.http_server.port == 0 {
        errors.push("httpServer.port must be greater than 0".to_string());
    }
    if config.http_server.max_connections == 0 {
        errors.push("httpServer.maxConnections must be greater than 0".to_string());
    }
    if config.http_server.read_timeout.as_std().is_zero() {
        errors.push("httpServer.readTimeout must be greater than 0".to_string());
    }
    if config.http_server.read_header_timeout.as_std().is_zero() {
        errors.push("httpServer.readHeaderTimeout must be greater than 0".to_string());
    }
    if config.http_server.write_timeout.as_std().is_zero() {
        errors.push("httpServer.writeTimeout must be greater than 0".to_string());
    }
    if config.http_server.idle_timeout.as_std().is_zero() {
        errors.push("httpServer.idleTimeout must be greater than 0".to_string());
    }
    if config.http_server.max_header_bytes == 0 {
        errors.push("httpServer.maxHeaderBytes must be greater than 0".to_string());
    }
    if config.http_server.static_assets.dir.as_os_str().is_empty() {
        errors.push("httpServer.static.dir must not be empty".to_string());
    }
    if config.http_server.static_assets.index.trim().is_empty() {
        errors.push("httpServer.static.index must not be empty".to_string());
    }

    if config.message_log.max_size < MIN_MESSAGE_LOG_SIZE {
        errors.push(format!(
            "messageLog.maxSize must be at least {}",
            MIN_MESSAGE_LOG_SIZE
        ));
    }
    if config.message_log.file.as_os_str().is_empty() {
        errors.push("messageLog.file must not be empty".to_string());
    } else if config.message_log.file.file_name().is_none() {
        errors.push("messageLog.file must include a file name".to_string());
    }
    if config.message_log.auto_persist.as_std().is_zero() {
        errors.push("messageLog.autoPersist must be greater than 0".to_string());
    }
    if config.message_log.compaction_factor < 1.0 {
        errors.push("messageLog.compactionFactor must be at least 1.0".to_string());
    }
    if config.message_log.compact_interval.as_std().is_zero() {
        errors.push("messageLog.compactInterval must be greater than 0".to_string());
    }

    validate_file_path(&config.files.services, "files.services", &mut errors);
    validate_file_path(&config.files.tasks, "files.tasks", &mut errors);
    validate_file_path(&config.files.schedules, "files.schedules", &mut errors);
    validate_file_path(&config.files.commands, "files.commands", &mut errors);

    if config.task_log.dir.as_os_str().is_empty() {
        errors.push("taskLog.dir must not be empty".to_string());
    }
    if config.task_log.max_size < MIN_TASK_LOG_SIZE {
        errors.push(format!(
            "taskLog.maxSize must be at least {}",
            MIN_TASK_LOG_SIZE
        ));
    }
    if config.task_log.max_rows == 0 {
        errors.push("taskLog.maxRows must be greater than 0".to_string());
    }
    if config.task_log.compaction_factor < 1.0 {
        errors.push("taskLog.compactionFactor must be at least 1.0".to_string());
    }
    if config.task_log.compact_interval.as_std().is_zero() {
        errors.push("taskLog.compactInterval must be greater than 0".to_string());
    }

    if config.task_engine.max_trigger_payload_size < MIN_PAYLOAD_SIZE {
        errors.push("taskEngine.maxTriggerPayloadSize must be greater than 0".to_string());
    }
    if config.task_engine.max_trigger_payload_size > Size::from_bytes(64 * 1024) {
        errors.push("taskEngine.maxTriggerPayloadSize must not exceed 64 KiB".to_string());
    }
    if config.task_engine.max_concurrent_runs == 0 {
        errors.push("taskEngine.maxConcurrentRuns must be greater than 0".to_string());
    }
    if config.task_engine.max_chain_depth == 0 {
        errors.push("taskEngine.maxChainDepth must be greater than 0".to_string());
    }
    if config.task_engine.max_fan_out == 0 {
        errors.push("taskEngine.maxFanOut must be greater than 0".to_string());
    }

    finish(errors)
}

fn validate_file_path(path: &std::path::Path, field: &str, errors: &mut Vec<String>) {
    if path.as_os_str().is_empty() {
        errors.push(format!("{field} must not be empty"));
    } else if path.file_name().is_none() {
        errors.push(format!("{field} must include a file name"));
    }
}

pub fn validate_bundle(bundle: &ConfigBundle) -> Result<(), ConfigError> {
    let mut errors = Vec::new();
    validate(&bundle.config)?;
    validate_services(&bundle.services, &mut errors);

    let schedule_ids: BTreeSet<String> = bundle
        .schedules
        .schedules
        .iter()
        .map(|schedule| schedule.id.clone())
        .collect();
    let command_ids: BTreeSet<String> = bundle
        .commands
        .commands
        .iter()
        .map(|command| command.id.clone())
        .collect();
    let service_keys = service_keys(&bundle.services);
    check_unique(
        bundle.tasks.tasks.iter().map(|task| task.id.as_str()),
        "task",
        &mut errors,
    );
    check_unique(
        bundle
            .schedules
            .schedules
            .iter()
            .map(|schedule| schedule.id.as_str()),
        "schedule",
        &mut errors,
    );
    check_unique(
        bundle
            .commands
            .commands
            .iter()
            .map(|command| command.id.as_str()),
        "command",
        &mut errors,
    );

    for (index, schedule) in bundle.schedules.schedules.iter().enumerate() {
        validate_schedule_at(schedule, &bundle.schedules, index, &mut errors);
    }
    for (index, command) in bundle.commands.commands.iter().enumerate() {
        validate_command_at(command, index, &mut errors);
    }
    for (index, task) in bundle.tasks.tasks.iter().enumerate() {
        let all_tasks: Vec<&TaskDefinition> = bundle.tasks.tasks.iter().collect();
        validate_task_at(
            task,
            &all_tasks,
            &schedule_ids,
            &command_ids,
            &service_keys,
            index,
            &mut errors,
        );
    }

    if let Some(timezone) = &bundle.schedules.timezone
        && !timezone.trim().is_empty()
        && timezone.parse::<chrono_tz::Tz>().is_err()
    {
        errors.push(format!(
            "schedules.timezone '{timezone}' is not an IANA timezone"
        ));
    }
    if let Some(location) = bundle.schedules.location
        && !location.is_valid()
    {
        errors.push("schedules.location is not valid".to_string());
    }

    finish(errors)
}

pub fn validate_services(file: &ServicesFile, errors: &mut Vec<String>) {
    let mut aliases = BTreeSet::new();
    let mut keys = BTreeSet::new();
    for (index, service) in file.services.iter().enumerate() {
        if service.protocol.trim().is_empty() {
            errors.push(format!("services[{index}].protocol must not be empty"));
        }
        if service.transport.trim().is_empty() {
            errors.push(format!("services[{index}].transport must not be empty"));
        }
        if service.entry.trim().is_empty() {
            errors.push(format!("services[{index}].entry must not be empty"));
        }
        if let Some(alias) = &service.alias {
            if alias.trim().is_empty() {
                errors.push(format!("services[{index}].alias must not be empty"));
            } else if !aliases.insert(alias.clone()) {
                errors.push(format!("duplicate service alias '{alias}'"));
            }
        }
        let key = (
            service.protocol.clone(),
            service.transport.clone(),
            service.entry.clone(),
        );
        if !keys.insert(key.clone()) {
            errors.push(format!(
                "duplicate service key '{}:{}:{}'",
                key.0, key.1, key.2
            ));
        }
        if let Err(error) =
            catalog::validate_service(&service.protocol, &service.transport, &service.params)
        {
            errors.push(format!("services[{index}]: {error}"));
        }
    }
}

pub fn validate_task_id(id: &str) -> Result<(), String> {
    if id.is_empty() {
        return Err("task id must not be empty".to_string());
    }
    if !is_valid_id(id) {
        return Err(format!("task id '{id}' must match [a-z][a-z0-9_-]{{0,63}}"));
    }
    Ok(())
}

pub fn validate_task(
    task: &TaskDefinition,
    other_tasks: &[&TaskDefinition],
    schedules: &BTreeSet<String>,
    commands: &BTreeSet<String>,
    services: &BTreeSet<ServiceRef>,
) -> Result<(), String> {
    let mut errors = Vec::new();
    validate_task_at(
        task,
        other_tasks,
        schedules,
        commands,
        services,
        0,
        &mut errors,
    );
    finish_inline(errors)
}

pub fn validate_tasks(
    tasks: &[TaskDefinition],
    schedules: &BTreeSet<String>,
    commands: &BTreeSet<String>,
    services: &BTreeSet<ServiceRef>,
) -> Result<(), String> {
    let mut errors = Vec::new();
    let mut ids = BTreeSet::new();
    for (index, task) in tasks.iter().enumerate() {
        if !ids.insert(task.id.clone()) {
            errors.push(format!("tasks[{index}]: duplicate task id '{}'", task.id));
        }
        let all_tasks: Vec<&TaskDefinition> = tasks.iter().collect();
        validate_task_at(
            task,
            &all_tasks,
            schedules,
            commands,
            services,
            index,
            &mut errors,
        );
    }
    finish_inline(errors)
}

pub fn validate_schedule(
    schedule: &ScheduleDefinition,
    defaults: &hodeco_contract::config::SchedulesFile,
) -> Result<(), String> {
    let mut errors = Vec::new();
    validate_schedule_at(schedule, defaults, 0, &mut errors);
    finish_inline(errors)
}

pub fn validate_command(command: &CommandDefinition) -> Result<(), String> {
    let mut errors = Vec::new();
    validate_command_at(command, 0, &mut errors);
    finish_inline(errors)
}

fn validate_task_at(
    task: &TaskDefinition,
    other_tasks: &[&TaskDefinition],
    schedule_ids: &BTreeSet<String>,
    command_ids: &BTreeSet<String>,
    service_keys: &BTreeSet<ServiceRef>,
    index: usize,
    errors: &mut Vec<String>,
) {
    let prefix = format!("tasks[{index}]");
    if !is_valid_id(&task.id) {
        errors.push(format!(
            "{prefix}.id '{}' must match [a-z][a-z0-9_-]{{0,63}}",
            task.id
        ));
    }
    if task.name.trim().is_empty() {
        errors.push(format!("{prefix}.name must not be empty"));
    }
    if task.run_timeout.as_std().is_zero() {
        errors.push(format!("{prefix}.runTimeout must be greater than 0"));
    }
    if task.max_runs_per_minute == 0 {
        errors.push(format!("{prefix}.maxRunsPerMinute must be greater than 0"));
    }
    if task.history.max_rows > 0 && task.history.max_size < MIN_TASK_HISTORY_SIZE {
        errors.push(format!(
            "{prefix}.history.maxSize must be at least {}",
            MIN_TASK_HISTORY_SIZE
        ));
    }

    let mut trigger_ids = BTreeSet::new();
    let mut http_triggers = 0;
    for (trigger_index, trigger) in task.triggers.iter().enumerate() {
        let trigger_prefix = format!("{prefix}.triggers[{trigger_index}]");
        if !is_valid_id(trigger.id()) {
            errors.push(format!(
                "{trigger_prefix}.id '{}' must be an ASCII slug",
                trigger.id()
            ));
        }
        if !trigger_ids.insert(trigger.id().to_string()) {
            errors.push(format!(
                "{trigger_prefix}: duplicate trigger id '{}'",
                trigger.id()
            ));
        }
        match trigger {
            TaskTrigger::Message(trigger) => {
                for service in &trigger.services {
                    validate_service_ref(service, service_keys, &trigger_prefix, errors);
                }
            }
            TaskTrigger::Schedule(trigger) => {
                if !schedule_ids.contains(&trigger.schedule) {
                    errors.push(format!(
                        "{trigger_prefix}.schedule '{}' does not exist",
                        trigger.schedule
                    ));
                }
                if trigger.max_runs == Some(0) {
                    errors.push(format!("{trigger_prefix}.maxRuns must be greater than 0"));
                }
            }
            TaskTrigger::Http(trigger) => {
                http_triggers += 1;
                if let Some(secret) = &trigger.secret
                    && secret.is_empty()
                {
                    errors.push(format!("{trigger_prefix}.secret must not be empty"));
                }
            }
            TaskTrigger::Manual(_) | TaskTrigger::Task(_) => {}
        }
    }
    if http_triggers > 1 {
        errors.push(format!("{prefix}: only one HTTP trigger is allowed"));
    }

    let mut action_ids = BTreeSet::new();
    for (action_index, action) in task.actions.iter().enumerate() {
        let action_prefix = format!("{prefix}.actions[{action_index}]");
        if !is_valid_id(&action.id) {
            errors.push(format!(
                "{action_prefix}.id '{}' must be an ASCII slug",
                action.id
            ));
        }
        if !action_ids.insert(action.id.clone()) {
            errors.push(format!(
                "{action_prefix}: duplicate action id '{}'",
                action.id
            ));
        }
        match &action.kind {
            ActionKind::Command(action) => {
                if !command_ids.contains(&action.command) {
                    errors.push(format!(
                        "{action_prefix}.command '{}' does not exist",
                        action.command
                    ));
                }
            }
            ActionKind::Message(action) => {
                validate_service_ref(
                    &action.message.service,
                    service_keys,
                    &action_prefix,
                    errors,
                );
                if let Err(error) = validate_message_payload(&action.message.payload) {
                    errors.push(format!("{action_prefix}.message.payload: {error}"));
                }
                if action.message.ttl.is_some_and(|ttl| ttl.as_std().is_zero()) {
                    errors.push(format!(
                        "{action_prefix}.message.ttl must be greater than 0"
                    ));
                }
            }
        }
    }

    let target_ids: BTreeSet<String> = other_tasks.iter().map(|task| task.id.clone()).collect();
    let mut command_result = 0;
    for (rule_index, rule) in task.next.iter().enumerate() {
        let rule_prefix = format!("{prefix}.next[{rule_index}]");
        match rule.when {
            NextCondition::CommandResult => {
                command_result += 1;
                if rule.command.as_deref().is_none_or(str::is_empty) {
                    errors.push(format!("{rule_prefix}.command must be set"));
                } else if let Some(command) = &rule.command
                    && !command_ids.contains(command)
                {
                    errors.push(format!("{rule_prefix}.command '{command}' does not exist"));
                }
                if !rule.tasks.is_empty() {
                    errors.push(format!(
                        "{rule_prefix}: command-result uses cases, not tasks"
                    ));
                }
                if rule.cases.contains_key(&255) {
                    errors.push(format!("{rule_prefix}.cases: exit code 255 is reserved"));
                }
                for (code, targets) in &rule.cases {
                    if *code == 255 {
                        continue;
                    }
                    for target in targets.iter() {
                        if !target_ids.contains(target) {
                            errors.push(format!(
                                "{rule_prefix}.cases[{code}]: task '{target}' does not exist"
                            ));
                        }
                    }
                }
            }
            _ => {
                if rule.tasks.is_empty() {
                    errors.push(format!("{rule_prefix}.tasks must not be empty"));
                }
                if rule.command.is_some() || !rule.cases.is_empty() {
                    errors.push(format!(
                        "{rule_prefix}: command and cases are only valid for command-result"
                    ));
                }
                for target in rule.tasks.iter() {
                    if !target_ids.contains(target) {
                        errors.push(format!("{rule_prefix}: task '{target}' does not exist"));
                    }
                }
            }
        }
    }
    if command_result > 1 {
        errors.push(format!("{prefix}: only one command-result rule is allowed"));
    }
}

fn check_unique<'a>(values: impl Iterator<Item = &'a str>, kind: &str, errors: &mut Vec<String>) {
    let mut seen = BTreeSet::new();
    for value in values {
        if !seen.insert(value) {
            errors.push(format!("duplicate {kind} id '{value}'"));
        }
    }
}

fn validate_service_ref(
    service: &ServiceRef,
    services: &BTreeSet<ServiceRef>,
    prefix: &str,
    errors: &mut Vec<String>,
) {
    if !services.contains(service) {
        errors.push(format!("{prefix}: service reference does not exist"));
    }
}

fn validate_message_payload(payload: &MessagePayload) -> Result<(), String> {
    match payload {
        MessagePayload::Trigger | MessagePayload::Text { .. } => Ok(()),
        MessagePayload::Hex { hex } => {
            let compact: String = hex
                .chars()
                .filter(|character| !character.is_whitespace())
                .collect();
            if !compact.len().is_multiple_of(2) {
                return Err("hex payload must contain an even number of digits".to_string());
            }
            if !compact
                .chars()
                .all(|character| character.is_ascii_hexdigit())
            {
                return Err("hex payload contains a non-hex character".to_string());
            }
            Ok(())
        }
        MessagePayload::Base64 { base64 } => Base64Bytes::from_str(base64)
            .map(|_| ())
            .map_err(|error| format!("invalid base64 payload: {error}")),
    }
}

fn validate_schedule_at(
    schedule: &ScheduleDefinition,
    defaults: &hodeco_contract::config::SchedulesFile,
    index: usize,
    errors: &mut Vec<String>,
) {
    let prefix = format!("schedules[{index}]");
    if !is_valid_id(&schedule.id) {
        errors.push(format!(
            "{prefix}.id '{}' must match [a-z][a-z0-9_-]{{0,63}}",
            schedule.id
        ));
    }
    if schedule.name.trim().is_empty() {
        errors.push(format!("{prefix}.name must not be empty"));
    }
    if let Some(timezone) = &schedule.timezone
        && timezone.parse::<chrono_tz::Tz>().is_err()
    {
        errors.push(format!(
            "{prefix}.timezone '{timezone}' is not an IANA timezone"
        ));
    }
    if let Some(location) = schedule.location
        && !location.is_valid()
    {
        errors.push(format!("{prefix}.location is not valid"));
    }
    let has_location = schedule.location.is_some() || defaults.location.is_some();

    if schedule.on.is_none() && schedule.from.is_none() {
        errors.push(format!("{prefix}: from or on is required"));
    }
    if let Some(from) = schedule.from
        && let Some(until) = schedule.until
        && from.as_date() > until.as_date()
    {
        errors.push(format!("{prefix}.until must not be before from"));
    }
    if let Some(rule) = &schedule.on {
        if rule
            .every
            .is_some_and(|every| every.duration().is_some_and(|duration| duration.is_zero()))
        {
            errors.push(format!("{prefix}.on.every must be greater than 0"));
        }
        if rule.every.is_some() && schedule.from.is_none() {
            errors.push(format!("{prefix}.on.every requires from"));
        }
        if rule.every.is_some_and(|every| every.is_sub_day()) {
            if !rule.at.is_empty() {
                errors.push(format!(
                    "{prefix}.on.at is not allowed for second, minute, or hour intervals"
                ));
            }
            if !rule.days.is_empty() || !rule.months.is_empty() || !rule.years.is_empty() {
                errors.push(format!(
                    "{prefix}: date filters are not allowed for second, minute, or hour intervals"
                ));
            }
        }
        if rule.at.is_empty() && rule.every.is_none() {
            errors.push(format!("{prefix}.on: at or every is required"));
        }
        for (at_index, at) in rule.at.iter().enumerate() {
            match at.parse() {
                Ok(hodeco_contract::domain::ParsedAt::Solar { event, .. }) => {
                    if !has_location {
                        errors.push(format!(
                            "{prefix}.on.at[{at_index}]: {event:?} requires a location"
                        ));
                    }
                }
                Ok(_) => {}
                Err(error) => errors.push(format!("{prefix}.on.at[{at_index}]: {error}")),
            }
        }
        for (day_index, day) in rule.days.iter().enumerate() {
            if let Err(error) = validate_day(day) {
                errors.push(format!("{prefix}.on.days[{day_index}]: {error}"));
            }
        }
        for year in &rule.years {
            if !(1..=9999).contains(year) {
                errors.push(format!("{prefix}.on.years: {year} is out of range"));
            }
        }
    }
    for (range_index, range) in schedule.except.iter().enumerate() {
        if range.from > range.to {
            errors.push(format!("{prefix}.except[{range_index}] is reversed"));
        }
    }
    if let Some(loop_rule) = &schedule.loop_rule {
        if loop_rule.intervals().is_empty() {
            errors.push(format!(
                "{prefix}.loop.every must contain at least one interval"
            ));
        }
        if loop_rule.repeat == Some(0) {
            errors.push(format!("{prefix}.loop.repeat must be greater than 0"));
        }
    }
    if let Some(jitter) = schedule.jitter
        && jitter.as_std() > std::time::Duration::from_secs(24 * 60 * 60)
    {
        errors.push(format!("{prefix}.jitter must not exceed 24 hours"));
    }
}

fn validate_day(day: &DaySpec) -> Result<(), String> {
    match day {
        DaySpec::Weekday(_) | DaySpec::LastDay | DaySpec::LastWeekday => Ok(()),
        DaySpec::MonthDay(day) | DaySpec::NearestWeekday(day) => {
            if (1..=31).contains(day) {
                Ok(())
            } else {
                Err(format!("month day {day} is out of range"))
            }
        }
        DaySpec::NthWeekday { nth, .. } => {
            if (1..=5).contains(&nth.abs()) {
                Ok(())
            } else {
                Err(format!("nth weekday {nth} is out of range"))
            }
        }
    }
}

fn validate_command_at(command: &CommandDefinition, index: usize, errors: &mut Vec<String>) {
    let prefix = format!("commands[{index}]");
    if !is_valid_id(&command.id) {
        errors.push(format!(
            "{prefix}.id '{}' must match [a-z][a-z0-9_-]{{0,63}}",
            command.id
        ));
    }
    if command.name.trim().is_empty() {
        errors.push(format!("{prefix}.name must not be empty"));
    }
    if command.executable.as_os_str().is_empty() {
        errors.push(format!("{prefix}.executable must not be empty"));
    }
    if command.timeout.as_std().is_zero() {
        errors.push(format!("{prefix}.timeout must be greater than 0"));
    }
    if command
        .working_dir
        .as_ref()
        .is_some_and(|path| path.as_os_str().is_empty())
    {
        errors.push(format!("{prefix}.workingDir must not be empty"));
    }
    for (arg_index, arg) in command.args.iter().enumerate() {
        if arg.contains('\0') {
            errors.push(format!("{prefix}.args[{arg_index}] contains a NUL byte"));
        }
    }
}

fn service_keys(file: &ServicesFile) -> BTreeSet<ServiceRef> {
    let mut services = BTreeSet::new();
    for service in &file.services {
        services.insert(ServiceRef::Key {
            protocol: service.protocol.clone(),
            transport: service.transport.clone(),
            entry: service.entry.clone(),
        });
        if let Some(alias) = &service.alias {
            services.insert(ServiceRef::Alias {
                alias: alias.clone(),
            });
        }
    }
    services
}

fn finish(errors: Vec<String>) -> Result<(), ConfigError> {
    if errors.is_empty() {
        Ok(())
    } else {
        Err(ConfigError::Validation(errors.join("; ")))
    }
}

fn finish_inline(errors: Vec<String>) -> Result<(), String> {
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join("; "))
    }
}

#[cfg(test)]
mod automation_tests {
    use super::*;
    use hodeco_contract::config::{CommandsFile, Config, SchedulesFile, ServicesFile, TasksFile};
    use hodeco_contract::domain::{
        ActionKind, AtSpec, CommandAction, CommandDefinition, HttpMethod, HttpTrigger,
        NextCondition, NextRule, OneOrMany, ScheduleDefinition, ScheduleEvery, ScheduleRule,
        ScheduleTrigger, TaskAction, TaskDefinition, TaskTrigger,
    };

    fn bundle() -> ConfigBundle {
        ConfigBundle {
            config: Config::default(),
            services: ServicesFile::default(),
            tasks: TasksFile::default(),
            schedules: SchedulesFile {
                timezone: Some("UTC".to_string()),
                ..SchedulesFile::default()
            },
            commands: CommandsFile {
                commands: vec![CommandDefinition {
                    id: "meter".to_string(),
                    name: "Meter".to_string(),
                    executable: "meter".into(),
                    ..CommandDefinition::default()
                }],
            },
        }
    }

    fn schedule(id: &str) -> ScheduleDefinition {
        ScheduleDefinition {
            id: id.to_string(),
            name: id.to_string(),
            on: Some(ScheduleRule {
                at: OneOrMany::One(AtSpec::Text("09:00".to_string())),
                ..ScheduleRule::default()
            }),
            ..ScheduleDefinition::default()
        }
    }

    fn task(id: &str) -> TaskDefinition {
        TaskDefinition {
            id: id.to_string(),
            name: id.to_string(),
            ..TaskDefinition::default()
        }
    }

    #[test]
    fn accepts_a_valid_automation_bundle() {
        let mut bundle = bundle();
        bundle.schedules.schedules.push(schedule("morning"));
        let mut task = task("evening");
        task.triggers.push(TaskTrigger::Schedule(ScheduleTrigger {
            id: "sunset".to_string(),
            schedule: "morning".to_string(),
            max_runs: Some(10),
        }));
        task.actions.push(TaskAction {
            id: "poll".to_string(),
            kind: ActionKind::Command(CommandAction {
                command: "meter".to_string(),
                args: vec!["--json".to_string()],
                output: Default::default(),
            }),
        });
        task.next.push(NextRule {
            when: NextCondition::AllSucceeded,
            tasks: OneOrMany::One("evening".to_string()),
            ..NextRule::default()
        });
        bundle.tasks.tasks.push(task);
        validate_bundle(&bundle).unwrap();
    }

    #[test]
    fn rejects_missing_catalog_references() {
        let mut bundle = bundle();
        let mut task = task("evening");
        task.triggers.push(TaskTrigger::Schedule(ScheduleTrigger {
            id: "sunset".to_string(),
            schedule: "missing".to_string(),
            max_runs: None,
        }));
        task.actions.push(TaskAction {
            id: "poll".to_string(),
            kind: ActionKind::Command(CommandAction {
                command: "missing".to_string(),
                args: Vec::new(),
                output: Default::default(),
            }),
        });
        task.next.push(NextRule {
            when: NextCondition::Always,
            tasks: OneOrMany::One("missing-task".to_string()),
            ..NextRule::default()
        });
        bundle.tasks.tasks.push(task);
        let error = validate_bundle(&bundle).unwrap_err().to_string();
        assert!(error.contains("schedule 'missing'"), "{error}");
        assert!(error.contains("command 'missing'"), "{error}");
        assert!(error.contains("task 'missing-task'"), "{error}");
    }

    #[test]
    fn rejects_invalid_trigger_and_next_shapes() {
        let mut bundle = bundle();
        let mut task = task("evening");
        task.triggers.push(TaskTrigger::Http(HttpTrigger {
            id: "one".to_string(),
            methods: vec![HttpMethod::Get],
            secret: None,
        }));
        task.triggers.push(TaskTrigger::Http(HttpTrigger {
            id: "two".to_string(),
            methods: vec![HttpMethod::Post],
            secret: None,
        }));
        task.next.push(NextRule {
            when: NextCondition::CommandResult,
            command: Some("meter".to_string()),
            ..NextRule::default()
        });
        task.next.push(NextRule {
            when: NextCondition::CommandResult,
            command: Some("meter".to_string()),
            ..NextRule::default()
        });
        bundle.tasks.tasks.push(task);
        let error = validate_bundle(&bundle).unwrap_err().to_string();
        assert!(error.contains("only one HTTP trigger"), "{error}");
        assert!(error.contains("only one command-result"), "{error}");
    }

    #[test]
    fn rejects_invalid_schedule_rules() {
        let mut bundle = bundle();
        let mut invalid = schedule("bad");
        invalid.name = " ".to_string();
        invalid.on = Some(ScheduleRule {
            every: Some(ScheduleEvery::Days(2)),
            ..ScheduleRule::default()
        });
        bundle.schedules.schedules.push(invalid);
        let error = validate_bundle(&bundle).unwrap_err().to_string();
        assert!(error.contains("name must not be empty"), "{error}");
        assert!(error.contains("requires from"), "{error}");
    }
}
