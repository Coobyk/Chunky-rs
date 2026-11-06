// Chunky (Chunky-rs): the Chunky chunk pregenerator, ported to Horizon as a
// native mod. This module (added to the game as `game::chunky`) holds
// everything that needs no game code: the sweep engine, the task and
// settings state, the `/chunky` command parser and the JSON persistence.
// The hook helpers (`register.rs`, `command.rs`, `console.rs`,
// `level_tick.rs`, `lifecycle.rs`, `list.rs`) bridge it to the game.

// The sweep engine is a port of Horizon's own `/pregen` engine
// (`horizon_client::pregen`): the area is cut into strips of columns, each
// strip swept north to south by a band a few rows deep, so that every chunk
// is generated once and only the rows in flight stay loaded. The mod hands
// each band to the level's streaming (`Streaming::set_band`), whose workers
// generate, light and save the chunks; the rows behind the band unload as
// any chunk the players leave.

use serde::{Deserialize, Serialize};
use std::sync::Mutex;

/// The mod's id: its row in Options > Mods and its settings keys.
pub const ID: &str = "chunky";
pub const VERSION: &str = "0.1.0";

/// How often the progress is reported, in seconds.
pub const REPORT_EVERY: f64 = 5.0;
/// How long the band may stay put before the task looks for what holds it.
pub const STALL: f64 = 30.0;
/// The widest strip, in chunks (`horizon_client::pregen::STRIP_WIDTH`).
const STRIP_WIDTH: i32 = 256;
/// How many chunks a band holds at least, in chunks.
const BAND_CHUNKS: i32 = 1024;
/// The largest radius, in blocks (`/pregen`'s world edge).
pub const MAX_RADIUS: f64 = 29_999_984.0;
/// The default radius, in blocks (Chunky's default).
pub const DEFAULT_RADIUS: f64 = 100.0;

/// The chunk areas a task sweeps: a square (`false`) or a circle (`true`)
/// of `radius` chunks around `center`, as `/chunky shape` sets it.

/// Where the band should hold streaming now: the columns `x0..=x1` of the
/// rows `z0..=z1`, and, for a circle, the centre chunk and radius the
/// circle is rounded by. The helper turns this into a
/// `horizon_client::streaming::Band`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BandSpec {
    pub x0: i32,
    pub x1: i32,
    pub z0: i32,
    pub z1: i32,
    pub circle: Option<((i32, i32), i32)>,
}

/// The sweep of one task: every chunk of the area generated once, strip by
/// strip, row by row. A port of `horizon_client::pregen::Pregen` (whose
/// `Band` this module rebuilds as a [`BandSpec`]), so the state is plain
/// data the JSON persistence can carry.
#[derive(Clone, Debug)]
pub struct Sweep {
    /// The centre chunk and the radius in chunks.
    pub center: (i32, i32),
    pub radius: i32,
    pub circle: bool,
    /// The strips' columns, west to east.
    strips: Vec<(i32, i32)>,
    strip: usize,
    /// The band's first row: every row of the strip before it is done.
    row: i32,
    /// Chunks of the area lit so far, and in all.
    pub done: u64,
    pub total: u64,
}

impl Sweep {
    pub fn new(center: (i32, i32), radius: i32, circle: bool) -> Self {
        let radius = radius.max(0);
        let (x0, width) = (center.0 - radius, 2 * radius + 1);
        let count = (width + STRIP_WIDTH - 1) / STRIP_WIDTH;
        // The columns `i * width / count` on, in i64: the product overflows
        // for a radius of a few hundred thousand chunks.
        let edge = |i: i32| x0 + (i64::from(width) * i64::from(i) / i64::from(count)) as i32;
        let strips = (0..count).map(|i| (edge(i), edge(i + 1) - 1)).collect();
        let mut sweep = Self {
            center,
            radius,
            circle,
            strips,
            strip: 0,
            row: 0,
            done: 0,
            total: 0,
        };
        sweep.total = (center.1 - radius..=center.1 + radius)
            .map(|z| sweep.row_chunks(x0, x0 + width - 1, z))
            .sum();
        sweep.row = sweep.rows().map_or(0, |r| r.0);
        sweep
    }

