# Schedules

Schedules are reusable recurrence definitions referenced by task schedule
triggers. They live in `schedules.yaml`. A schedule computes occurrences in the
server process; the browser only displays previews and next times.

```yaml
location: [52.52, 13.405, 34]   # optional default: lat, lon, elevation m
timezone: Europe/Berlin         # optional default: system timezone

schedules:
  - id: before-sunset
    name: Before sunset
    timezone: Europe/Berlin
    location: [52.52, 13.405, 34]
    from: 2026-01-01
    until: 2026-12-31
    on:
      at: "sunset - 15m"
      days: [mon, tue, wed, thu, fri]
      allow: "16:00~22:00"
    loop:
      every: 30m
      repeat: 2
```

## Rules

- `on.at` accepts fixed times (`09:00`, `09:00:30`), hourly minute marks
  (`:15`), and solar events (`sunrise`, `sunset`, `dawn`, `dusk`, `noon`,
  `nadir`) with signed offsets (`sunset - 15m`).
- `on.every` accepts `s`, `m`, `h`, `d`, `w`, `mo`, and `y` units. Sub-day
  intervals are continuous elapsed time and do not allow `at` or date filters.
  Day/week and month/year intervals are anchored at the local `from` date;
  missing calendar dates (for example the 31st in a short month) are skipped.
- `on.days` accepts weekdays, month days `1..31`, `last`, `lw` (last weekday),
  `Nw` (nearest weekday to day N), and nth/last weekday (`1mon`, `-1fri`).
  `dayMatch` is `any` or `all`.
- `months` and `years` filter dates.
- `allow`/`deny` are inclusive local-time windows; wrap-around is supported.
  `deny` wins.
- `except` accepts dates and inclusive date ranges.
- `jitter` is a deterministic ± offset. It is seeded from the schedule, base
  time, and attempt index, and clamped to `from`/`until`/now.
- `from`/`until` accept a date (local midnight) or timestamp. `until` is
  exclusive. `from` alone is a one-shot at that timestamp.
- Solar events use NOAA-style calculations. `dawn`/`dusk` are civil twilight.
  Sunrise and sunset apply the observer elevation horizon-dip correction:
  `dip_deg = 0.0293 * sqrt(elevation_m)`. Polar events that do not exist are
  skipped.
- DST: nonexistent local times are skipped; ambiguous local times use the first
  occurrence.

## Loops

`loop` is a sub-schedule spawned after each base occurrence. `every` is one
interval or an ordered backoff sequence; `repeat` is the number of additional
attempts. Without `repeat`, the loop continues until `until` or the next base
occurrence. Allow/deny windows suppress an attempt but do not stop the loop. The
next base occurrence replaces the active loop.

## Runtime

- Scheduler state is in memory only. On restart or task enable, the next future
  occurrence is computed from the catalog. Missed occurrences are skipped.
- `maxRuns` on a schedule trigger counts actual starts for that trigger in
  memory. It resets on gateway restart, when the task is re-enabled, and when
  its history is purged. It does not depend on TLOG history, so it still works
  when task history is disabled.
- `schedules.preview` returns up to 100 occurrences from a draft schedule,
  including skipped allow/deny candidates and their reasons.
- The schedule list includes `usedBy` task IDs and the next occurrence.

## UI

Schedules has a list with rule summary, next time, usage, and text actions. The
schedule editor is a full-page two-column form with an explicit Refresh button
for the server-side preview. Deletion is blocked while tasks still reference the
schedule.
