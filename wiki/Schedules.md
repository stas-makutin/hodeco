# Schedules

A schedule answers one question: **when?** It is a reusable recurrence
definition in `schedules.yaml`. A task's schedule trigger points at a schedule
by ID, and the gateway computes occurrences in the server process. The browser
only displays previews and next times.

If you are new to schedules, start with the examples below. The field-by-field
rules are grouped after them, and the UI workflow is at the end.

## Three examples

**Every weekday at 09:00:**

```yaml
schedules:
  - id: weekday-morning
    name: Weekday morning
    at: "09:00"
    days: [mon, tue, wed, thu, fri]
```

**15 minutes before sunset, repeating twice:**

```yaml
schedules:
  - id: before-sunset
    name: Before sunset
    at: "sunset - 15m"
    repeatAt: [30m, 1h]
```

**Every third day at 09:00:**

```yaml
schedules:
  - id: every-third-day
    name: Every third day
    from: 2026-01-01
    at: "09:00"
    every: 3d
```

A schedule with only `from` is a one-shot at that timestamp:

```yaml
schedules:
  - id: once
    name: One time
    from: 2026-06-01T12:00:00
```

A task references the schedule by ID:

```yaml
# tasks.yaml
triggers:
  - id: sunset
    type: schedule
    schedule: before-sunset
    maxRuns: 365
```

`maxRuns` is optional and caps actual starts for that trigger. See
[Tasks](Tasks) for the trigger side and run history.

## Shared defaults

`location` and `timezone` at the top of `schedules.yaml` are defaults for every
schedule:

```yaml
location: [52.52, 13.405, 34]   # lat, lon, elevation m
timezone: Europe/Berlin
```

A schedule can override either one. The location is used for solar events, and
the timezone is used for local times and date filters.

```yaml
schedules:
  - id: before-sunset
    name: Before sunset
    timezone: Europe/Berlin
    location: [52.52, 13.405, 34]
    from: 2026-01-01
    until: 2026-12-31
    at: "sunset - 15m"
    days: [mon, tue, wed, thu, fri]
    jitter: 30s
    repeatAt: [30m, 1h]
```

## The fields

| Field | What it does |
| --- | --- |
| `id` | Immutable ASCII slug, unique in the catalog. |
| `name` | Display name. |
| `timezone` | Optional IANA timezone. Falls back to the shared default, then the host timezone. |
| `location` | Optional `[lat, lon]` or `[lat, lon, elevation_m]` for solar events. |
| `from`, `until` | Start and end of the schedule. `from` is required when you use `every`. `until` is exclusive. |
| `at` | Fixed times, hourly minute marks, or solar events. |
| `every` | One base cadence. |
| `days`, `months`, `years` | Date filters. |
| `dayMatch` | `any` or `all` when several day rules are present. |
| `allow`, `deny` | Inclusive local-time windows; `deny` wins. |
| `except` | Dates and inclusive date ranges to skip. |
| `jitter` | Deterministic ± offset applied to occurrences. |
| `repeatAt` | Extra offsets after every base occurrence. |

### `at`

`at` accepts:

- fixed times: `09:00`, `09:00:30`;
- hourly minute marks: `:15`, meaning 15 minutes past every hour;
- solar events: `sunrise`, `sunset`, `dawn`, `dusk`, `noon`, `nadir`;
- signed offsets from a solar event: `sunset - 15m`, `sunrise + 30m`.

### `every`

`every` is a single base cadence. It accepts `s`, `m`, `h`, `d`, `w`, `mo`, and
`y` units. The meaning depends on the unit:

- **Calendar cadence** (`d`, `w`, `mo`, `y`) selects dates. It can be combined
  with `at`, which selects the times on those dates. For example, `at: "09:00"`
  with `every: 3d` means every third day at 09:00.
- **Elapsed cadence** (`s`, `m`, `h`) is a continuous interval from `from`. It
  cannot be combined with `at` or date filters. Use `repeatAt` for fixed offsets
  after an At occurrence.

`every` requires `from`. A date-only `from` means local midnight.

Multiple base cadences are expressed as multiple schedules. A task may reference
several schedule triggers.

### Date filters

`days` accepts:

- weekdays (`mon`, `tue`, …);
- month days `1..31`;
- `last` for the last day of the month;
- `lw` for the last weekday (Monday–Friday);
- `Nw` for the weekday nearest day N;
- nth or last weekday (`1mon`, `-2fri`).

`months` and `years` filter dates. When several day rules are present,
`dayMatch: all` requires all of them; `any` is the default.

### Windows and exceptions

- `allow` and `deny` are inclusive windows in the effective schedule timezone.
  Windows may wrap around midnight. `deny` wins.
- `except` accepts dates and inclusive date ranges. Occurrences whose local date
  falls inside a range are skipped. The dates use the effective schedule
  timezone.

### Jitter

`jitter` is a deterministic ± offset applied to base and repeat occurrences. It
is seeded from the schedule, the base time, and the attempt index, and clamped
to `from`/`until`. It does not make every occurrence random; the same schedule
and attempt produce the same offset.

### `from` and `until`

`from` and `until` accept a date (local midnight) or a timestamp. `until` is
exclusive. A date-only `from` means local midnight in the effective timezone.

### Solar events

Solar events use NOAA-style calculations:

- `dawn` and `dusk` are civil twilight.
- Sunrise and sunset apply the observer elevation horizon-dip correction:
  `dip_deg = 0.0293 * sqrt(elevation_m)`.