    /// A sweep saved mid-run: the strips follow from the centre and radius.
    pub fn restore(
        center: (i32, i32),
        radius: i32,
        circle: bool,
        strip: usize,
        row: i32,
        done: u64,
        total: u64,
    ) -> Self {
        let mut sweep = Self::new(center, radius, circle);
        // Past the last strip the sweep was finished; band() and rows()
        // answer none for it.
        sweep.strip = strip;
        sweep.row = row;
        sweep.done = done;
        sweep.total = if total == 0 { sweep.total } else { total };
        sweep
    }

    /// How far from the centre column the area reaches at `d` chunks from
    /// the centre row (or, the same, from the centre column), if it does.
    fn half_width(&self, d: i32) -> Option<i32> {
        let (r, d) = (i64::from(self.radius), i64::from(d));
        if !self.circle {
            return (d.abs() <= r).then_some(self.radius);
        }
        (d * d <= r * r).then(|| (r * r - d * d).isqrt() as i32)
    }

    fn contains(&self, x: i32, z: i32) -> bool {
        self.half_width(z - self.center.1)
            .is_some_and(|h| (x - self.center.0).abs() <= h)
    }

    /// The area's chunks in columns `x0..=x1` of row `z`.
    fn row_chunks(&self, x0: i32, x1: i32, z: i32) -> u64 {
        self.half_width(z - self.center.1).map_or(0, |h| {
            let (lo, hi) = (x0.max(self.center.0 - h), x1.min(self.center.0 + h));
            (i64::from(hi) - i64::from(lo) + 1).max(0) as u64
        })
    }

    /// The first and last rows of the current strip that hold chunks of the
    /// area.
    fn rows(&self) -> Option<(i32, i32)> {
        let &(x0, x1) = self.strips.get(self.strip)?;
        let nearest = (x0 - self.center.0).max(self.center.0 - x1).max(0);
        let h = self.half_width(nearest)?;
        Some((self.center.1 - h, self.center.1 + h))
    }

    /// The band streaming should hold now; none once every strip is done.
    pub fn band(&self) -> Option<BandSpec> {
        let &(x0, x1) = self.strips.get(self.strip)?;
        let (_, last) = self.rows()?;
        let rows = (BAND_CHUNKS + (x1 - x0)) / (x1 - x0 + 1);
        Some(BandSpec {
            x0,
            x1,
            z0: self.row,
            z1: (self.row + rows - 1).min(last),
            circle: self.circle.then_some((self.center, self.radius)),
        })
    }

    /// Move past the band's first rows once all their chunks are `lit`.
    /// Returns whether the band moved.
    pub fn advance(&mut self, lit: impl Fn(i32, i32) -> bool) -> bool {
        let mut moved = false;
        while let (Some(&(x0, x1)), Some((_, last))) = (self.strips.get(self.strip), self.rows()) {
            let z = self.row;
            if !(x0..=x1).all(|x| !self.contains(x, z) || lit(x, z)) {
                break;
            }
            self.done += self.row_chunks(x0, x1, z);
            self.row += 1;
            if self.row > last {
                self.strip += 1;
                self.row = self.rows().map_or(self.row, |r| r.0);
            }
            moved = true;
        }
        moved
    }

    /// The chunks of the band's first row, which it waits for.
    pub fn first_row(&self) -> Vec<(i32, i32)> {
        let Some(&(x0, x1)) = self.strips.get(self.strip) else {
            return Vec::new();
        };
        (x0..=x1)
            .filter(|&x| self.contains(x, self.row))
            .map(|x| (x, self.row))
            .collect()
    }

    pub fn finished(&self) -> bool {
        self.strip >= self.strips.len()
    }
}

/// A pregeneration task: where it sweeps, and when.
#[derive(Clone, Debug)]
pub struct Task {
    pub sweep: Sweep,
    /// The task's dimension (`minecraft:overworld`), as the hook reports it.
    pub dim: String,
    pub paused: bool,
    /// The player who started it, told the progress too.
    pub owner: Option<u128>,
    /// Time spent running, up to the last pause, in seconds.
    pub ran: f64,
    /// When it last resumed, as seconds since the Unix epoch; none while
    /// paused.
    pub resumed: Option<f64>,
    /// When the progress was last reported.
    pub reported: Option<f64>,
    /// When the band last moved, to notice a task that stopped advancing.
    pub moved: Option<f64>,
    /// What was done at the last report, so the rate is the current one
    /// rather than the run's average.
    pub reported_done: u64,
}

impl Task {
    pub fn new(sweep: Sweep, dim: String, owner: Option<u128>) -> Self {
        let now = now_secs();
        Self {
            sweep,
            dim,
            paused: false,
            owner,
            ran: 0.0,
            resumed: Some(now),
            reported: Some(now),
            moved: Some(now),
            reported_done: 0,
        }
    }

    pub fn pause(&mut self, now: f64) {
        if self.paused {
            return;
        }
        self.paused = true;
        self.ran += (now - self.resumed.unwrap_or(now)).max(0.0);
        self.resumed = None;
    }

    pub fn resume(&mut self, now: f64) {
        if !self.paused {
            return;
        }
        self.paused = false;
        self.resumed = Some(now);
    }
}

/// What the next task takes its area from (`/chunky center`, `/chunky
/// radius`, `/chunky shape`), and whether the progress is reported.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Settings {
    /// The centre in blocks, as `/chunky center` set it.
    pub center: Option<(i32, i32)>,
    /// The radius in blocks, as `/chunky radius` set it.
    pub radius: f64,
    pub circle: bool,
    /// No periodic reports; `/chunky status` still answers.
    pub quiet: bool,
}

impl Settings {
    pub const fn new() -> Self {
        Self {
            center: None,
            radius: DEFAULT_RADIUS,
            circle: false,
            quiet: false,
        }
    }
}

pub static TASK: Mutex<Option<Task>> = Mutex::new(None);
pub static SETTINGS: Mutex<Settings> = Mutex::new(Settings::new());

/// Seconds since the Unix epoch, the times the persistence carries.
pub fn now_secs() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}

/// `1h 02m 03s`.
pub fn duration(seconds: f64) -> String {
    let s = seconds.max(0.0).round() as u64;
    match (s / 3600, s / 60 % 60, s % 60) {
        (0, 0, s) => format!("{s}s"),
        (0, m, s) => format!("{m}m {s:02}s"),
        (h, m, s) => format!("{h}h {m:02}m {s:02}s"),
    }
}

/// The task's progress line: Chunky's, in Horizon's `/pregen` wording. The
/// rate is the current one: what was done since the last report.
pub fn progress_line(task: &Task) -> String {
    let p = &task.sweep;
    let now = now_secs();
    let window = now - task.reported.unwrap_or(now);
    // A stale report (the world was closed) says nothing about the rate.
    let rate = if window <= 30.0 {
        (p.done - task.reported_done) as f64 / window.max(1e-3)
    } else {
        0.0
    };
    let left = p.total.saturating_sub(p.done);
    let eta = if rate > 0.0 && left > 0 {
        duration(left as f64 / rate)
    } else {
        "unknown".into()
    };
    format!(
        "{} chunks ({}%), {rate:.1} chunks/s, {} left",
        p.done,
        (100.0 * p.done as f64 / p.total.max(1) as f64).floor() as u64,
        if p.done >= p.total {
            "nothing".to_owned()
        } else {
            eta
        },
    )
}

/// What a `/chunky` line asks for. The numbers accept what Chunky's do
/// (`100`, `-50.5`); the words, its commands.
#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    /// No arguments: the command list.
    Help,
    Start,
    Pause,
    Continue,
    Cancel,
    /// Progress and settings (`/chunky status`, `/chunky selection`).
    Status,
    Quiet,
    /// The centre from the world spawn.
    Spawn,
    Center(f64, f64),
    Radius(f64),
    /// `square` or `circle`, as Chunky's shapes (and patterns) go.
    Shape(String),
    Unknown(String),
}

pub fn parse(line: &str) -> Action {
    let mut words = line.split_whitespace();
    if words.next() != Some("chunky") {
        return Action::Unknown(line.into());
    }
    match words.next() {
        None => Action::Help,
        Some("start") => Action::Start,
        Some("pause") => Action::Pause,
        Some("continue") | Some("resume") => Action::Continue,
        Some("cancel") | Some("stop") => Action::Cancel,
        Some("status") | Some("selection") => Action::Status,
        Some("quiet") => Action::Quiet,
        Some("spawn") => Action::Spawn,
        Some("center") | Some("centre") => match (words.next(), words.next()) {
            (Some(x), Some(z)) => match (x.parse::<f64>(), z.parse::<f64>()) {
                (Ok(x), Ok(z)) => Action::Center(x, z),
                _ => Action::Unknown(line.into()),
            },
            _ => Action::Unknown(line.into()),
        },
        Some("radius") => match words.next().and_then(|r| r.parse::<f64>().ok()) {
            Some(r) => Action::Radius(r),
            None => Action::Unknown(line.into()),
        },
        Some("shape") => match words.next() {
            Some(s) => Action::Shape(s.to_owned()),
            None => Action::Unknown(line.into()),
        },
        Some(other) => Action::Unknown(other.to_owned()),
    }
}