- Polar events that do not exist are skipped.

### Daylight saving time

Nonexistent local times are skipped. Ambiguous local times use the first
occurrence.

## Repeats

`repeatAt` adds extra occurrences after every base occurrence, including a
from-only one-shot:

```yaml
repeatAt: [30m, 1h]
```

- It accepts one duration or a list. Omit it for no repeats.
- Each duration is measured from the **base occurrence**, not from the previous
  repeat. The example runs at the base, 30 minutes later, and 1 hour after the
  base.
- The number of extra runs equals the number of `repeatAt` entries. There is no
  count or unlimited mode.
- Offsets must be greater than zero and unique. They are applied in ascending
  order; `repeatIndex` is one-based.
- Repeats are elapsed durations. `1d` here means 24 hours, not a calendar day.
- A repeat that lands on or after the next base occurrence is skipped. The next
  base occurrence then starts a new repeat sequence.
- `until` stops repeats at its boundary.
- Allow/deny windows suppress an attempt but do not change the remaining
  offsets.

For example, `at: "sunset"` with `repeatAt: [30m, 1h]` runs at sunset, 30
minutes after sunset, and 1 hour after sunset.

## Nth and last weekdays

Negative positions count from the end of the month:

| UI position | Stored value | Example |
| --- | --- | --- |
| 1st | `1mon` | first Monday |
| 2nd | `2mon` | second Monday |
| … | … | … |
| Last | `-1mon` | last Monday |
| 2nd-last | `-2mon` | second-to-last Monday |
| 3rd-last | `-3mon` | third-to-last Monday |
| 4th-last | `-4mon` | fourth-to-last Monday |
| 5th-last | `-5mon` | fifth-to-last Monday |

`last monday` remains a textual alias for `-1mon`. If the month has fewer
occurrences than requested, the rule does not match. `-1mon` means the last
Monday specifically; `lw` means the last weekday (Monday–Friday), whichever it
is.

## Previewing a schedule

`schedules.preview` returns draft occurrences for a schedule. It returns 20 by
default and at most 100; `limit` is clamped to that range. The result includes
skipped allow/deny candidates and their reasons, and repeat occurrences include
a one-based `repeatIndex`.

The schedule list also includes:

- `usedBy` task IDs;
- the next occurrence;
- the resolved host `systemTimezone` used when no schedule/default timezone is
  set.

## Runtime behavior

- Scheduler state is in memory only. On restart or task enable, the next future
  occurrence is computed from the catalog. Missed occurrences are skipped.
- `maxRuns` on a schedule trigger counts actual starts for that trigger in
  memory. It resets on gateway restart, when the task is re-enabled, and when
  its history is purged. It does not depend on task history files, so it still
  works when task history is disabled.
- Deletion is blocked while tasks still reference the schedule.

## Using the schedule editor

The schedules page exposes a **Defaults** editor for the shared `location` and
`timezone`. Values set there apply to schedules that do not override them; use
`[latitude, longitude]` or `[latitude, longitude, elevation]` for the location.
Clearing the location is rejected while any schedule needs it for solar events.

Timezone uses **Default / Custom**; Custom shows a searchable IANA picker inline
with the radios. Location uses **None / Custom** in the Defaults dialog and
**Default / Custom** in the schedule editor. **Set location…** is disabled while
the default/none choice is selected. The Defaults dialog Save button stays
disabled until a setting changes.

**Set location…** opens one shared two-pane dialog with coordinates on the left
and a solar-event preview for a selectable date on the right. The two panes fill
the dialog width. The same dialog is used from the Defaults editor and the
schedule editor.

The schedule editor uses a `YYYY-MM-DD` date field plus an optional **Time**
checkbox with hour/minute/second controls for `from` and `until`. Date-only
values use the selected schedule timezone; a checked time is interpreted in that
timezone and converted to RFC 3339 for the API. Date fields normalize on Enter or
focus loss, and invalid drafts are reported in the fixed editor header.

Schedule jitter uses the common **Add jitter** row with a `±` number-plus-unit
editor; the editor is disabled, not hidden, when jitter is off.

The **Recurrence** section is always visible. There is no enable checkbox:
leaving At and Every empty makes the schedule a one-shot at **From**. The section
contains At rows, the Every base cadence, the Repeat block, day/month/year
filters, and jitter.

The editor exposes At entries, the Every base cadence, the Repeat block, days,
months, years, and jitter as structured controls; raw JSON is not required.
Fixed At times use hour/minute/second controls, solar offsets use a signed
amount plus unit, and the every interval has amount/unit controls.

Repeat has ordered **Repeat after** offset rows with add/remove controls. Its
help text reads:

> Each offset adds one extra run after every scheduled occurrence. Repeats that
> land on or after the next scheduled occurrence are skipped; Until also stops
> them.

The special-day builder offers `1st…5th`, `Last`, and `2nd-last…5th-last`
positions. Validation accepts `1..=5` and `-5..=-1` and rejects `0`.

The schedule list shows the rule summary, next time, usage, and text actions.
The editor is a full-page two-column form with an explicit Refresh button for
the server-side preview.

## Related pages

- [Tasks](Tasks) — how a task uses a schedule trigger and what a run does.
- [Configuration](Configuration) — where `schedules.yaml` lives and how changes
  are applied.
- [HTTP API](HTTP-API) — `schedules.*`, `schedules.preview`, and
  `schedules.solarPreview`.