/// The command list, as `/chunky` answers it.
pub const HELP: &str = "Chunky: /chunky start, /chunky pause, /chunky continue, /chunky cancel, \
     /chunky status, /chunky center <x> <z>, /chunky radius <blocks>, /chunky shape <square|circle>, \
     /chunky spawn, /chunky quiet";

// ---- Persistence ----------------------------------------------------------

#[derive(Serialize, Deserialize, Default)]
struct SettingsDto {
    center: Option<[i32; 2]>,
    radius: Option<f64>,
    circle: Option<bool>,
    quiet: Option<bool>,
}

#[derive(Serialize, Deserialize)]
struct TaskDto {
    dim: String,
    center: [i32; 2],
    radius: i32,
    circle: bool,
    strip: usize,
    row: i32,
    done: u64,
    total: u64,
    paused: bool,
    owner: Option<String>,
    ran: f64,
    resumed: Option<f64>,
    reported: Option<f64>,
    reported_done: Option<u64>,
    moved: Option<f64>,
}

/// What `chunky.json` in the world's folder holds: the settings and the
/// task, so a task continues after the world opens again (Chunky's
/// `continue on restart`).
#[derive(Serialize, Deserialize, Default)]
pub struct Stored {
    settings: SettingsDto,
    task: Option<TaskDto>,
}

fn owner_to_string(owner: Option<u128>) -> Option<String> {
    owner.map(|o| format!("{o:#x}"))
}

fn owner_from_string(owner: &Option<String>) -> Option<u128> {
    owner
        .as_deref()
        .and_then(|o| u128::from_str_radix(o.trim_start_matches("0x"), 16).ok())
}

impl Stored {
    pub fn current() -> Self {
        let settings = SETTINGS.lock().unwrap();
        let task = TASK.lock().unwrap();
        Self {
            settings: SettingsDto {
                center: settings.center.map(|(x, z)| [x, z]),
                radius: Some(settings.radius),
                circle: Some(settings.circle),
                quiet: Some(settings.quiet),
            },
            task: task.as_ref().map(|t| TaskDto {
                dim: t.dim.clone(),
                center: [t.sweep.center.0, t.sweep.center.1],
                radius: t.sweep.radius,
                circle: t.sweep.circle,
                strip: t.sweep.strip,
                row: t.sweep.row,
                done: t.sweep.done,
                total: t.sweep.total,
                paused: t.paused,
                owner: owner_to_string(t.owner),
                ran: t.ran,
                resumed: t.resumed,
                reported: t.reported,
                reported_done: Some(t.reported_done),
                moved: t.moved,
            }),
        }
    }

    /// Restore what was saved: the settings, and the task when it has work
    /// left. Answers the task to resume, if any.
    pub fn apply(self) -> Option<Task> {
        let Self { settings, task } = self;
        let mut settings_lock = SETTINGS.lock().unwrap();
        if let Some(center) = settings.center {
            settings_lock.center = Some((center[0], center[1]));
        }
        if let Some(radius) = settings.radius {
            settings_lock.radius = radius.clamp(0.0, MAX_RADIUS);
        }
        if let Some(circle) = settings.circle {
            settings_lock.circle = circle;
        }
        if let Some(quiet) = settings.quiet {
            settings_lock.quiet = quiet;
        }
        drop(settings_lock);
        task.map(|t| {
            Task {
                sweep: Sweep::restore(
                    (t.center[0], t.center[1]),
                    t.radius,
                    t.circle,
                    t.strip,
                    t.row,
                    t.done,
                    t.total,
                ),
                dim: t.dim,
                paused: t.paused,
                owner: owner_from_string(&t.owner),
                ran: t.ran,
                resumed: t.resumed,
                reported: t.reported,
                reported_done: t.reported_done.unwrap_or(0),
                moved: t.moved,
            }
        })
        .filter(|t| !t.sweep.finished() && t.sweep.total > 0)
    }
}

/// The world's folder the mod keeps `chunky/chunky.json` in, as the helper
/// reads it off the game (`native_world_dir`).
pub fn state_path(world_dir: &std::path::Path) -> std::path::PathBuf {
    world_dir.join("chunky").join("chunky.json")
}

/// Save the state now. False when there is no world folder, or the file
/// could not be written.
pub fn save(world_dir: &std::path::Path) -> bool {
    let path = state_path(world_dir);
    let Some(parent) = path.parent() else {
        return false;
    };
    if std::fs::create_dir_all(parent).is_err() {
        return false;
    }
    let Ok(bytes) = serde_json::to_vec_pretty(&Stored::current()) else {
        return false;
    };
    // Atomic, as Horizon's own server mods write: a temporary file flushed,
    // then renamed over the old one.
    let temp = parent.join(".chunky.json.tmp");
    let saved = (|| {
        use std::io::Write;
        let mut file = std::fs::File::create(&temp).ok()?;
        file.write_all(&bytes).ok()?;
        file.sync_all().ok()?;
        std::fs::rename(&temp, &path).ok()
    })();
    saved.is_some()
}

/// Load the state back (the settings, and a task to resume). None when
/// there is nothing to resume; the settings apply either way.
pub fn load(world_dir: &std::path::Path) -> Option<Task> {
    match std::fs::read(state_path(world_dir)) {
        Ok(bytes) => serde_json::from_slice::<Stored>(&bytes)
            .ok()
            .and_then(Stored::apply),
        Err(_) => {
            // No file (a first run): the defaults. A damaged one stays as it
            // is, so it is never replaced by an empty state.
            Stored::default().apply();
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sweeps_every_chunk_of_the_area_once() {
        for circle in [false, true] {
            for (center, radius) in [((0, 0), 0), ((3, -7), 5), ((-40, 12), 130), ((0, 0), 300)] {
                let mut sweep = Sweep::new(center, radius, circle);
                let r = i64::from(radius);
                let area: u64 = (-radius..=radius)
                    .flat_map(|dz| (-radius..=radius).map(move |dx| (dx, dz)))
                    .filter(|&(dx, dz)| !circle || i64::from(dx).pow(2) + i64::from(dz).pow(2) <= r * r)
                    .count() as u64;
                assert_eq!(sweep.total, area, "{center:?} {radius}");
                // Every chunk lit from the start: the sweep finishes at once.
                sweep.advance(|_, _| true);
                assert!(sweep.finished());
                assert_eq!(sweep.done, area);
                // No chunk twice: the band walks strictly forward.
                let mut sweep = Sweep::new(center, radius, circle);
                let mut seen = std::collections::HashSet::new();
                while !sweep.finished() {
                    let band = sweep.band().unwrap();
                    for z in band.z0..=band.z1 {
                        for x in band.x0..=band.x1 {
                            if sweep.contains(x, z) {
                                assert!(seen.insert((x, z)));
                            }
                        }
                    }
                    assert!(sweep.advance(|_, _| true));
                }
            }
        }
    }

    #[test]
    fn waits_for_unlit_rows() {
        let mut sweep = Sweep::new((0, 0), 10, false);
        // Every row above the centre lit: they alone are done, and the band
        // waits at the centre row.
        sweep.advance(|_, z| z < 0);
        assert_eq!(sweep.done, 210);
        assert_eq!(sweep.band().map(|b| b.z0), Some(0));
        // Nothing moved: a stall the driver reports.
        assert!(!sweep.advance(|_, _| false));
    }

    #[test]
    fn bands_are_bounded() {
        let sweep = Sweep::new((0, 0), 2000, false);
        let band = sweep.band().unwrap();
        assert!(band.z1 - band.z0 <= BAND_CHUNKS);
        assert!(sweep.first_row().len() <= 2001);
    }

    #[test]
    fn restore_picks_up_where_a_sweep_left_off() {
        let mut sweep = Sweep::new((3, -7), 20, false);
        sweep.advance(|x, z| x >= 0 && z < 5);
        let restored = Sweep::restore(
            sweep.center,
            sweep.radius,
            sweep.circle,
            sweep.strip,
            sweep.row,
            sweep.done,
            sweep.total,
        );
        assert_eq!(restored.band(), sweep.band());
        assert_eq!(restored.total, sweep.total);
    }

    #[test]
    fn times_pause_and_resume() {
        let mut task = Task::new(Sweep::new((0, 0), 1, false), "minecraft:overworld".into(), None);
        let now = 1000.0;
        task.ran = 0.0;
        task.resumed = Some(now);
        task.pause(now + 10.0);
        assert!((task.ran - 10.0).abs() < 1e-6);
        task.resume(now + 20.0);
        assert!((task.ran - 10.0).abs() < 1e-6);
    }

    #[test]
    fn parses_the_command_surface() {
        assert_eq!(parse("chunky"), Action::Help);
        assert_eq!(parse("chunky start"), Action::Start);
        assert_eq!(parse("chunky continue"), Action::Continue);
        assert_eq!(parse("chunky resume"), Action::Continue);
        assert_eq!(parse("chunky stop"), Action::Cancel);
        assert_eq!(parse("chunky selection"), Action::Status);
        assert_eq!(parse("chunky center 100 -50"), Action::Center(100.0, -50.0));
        assert_eq!(parse("chunky center 10.5 3"), Action::Center(10.5, 3.0));
        assert_eq!(parse("chunky radius 1000"), Action::Radius(1000.0));
        assert_eq!(parse("chunky shape circle"), Action::Shape("circle".into()));
        assert_eq!(parse("chunky shape"), Action::Unknown("chunky shape".into()));
        assert_eq!(parse("chunky bogus"), Action::Unknown("bogus".into()));
        assert_eq!(parse("help"), Action::Unknown("help".into()));
    }

    #[test]
    fn json_round_trips_the_state() {
        let sweep = Sweep::restore((10, -3), 40, true, 0, 7, 123, 4567);
        let task = Task {
            sweep,
            dim: "minecraft:the_nether".into(),
            paused: true,
            owner: Some(u128::from_be_bytes([1; 16])),
            ran: 12.5,
            resumed: None,
            reported: Some(1.0),
            moved: Some(2.0),
            reported_done: 100,
        };
        let stored = Stored {
            settings: SettingsDto {
                center: Some([5, -6]),
                radius: Some(500.0),
                circle: Some(true),
                quiet: Some(true),
            },
            task: Some(TaskDto {
                dim: task.dim.clone(),
                center: [task.sweep.center.0, task.sweep.center.1],
                radius: task.sweep.radius,
                circle: task.sweep.circle,
                strip: task.sweep.strip,
                row: task.sweep.row,
                done: task.sweep.done,
                total: task.sweep.total,
                paused: task.paused,
                owner: owner_to_string(task.owner),
                ran: task.ran,
                resumed: task.resumed,
                reported: task.reported,
                reported_done: Some(task.reported_done),
                moved: task.moved,
            }),
        };
        let bytes = serde_json::to_vec(&stored).unwrap();
        let back: Stored = serde_json::from_slice(&bytes).unwrap();
        let resumed_task = back.apply().unwrap();
        assert_eq!(resumed_task.dim, task.dim);
        assert_eq!(resumed_task.sweep.center, task.sweep.center);
        assert_eq!(resumed_task.sweep.band(), task.sweep.band());
        assert_eq!(resumed_task.sweep.done, 123);
        assert_eq!(resumed_task.owner, task.owner);
        assert_eq!(resumed_task.reported_done, 100);
        assert!(resumed_task.paused);
        assert_eq!(SETTINGS.lock().unwrap().radius, 500.0);
        assert!(SETTINGS.lock().unwrap().quiet);
        assert_eq!(SETTINGS.lock().unwrap().center, Some((5, -6)));
        // Clean up for the other tests.
        *SETTINGS.lock().unwrap() = Settings::new();
    }

    #[test]
    fn a_finished_task_does_not_resume() {
        let sweep = Sweep::restore((0, 0), 1, false, 1, 0, 9, 9);
        assert!(sweep.finished());
        let stored = Stored {
            settings: SettingsDto::default(),
            task: Some(TaskDto {
                dim: "minecraft:overworld".into(),
                center: [0, 0],
                radius: 1,
                circle: false,
                strip: sweep.strip,
                row: sweep.row,
                done: sweep.done,
                total: sweep.total,
                paused: false,
                owner: None,
                ran: 1.0,
                resumed: None,
                reported: None,
                reported_done: None,
                moved: None,
            }),
        };
        assert!(stored.apply().is_none());
    }

    #[test]
    fn formats_durations_and_progress() {
        assert_eq!(duration(5.0), "5s");
        assert_eq!(duration(65.0), "1m 05s");
        assert_eq!(duration(3723.0), "1h 02m 03s");
        let mut task = Task::new(Sweep::new((0, 0), 10, false), "minecraft:overworld".into(), None);
        task.sweep.done = 50;
        let line = progress_line(&task);
        assert!(line.contains("50 chunks (11%)"), "{line}");
        assert!(line.contains("left"), "{line}");
    }
}
